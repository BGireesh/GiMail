//! Calendar notifications. When an event keeps its calendar's default reminders, its reminder
//! comes `notifications/calendarEvents/leadTimeInSeconds` before it starts (two minutes unless
//! set). Otherwise it comes at each of the event's own pop-up reminders. Every 30 s the due ones
//! show, never more than five minutes into the event, and events are fetched again every 30
//! minutes. Events without a title, declined, cancelled, all day, or on a calendar whose
//! notifications are off never notify.
//!
//! A notification has the event's title, its call's provider (or else its location), and when it
//! is (`in 2 mins · 10:00 – 10:30AM`). With a call, it has a `Join meeting` button. A click opens
//! the week on the event's day with its details, and the button opens the call too.

use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};

use exo_store::{
    CalendarEvent, ConferenceKind, EventReminders, EventStatus, EventTime, RsvpStatus, Settings,
    Store, StoredEvent,
};
use jiff::{SignedDuration, Timestamp, ToSpan, tz::TimeZone};
use tokio::sync::watch;

use super::{Click, Notice, Notifier, Open};
use crate::{
    calendar::event_key,
    format::dates::{calendar_range_compact, relative_compact},
};

/// How often due notifications are looked for.
const POLL: Duration = Duration::from_secs(30);
/// How often the events are fetched again.
const REFETCH: SignedDuration = SignedDuration::from_mins(30);
/// How late a notification may still show: five minutes past the start.
const LATE: SignedDuration = SignedDuration::from_mins(5);
/// The nudge a notification within the minute gets, so the 30 s poll doesn't make it read a
/// minute short.
const NUDGE: SignedDuration = SignedDuration::from_secs(30);

/// One time an event notifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder<'a> {
    pub event: &'a StoredEvent,
    /// When it shows.
    pub at: Timestamp,
    pub start: Timestamp,
    pub end: Timestamp,
}

/// The calendars whose events notify on this device. These are the calendars of the signed-in
/// calendar accounts that sync and have notifications on.
pub fn calendars(settings: &Settings) -> Vec<String> {
    settings.calendars_where(|calendar| calendar.sync && calendar.notifies())
}

/// When `stored` notifies, with `lead` the setting's lead time and `me` the account. It never does
/// for an event without an id, a time or a title, one I declined, a cancelled one, or one all day.
pub fn reminders<'a>(stored: &'a StoredEvent, lead: SignedDuration, me: &str) -> Vec<Reminder<'a>> {
    let event = &stored.event;
    let (Some(EventTime::Timed { at: start, .. }), Some(EventTime::Timed { at: end, .. })) =
        (&event.start, &event.end)
    else {
        return Vec::new();
    };
    let (start, end) = (*start, *end);
    let titled = event
        .summary
        .as_deref()
        .is_some_and(|summary| !summary.is_empty());
    if event.id.is_none()
        || !titled
        || event.status == EventStatus::Cancelled
        || event.my_rsvp(me) == Some(RsvpStatus::Declined)
    {
        return Vec::new();
    }
    let reminder = |at| Reminder {
        event: stored,
        at,
        start,
        end,
    };
    match &event.reminders {
        EventReminders::Default => vec![reminder(start - lead)],
        EventReminders::None => Vec::new(),
        EventReminders::Custom(minutes) => minutes
            .iter()
            .map(|&mins| reminder(start - SignedDuration::from_mins(i64::from(mins))))
            .collect(),
    }
}

/// Whether `reminder` shows at `now`, which it does when it's due and the event started at most
/// five minutes ago.
pub fn due(reminder: &Reminder<'_>, now: Timestamp) -> bool {
    reminder.at < now && now <= reminder.start + LATE
}

/// The plain email address of the calendar account owning `calendar_id`, falling back to
/// `fallback`. Prefers signed-in accounts when looking up a calendar ID.
fn calendar_owner<'a>(settings: &'a Settings, calendar_id: &str, fallback: &'a str) -> &'a str {
    settings
        .calendar_accounts
        .iter()
        .find(|(_, account)| account.authed && account.calendars.contains_key(calendar_id))
        .or_else(|| {
            settings
                .calendar_accounts
                .iter()
                .find(|(_, account)| account.calendars.contains_key(calendar_id))
        })
        .map_or(fallback, |(key, account)| account.email_for(key))
}

/// The notification for `reminder` at `now` in `account`, with its times in `tz`.
pub fn notice(reminder: &Reminder<'_>, account: &str, now: Timestamp, tz: &TimeZone) -> Notice {
    notice_for(reminder, account, account, now, tz)
}

/// [`notice`] for an event on a calendar owned by `owner` (which may differ from the primary mail
/// `account` on cross-provider/secondary calendar accounts).
pub fn notice_for(
    reminder: &Reminder<'_>,
    account: &str,
    owner: &str,
    now: Timestamp,
    tz: &TimeZone,
) -> Notice {
    let event = &reminder.event.event;
    // A start minutes or seconds away reads 30 s later, unless the notification is late.
    let relative = relative_compact(reminder.start, now);
    let soon = relative.contains("min") || relative == "now";
    let start = if now <= reminder.start && now - NUDGE <= reminder.at && soon {
        reminder.start + NUDGE
    } else {
        reminder.start
    };
    let (provider, link) = conference(event, owner);
    let key = event_key(reminder.event, event, &reminder.start.to_zoned(tz.clone()));
    let (day, end_hour) = (
        reminder.start.to_zoned(tz.clone()).date(),
        reminder.end.to_zoned(tz.clone()).hour(),
    );
    let open = |url| Click {
        account: account.to_owned(),
        open: Open::Event {
            key: key.clone(),
            day,
            end_hour,
            url,
        },
    };
    Notice {
        key: format!("CALENDAR-{}", event.id.as_deref().unwrap_or_default()),
        title: event.summary.clone().unwrap_or_default(),
        subtitle: provider.map(str::to_owned).or_else(|| {
            event
                .location
                .clone()
                .filter(|location| !location.is_empty())
        }),
        body: format!(
            "{} · {}",
            relative_compact(start, now),
            calendar_range_compact(start, reminder.end, tz)
        ),
        // The app's icon, as Exo's own reminder.
        image: None,
        button: provider.map(|_| open(link)),
        click: open(None),
    }
}

/// The call's provider as the notification names it, and its link. A Meet link opens as this
/// account; Teams, Zoom, and other links open directly.
fn conference(event: &CalendarEvent, me: &str) -> (Option<&'static str>, Option<String>) {
    let Some(conf) = event.conference.as_ref().filter(|c| !c.pending) else {
        return (None, None);
    };
    if let Some(url) = &conf.join_url {
        return match &conf.kind {
            ConferenceKind::Teams => (Some("Microsoft Teams"), Some(url.clone())),
            ConferenceKind::Zoom => (Some("Zoom"), Some(url.clone())),
            ConferenceKind::GoogleMeet => {
                let link = url::Url::parse(url).ok().map(|mut link| {
                    link.query_pairs_mut().append_pair("authuser", me);
                    link.to_string()
                });
                (Some("Google Meet"), link)
            }
            ConferenceKind::Other(_) => (Some("Video"), Some(url.clone())),
        };
    }
    if let Some(dial) = &conf.dial_in {
        return (Some("Phone"), Some(dial.clone()));
    }
    (None, None)
}

/// Posts calendar notifications from `store` for `account_key` (whose display address is `email`)
/// while `notifier` has somewhere to post them, until the store goes.
pub(crate) async fn run(
    store: Store,
    account_key: String,
    email: String,
    notifier: watch::Receiver<Option<Notifier>>,
) {
    // Each reminder shows once (by the event's key and its time), remembered until its event is
    // too far gone to show it.
    let mut shown: HashMap<(String, Timestamp), Timestamp> = HashMap::new();
    let mut fetched: Option<(Timestamp, Vec<String>)> = None;
    let mut poll = tokio::time::interval(POLL);
    loop {
        poll.tick().await;
        let Some(notifier) = notifier.borrow().clone() else {
            continue;
        };
        let settings: Settings = match store.settings().await {
            Ok(settings) => settings,
            Err(error) => {
                log::warn!("reading settings for calendar notifications: {error}");
                continue;
            }
        };
        let calendars = calendars(&settings);
        let (now, tz) = (Timestamp::now(), TimeZone::system());
        let today = now.to_zoned(tz.clone()).date();
        // Today and tomorrow, for what's on just after midnight.
        let days = today..today.checked_add(2.days()).unwrap_or(today);
        let stale = fetched
            .as_ref()
            .is_none_or(|(at, ids)| now.duration_since(*at) >= REFETCH || *ids != calendars);
        if stale {
            for calendar in &calendars {
                if let Err(error) = store.sync_events(calendar, days.clone(), &tz).await {
                    log::info!("fetching events for notifications: {error}");
                }
            }
            fetched = Some((now, calendars.clone()));
        }
        let events = match store.events(&calendars, days, &tz).await {
            Ok(events) => events,
            Err(error) => {
                log::warn!("reading events for calendar notifications: {error}");
                continue;
            }
        };
        let lead = SignedDuration::from_secs(settings.notifications.calendar_events.lead_time());
        shown.retain(|_, start| now <= *start + LATE);
        let mut due_now = Vec::new();
        for stored in &events {
            let owner = calendar_owner(&settings, &stored.calendar_id, &email);
            for reminder in reminders(stored, lead, owner)
                .into_iter()
                .filter(|reminder| due(reminder, now))
            {
                let id = (stored.event.id.clone().unwrap_or_default(), reminder.at);
                if shown.insert(id, reminder.start).is_none() {
                    due_now.push(notice_for(&reminder, &account_key, owner, now, &tz));
                }
            }
        }
        // An event due twice in one poll (two reminders) shows once.
        let mut keys = BTreeSet::new();
        due_now.retain(|notice| keys.insert(notice.key.clone()));
        notifier.post(due_now);
    }
}

#[cfg(test)]
mod tests {
    use exo_store::{
        CalendarAccount, CalendarNotifications, CalendarSetting, Conference, EventAttendee,
    };

    use super::*;

    const ME: &str = "maya@brightmoor.co";

    fn tz() -> TimeZone {
        TimeZone::get("America/New_York").expect("zone")
    }

    fn at(text: &str) -> Timestamp {
        format!("{text}[America/New_York]")
            .parse::<jiff::Zoned>()
            .expect("time")
            .timestamp()
    }

    fn stored(start: &str, end: &str) -> StoredEvent {
        StoredEvent {
            calendar_id: ME.into(),
            event: CalendarEvent {
                id: Some("evt1".into()),
                summary: Some("Design review".into()),
                start: Some(EventTime::timed(at(start), None)),
                end: Some(EventTime::timed(at(end), None)),
                ..CalendarEvent::default()
            },
            unsynced: false,
        }
    }

    fn lead() -> SignedDuration {
        SignedDuration::from_secs(120)
    }

    #[test]
    fn default_reminders_use_the_lead_time_and_pop_ups_their_own() {
        let event = stored("2026-11-17T10:00", "2026-11-17T10:30");
        let times = |event: &StoredEvent| {
            reminders(event, lead(), ME)
                .into_iter()
                .map(|reminder| reminder.at)
                .collect::<Vec<_>>()
        };
        assert_eq!(times(&event), [at("2026-11-17T09:58")]);
        let mut own = event.clone();
        own.event.reminders = EventReminders::Custom(vec![10, 1]);
        assert_eq!(
            times(&own),
            [at("2026-11-17T09:50"), at("2026-11-17T09:59")],
            "custom reminders only"
        );
        own.event.reminders = EventReminders::None;
        assert!(times(&own).is_empty(), "no reminders, no notification");
        own.event.reminders = EventReminders::Default;
        assert_eq!(times(&own), [at("2026-11-17T09:58")]);
    }

    #[test]
    fn some_events_never_notify() {
        let event = stored("2026-11-17T10:00", "2026-11-17T10:30");
        let quiet = |change: &dyn Fn(&mut CalendarEvent)| {
            let mut event = event.clone();
            change(&mut event.event);
            reminders(&event, lead(), ME).is_empty()
        };
        assert!(!quiet(&|_| {}));
        assert!(
            quiet(&|event| event.summary = Some(String::new())),
            "untitled"
        );
        assert!(quiet(&|event| event.status = EventStatus::Cancelled));
        assert!(
            quiet(&|event| event.attendees = vec![EventAttendee {
                email: Some(ME.into()),
                response_status: RsvpStatus::Declined,
                ..EventAttendee::default()
            }]),
            "declined"
        );
        assert!(
            quiet(&|event| {
                event.start = Some(EventTime::AllDay(jiff::civil::date(2026, 11, 17)));
                event.end = Some(EventTime::AllDay(jiff::civil::date(2026, 11, 18)));
            }),
            "all day"
        );
    }

    #[test]
    fn due_until_five_minutes_into_the_event() {
        let event = stored("2026-11-17T10:00", "2026-11-17T10:30");
        let [reminder] = reminders(&event, lead(), ME).try_into().expect("one");
        assert!(
            !due(&reminder, at("2026-11-17T09:58")),
            "strictly after its time"
        );
        assert!(due(&reminder, at("2026-11-17T09:58") + 1.second()));
        assert!(due(&reminder, at("2026-11-17T10:05")), "five minutes in");
        assert!(!due(&reminder, at("2026-11-17T10:05") + 1.second()));
    }

    #[test]
    fn what_a_notification_says() {
        let event = stored("2026-11-17T10:00", "2026-11-17T10:30");
        let [reminder] = reminders(&event, lead(), ME).try_into().expect("one");
        // The poll sees it up to 30 s late, and it still reads two minutes.
        for late in [1, 29] {
            let notice = notice(
                &reminder,
                ME,
                reminder.at + SignedDuration::from_secs(late),
                &tz(),
            );
            assert_eq!(notice.body, "in 2 mins · 10:00 – 10:30AM", "{late}s late");
        }
        let notice = super::notice(&reminder, ME, at("2026-11-17T10:03"), &tz());
        assert_eq!(notice.body, "3 mins ago · 10:00 – 10:30AM");
        assert_eq!(
            (
                notice.key.as_str(),
                notice.title.as_str(),
                notice.subtitle.as_deref(),
                notice.button.is_some()
            ),
            ("CALENDAR-evt1", "Design review", None, false)
        );
        assert_eq!(
            notice.click.open,
            Open::Event {
                key: format!("{ME}_evt1"),
                day: jiff::civil::date(2026, 11, 17),
                end_hour: 10,
                url: None
            }
        );

        let mut located = event.clone();
        located.event.location = Some("Room 4".into());
        let [reminder] = reminders(&located, lead(), ME).try_into().expect("one");
        assert_eq!(
            super::notice(&reminder, ME, reminder.at + 1.second(), &tz())
                .subtitle
                .as_deref(),
            Some("Room 4")
        );
    }

    #[test]
    fn a_call_names_its_provider_and_joins() {
        let mut event = stored("2026-11-17T10:00", "2026-11-17T10:30");
        event.event.location = Some("Room 4".into());
        event.event.conference = Some(Conference {
            kind: ConferenceKind::GoogleMeet,
            join_url: Some("https://meet.google.com/abc-defg-hij".into()),
            label: None,
            pending: false,
            dial_in: Some("tel:+1-555-0100".into()),
        });
        let [reminder] = reminders(&event, lead(), ME).try_into().expect("one");
        let notice = notice(&reminder, ME, reminder.at + 1.second(), &tz());
        assert_eq!(
            notice.subtitle.as_deref(),
            Some("Google Meet"),
            "over the location"
        );
        let Some(Click {
            open: Open::Event { url, .. },
            ..
        }) = notice.button
        else {
            panic!("Join meeting")
        };
        assert_eq!(
            url.as_deref(),
            Some("https://meet.google.com/abc-defg-hij?authuser=maya%40brightmoor.co")
        );

        let provider = |kind: ConferenceKind| {
            let event = CalendarEvent {
                conference: Some(Conference {
                    kind,
                    join_url: Some("https://example.com/call".into()),
                    label: None,
                    pending: false,
                    dial_in: None,
                }),
                ..CalendarEvent::default()
            };
            conference(&event, ME)
        };
        assert_eq!(provider(ConferenceKind::Zoom).0, Some("Zoom"));
        assert_eq!(
            provider(ConferenceKind::Teams),
            (
                Some("Microsoft Teams"),
                Some("https://example.com/call".into())
            )
        );
        assert_eq!(
            provider(ConferenceKind::Other("Whereby".into())).0,
            Some("Video")
        );

        // Phone-only and SIP-only conferences produce Join meeting buttons with valid tel:/sip: URIs.
        for uri in ["tel:+1-555-0100", "sip:123456@myprovider.com"] {
            let mut dial = stored("2026-11-17T10:00", "2026-11-17T10:30");
            dial.event.conference = Some(Conference {
                kind: ConferenceKind::Other(String::new()),
                join_url: None,
                label: None,
                pending: false,
                dial_in: Some(uri.into()),
            });
            let [reminder] = reminders(&dial, lead(), ME).try_into().expect("one");
            let notice = super::notice(&reminder, ME, reminder.at + 1.second(), &tz());
            let Some(Click {
                open: Open::Event { url: Some(got), .. },
                ..
            }) = notice.button
            else {
                panic!("Join meeting button for {uri}");
            };
            assert_eq!(super::super::meeting_uri(&got).as_deref(), Some(uri));
        }
    }

    #[test]
    fn only_calendars_that_sync_with_notifications_on() {
        let setting = |sync: bool, desktop: bool| CalendarSetting {
            sync,
            show: true,
            notifications: Some(CalendarNotifications {
                enabled: true,
                enabled_desktop: desktop,
            }),
            ..CalendarSetting::default()
        };
        let mut settings = Settings::default();
        let calendars_of = [
            ("a", setting(true, true)),
            ("b", setting(true, false)),
            ("c", setting(false, true)),
            (
                "d",
                CalendarSetting {
                    sync: true,
                    ..CalendarSetting::default()
                },
            ),
        ];
        settings.calendar_accounts.insert(
            ME.into(),
            CalendarAccount {
                email: ME.into(),
                calendars: calendars_of
                    .into_iter()
                    .map(|(id, setting)| (id.to_owned(), setting))
                    .collect(),
                ..CalendarAccount::default()
            },
        );
        assert_eq!(calendars(&settings), ["a"]);
        // A calendar account's calendars notify too, unless it's signed out.
        let other = |authed: bool| CalendarAccount {
            email: "maya.lin@example.com".into(),
            authed,
            calendars: [("e".to_owned(), setting(true, true))].into(),
            ..CalendarAccount::default()
        };
        settings
            .calendar_accounts
            .insert("maya.lin@example.com".into(), other(true));
        assert_eq!(calendars(&settings), ["a", "e"]);
        settings
            .calendar_accounts
            .insert("maya.lin@example.com".into(), other(false));
        assert_eq!(calendars(&settings), ["a"]);
    }

    #[test]
    fn cross_provider_calendar_uses_owner_email_for_rsvp_and_meet_authuser() {
        let priya = "priya@brightmoor.co";
        let mut settings = Settings::default();
        settings.calendar_accounts.insert(
            format!("microsoft {ME}"),
            CalendarAccount {
                email: ME.into(),
                provider: "microsoft".into(),
                authed: true,
                calendars: [("AAMkPrimary".into(), CalendarSetting::default())].into(),
            },
        );
        settings.calendar_accounts.insert(
            priya.into(),
            CalendarAccount {
                email: priya.into(),
                provider: "google".into(),
                authed: true,
                calendars: [(priya.into(), CalendarSetting::default())].into(),
            },
        );

        assert_eq!(calendar_owner(&settings, priya, ME), priya);
        assert_eq!(calendar_owner(&settings, "AAMkPrimary", ME), ME);
        assert_eq!(calendar_owner(&settings, "unknown", ME), ME);

        // Declined by Priya suppresses the reminder even though Maya didn't decline.
        let mut declined = stored("2026-11-17T10:00", "2026-11-17T10:30");
        declined.calendar_id = priya.into();
        declined.event.attendees = vec![EventAttendee {
            email: Some(priya.into()),
            response_status: RsvpStatus::Declined,
            ..EventAttendee::default()
        }];
        let owner = calendar_owner(&settings, &declined.calendar_id, ME);
        assert!(reminders(&declined, lead(), owner).is_empty());

        // Google Meet link on Priya's secondary calendar uses Priya for `authuser` while keeping
        // the notification click routed to the primary Microsoft mail account's canonical key.
        let primary_key = format!("microsoft {ME}");
        let mut meet = stored("2026-11-17T10:00", "2026-11-17T10:30");
        meet.calendar_id = priya.into();
        meet.event.conference = Some(Conference {
            kind: ConferenceKind::GoogleMeet,
            join_url: Some("https://meet.google.com/abc-defg-hij".into()),
            label: None,
            pending: false,
            dial_in: None,
        });
        let [reminder] = reminders(&meet, lead(), owner).try_into().expect("one");
        let built = notice_for(
            &reminder,
            &primary_key,
            owner,
            reminder.at + 1.second(),
            &tz(),
        );
        assert_eq!(built.click.account, primary_key);
        let Some(Click {
            account,
            open: Open::Event { url: Some(got), .. },
        }) = built.button
        else {
            panic!("expected Join meeting button");
        };
        assert_eq!(account, primary_key);
        assert_eq!(
            got,
            "https://meet.google.com/abc-defg-hij?authuser=priya%40brightmoor.co"
        );
    }
}
