//! An event as the calendar shows it: its title, times, colour, and my answer, resolved from
//! Calendar's event and the account's calendar settings.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use exo_store::{
    AccessRole, Calendar, CalendarEvent, ConferenceKind, EventTime, EventType, RsvpStatus,
    Settings, ShowAs, StoredEvent, Visibility,
};
use exo_ui::CalendarColor;
use jiff::{Zoned, civil::Date, tz::TimeZone};
use regex::Regex;

pub use crate::rsvp::Rsvp;
use crate::thread::invite::Invite;

/// A video call extracted from an event or `.ics` description (`Meet`, `Zoom`, or `Teams`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConferenceCall {
    /// The row's short provider label (`"Meet"`, `"Zoom"`, or `"Teams"`).
    pub label: &'static str,
    pub url: String,
}

/// A resolved conference call ready for the event details popover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedConference {
    /// The entry point's label (`meet.google.com/abc-defg-hij`).
    pub label: String,
    /// Its link (a Meet link opens as `me`, with `authuser`), and the link as Calendar has it.
    pub uri: String,
    pub raw: String,
    /// The copy tooltip's text.
    pub copy: String,
    pub join: &'static str,
}

static TEAMS_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([\n|]\s*)(.+?)<(https?://.+?)>").expect("valid regex"));
static ANGLE_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(https?://.+?)>").expect("valid regex"));
static MEET_JOIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|\n)(?:Join|Join with Google Meet): (https?://(?:www\.)?meet\.google\.com/\S+)",
    )
    .expect("valid regex")
});
static MEET_BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://(?:www\.)?meet\.google\.com/[a-z]{3}-[a-z]{4}-[a-z]{3}[^\s<>"')]*"#)
        .expect("valid regex")
});
static ZOOM_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://(?:[a-zA-Z0-9-]+\.)?zoom\.us/(?:j|my|w)/[^\s<>"')]+"#)
        .expect("valid regex")
});
static TEAMS_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://teams\.microsoft\.com/l/meetup-join/[^\s<>"')]+"#).expect("valid regex")
});

/// Rewrites `<url>` and Teams `text<url>` join lines into HTML `<a>` links and extracts the first
/// `Meet`, `Teams`, or `Zoom` video call URL found in `description`.
#[must_use]
pub fn extract_conference_text(description: &str) -> (String, Option<ConferenceCall>) {
    let mut text = description.to_owned();
    let mut teams = None;
    if description.contains("Microsoft Teams meeting") {
        let trimmed = text.trim().trim_matches('_').to_owned();
        if let Some(at) = trimmed.find("Microsoft Teams meeting") {
            let tail = TEAMS_LINE.replace_all(&trimmed[at..], |found: &regex::Captures| {
                if found[3].contains("teams.microsoft.com/l/meetup-join/") {
                    teams = Some(found[3].to_owned());
                }
                format!("{}<a href=\"{}\">{}</a>", &found[1], &found[3], &found[2])
            });
            text = format!("{}{tail}", &trimmed[..at]);
        }
    }
    let text = ANGLE_URL
        .replace_all(&text, " <a href=\"$1\">$1</a>")
        .into_owned();
    let meet = description.contains("meet.google.com").then(|| {
        MEET_JOIN
            .captures(description)
            .map(|found| found[1].to_owned())
            .or_else(|| {
                MEET_BARE
                    .find(description)
                    .map(|found| found.as_str().to_owned())
            })
    });
    let teams = teams.or_else(|| {
        description.contains("teams.microsoft.com").then(|| {
            TEAMS_URL
                .find(description)
                .map(|found| found.as_str().to_owned())
        })?
    });
    let zoom = description.contains("zoom.us").then(|| {
        ZOOM_URL
            .find(description)
            .map(|found| found.as_str().to_owned())
    });
    let call = meet
        .flatten()
        .map(|url| ConferenceCall { label: "Meet", url })
        .or_else(|| {
            teams.map(|url| ConferenceCall {
                label: "Teams",
                url,
            })
        })
        .or_else(|| {
            zoom.flatten()
                .map(|url| ConferenceCall { label: "Zoom", url })
        });
    (text, call)
}

/// Returns the video entry-point URI on `source`, if a non-pending conference has one.
#[must_use]
pub fn video_uri_of(source: &CalendarEvent) -> Option<String> {
    source
        .conference
        .as_ref()
        .filter(|conf| !conf.pending)
        .and_then(|conf| conf.join_url.clone())
}

/// Resolves an event's conference row from its typed [`exo_store::Conference`], falling back to
/// Meet/Zoom/Teams URLs embedded in `description` (such as `.ics` invites).
#[must_use]
pub fn conference_of(source: &CalendarEvent, me: &str) -> Option<ResolvedConference> {
    if let Some(conf) = source.conference.as_ref().filter(|conf| !conf.pending)
        && let Some(raw) = conf.join_url.as_deref()
        && let Some(web) = crate::format::urls::web(raw)
    {
        let default_label = || {
            conf.label
                .clone()
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| raw.trim_start_matches("https://").to_owned())
        };
        let (label, uri, copy, join) = match &conf.kind {
            ConferenceKind::GoogleMeet => {
                let label = default_label();
                let mut link = url::Url::parse(&web).ok()?;
                link.query_pairs_mut().append_pair("authuser", me);
                (label.clone(), link.to_string(), label, "Join Google Meet")
            }
            ConferenceKind::Teams => {
                let label = if raw.contains("teams.microsoft.com/l/meetup-join/") {
                    "teams.microsoft.com/l/meetup-join/…".to_owned()
                } else {
                    default_label()
                };
                (label, web.clone(), web, "Join Teams Call")
            }
            ConferenceKind::Zoom => (default_label(), web.clone(), web, "Join Zoom Call"),
            ConferenceKind::Other(_) => (default_label(), web.clone(), web, "Join Meeting"),
        };
        return Some(ResolvedConference {
            label,
            uri,
            raw: raw.to_owned(),
            copy,
            join,
        });
    }
    let description = source.description.as_deref()?;
    let (_, call) = extract_conference_text(description);
    let call = call?;
    let web = crate::format::urls::web(&call.url)?;
    let (label, uri, copy, join) = match call.label {
        "Meet" => {
            let label = web.trim_start_matches("https://").to_owned();
            let mut link = url::Url::parse(&web).ok()?;
            link.query_pairs_mut().append_pair("authuser", me);
            (label.clone(), link.to_string(), label, "Join Google Meet")
        }
        "Zoom" => (
            web.trim_start_matches("https://").to_owned(),
            web.clone(),
            web,
            "Join Zoom Call",
        ),
        "Teams" => (
            "teams.microsoft.com/l/meetup-join/…".to_owned(),
            web.clone(),
            web,
            "Join Teams Call",
        ),
        _ => return None,
    };
    Some(ResolvedConference {
        label,
        uri,
        raw: call.url,
        copy,
        join,
    })
}

/// How a block is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// A coloured fill, for an event accepted or mine.
    Filled,
    /// An outline, for an event declined or not answered.
    Bordered,
    /// The fill in a dashed outline, for a maybe.
    Dashed,
}

/// An event on the calendar.
#[derive(Clone, Debug)]
pub struct Event {
    /// Unique per instance.
    pub key: String,
    /// What the block says: the summary, `Busy` when its details are hidden, else `(No title)`.
    pub title: String,
    pub start: Zoned,
    pub end: Zoned,
    /// True date-only all-day event (`EventTime::AllDay` or `VALUE=DATE`).
    pub all_day: bool,
    /// Placed in the grid's all-day header band (`all_day || duration >= 24 hours`).
    pub span_row: bool,
    pub color: CalendarColor,
    /// My answer.
    pub rsvp: Option<Rsvp>,
    /// The answer the block shows.
    pub busy: Rsvp,
    /// Everyone but me declined.
    pub everyone_else_declined: bool,
    /// iCalendar UID, shared with the invite in a thread.
    pub ical_uid: Option<String>,
    /// The calendar it's on (empty for an invite the calendar doesn't have).
    pub calendar_id: String,
    /// Calendar's id for it (none for such an invite).
    pub id: Option<String>,
    /// I may change it, because I organize it on a calendar I can write to.
    pub editable: bool,
    /// What its block says for the time instead of its own range, for a draft typed in another
    /// zone.
    pub time_text: Option<String>,
    /// A slot of availability being shared, and whether it can be shared (it must be as long as
    /// the meeting and not over).
    pub share: Option<bool>,
    /// What its block says while hovered, which is why a slot can't be shared.
    pub tooltip: Option<&'static str>,
}

impl Event {
    /// Whether the event belongs in the calendar header's all-day band (`all_day` or `>= 24h`).
    #[must_use]
    pub const fn in_all_day_row(&self) -> bool {
        self.span_row
    }

    /// The `stored` event from `calendar`, for `me`, coloured by `colors` (each calendar's colour
    /// from the settings), its times in `tz`. `None` for an event without usable times.
    pub fn new(
        stored: &StoredEvent,
        calendar: Option<&Calendar>,
        colors: &BTreeMap<String, CalendarColor>,
        me: &str,
        tz: &TimeZone,
    ) -> Option<Self> {
        let event = &stored.event;
        let start_time = event.start.as_ref()?;
        let end_time = event.end.as_ref()?;
        let start = start_time.zoned(tz)?;
        let end = end_time.zoned(tz)?;
        let all_day = start_time.is_all_day() && end_time.is_all_day();
        let span_row =
            all_day || end.timestamp().duration_since(start.timestamp()).as_hours() >= 24;
        let rsvp = my_attendance(event, me);
        let access = calendar.map(|calendar| calendar.access_role);
        let private = matches!(
            event.visibility,
            Visibility::Private | Visibility::Confidential
        );
        let hidden = access == Some(AccessRole::FreeBusyReader)
            || (private
                && matches!(access, Some(AccessRole::Reader | AccessRole::Writer))
                && !calendar.is_some_and(|calendar| calendar.can_view_private_items));
        let title = match event
            .summary
            .as_deref()
            .filter(|summary| !summary.is_empty())
        {
            Some(summary) => summary.to_owned(),
            None if hidden => "Busy".to_owned(),
            None => "(No title)".to_owned(),
        };
        let color = event
            .color_id
            .as_deref()
            .and_then(google_color)
            .or_else(|| {
                event
                    .categories
                    .iter()
                    .find_map(|category| category.preset.and_then(outlook_category_color))
            })
            .or_else(|| colors.get(&stored.calendar_id).copied())
            .unwrap_or(CalendarColor::BLUE);
        let organizer = event.is_me_organizer(me);
        let writable = calendar.is_some_and(|calendar| {
            calendar.is_writable() && (!private || calendar.can_view_private_items)
        });
        // A new event that Calendar hasn't got yet is editable too. The edit waits behind the new
        // event in its queue.
        let editable = organizer && writable && event.event_type != EventType::FromGmail;
        let creator = event
            .creator
            .as_ref()
            .is_some_and(|creator| creator.is_me(me));
        let invitee = event.attendees.iter().any(|attendee| attendee.is_me(me));
        let busy = rsvp.unwrap_or(if creator || !invitee {
            if event.show_as == ShowAs::Tentative {
                Rsvp::Tentative
            } else {
                Rsvp::Accepted
            }
        } else {
            Rsvp::NeedsAction
        });
        let others: Vec<_> = event
            .attendees
            .iter()
            .filter(|attendee| !attendee.resource && !attendee.is_me(me))
            .collect();
        let everyone_else_declined = !others.is_empty()
            && others
                .iter()
                .all(|attendee| attendee.response_status == RsvpStatus::Declined);
        Some(Self {
            key: unique_key(stored, event, &start),
            title,
            start,
            end,
            all_day,
            span_row,
            color,
            rsvp,
            busy,
            everyone_else_declined,
            ical_uid: event.ical_uid.clone(),
            calendar_id: stored.calendar_id.clone(),
            id: event.id.clone(),
            editable,
            time_text: None,
            share: None,
            tooltip: None,
        })
    }

    /// The invite being read as an event, for a day that doesn't have it on the calendar, in the
    /// primary calendar's colour.
    pub fn from_invite(invite: &Invite, color: CalendarColor, me: &str, tz: &TimeZone) -> Self {
        let rsvp = invite
            .attendees
            .iter()
            .find(|(_, email, _)| email.eq_ignore_ascii_case(me))
            .map(|(_, _, response)| *response);
        let (start, end) = invite.zoned(tz);
        let all_day = invite.all_day;
        let span_row =
            all_day || end.timestamp().duration_since(start.timestamp()).as_hours() >= 24;
        Self {
            key: format!("calendar-invite_{}", invite.uid),
            title: if invite.summary.is_empty() {
                "(No title)".to_owned()
            } else {
                invite.summary.clone()
            },
            all_day,
            span_row,
            start,
            end,
            color,
            rsvp,
            busy: rsvp.unwrap_or(Rsvp::NeedsAction),
            everyone_else_declined: false,
            ical_uid: Some(invite.uid.clone()),
            calendar_id: String::new(),
            id: None,
            editable: false,
            time_text: None,
            share: None,
            tooltip: None,
        }
    }

    /// The event a form makes or changes: mine, accepted, editable.
    pub fn draft(
        key: String,
        title: String,
        start: Zoned,
        end: Zoned,
        all_day: bool,
        color: CalendarColor,
    ) -> Self {
        let span_row =
            all_day || end.timestamp().duration_since(start.timestamp()).as_hours() >= 24;
        Self {
            key,
            title,
            start,
            end,
            all_day,
            span_row,
            color,
            rsvp: None,
            busy: Rsvp::Accepted,
            everyone_else_declined: false,
            ical_uid: None,
            calendar_id: String::new(),
            id: None,
            editable: true,
            time_text: None,
            share: None,
            tooltip: None,
        }
    }

    /// Whether the block is drawn. Declined events are left off unless the settings show them.
    pub fn shown(&self, show_declined: bool) -> bool {
        show_declined || self.rsvp != Some(Rsvp::Declined)
    }

    /// The block's style.
    pub fn style(&self) -> Style {
        match self.busy {
            Rsvp::Declined | Rsvp::NeedsAction => Style::Bordered,
            Rsvp::Tentative => Style::Dashed,
            Rsvp::Accepted => Style::Filled,
        }
    }

    /// Whether it's on `day`, which means it starts before the day ends and ends after the day
    /// starts.
    pub fn on(&self, day: Date) -> bool {
        let tz = self.start.time_zone().clone();
        let (Ok(from), Ok(to)) = (
            day.to_zoned(tz.clone()),
            day.tomorrow().and_then(|next| next.to_zoned(tz)),
        ) else {
            return false;
        };
        self.start < to && self.end > from
    }
}

/// My answer to `event`, from my attendee entry or Outlook's `responseStatus`.
pub(crate) fn my_attendance(event: &CalendarEvent, me: &str) -> Option<Rsvp> {
    event.my_rsvp(me).map(Rsvp::from_store)
}

/// An event's own colour, by Google Calendar's colour id.
fn google_color(id: &str) -> Option<CalendarColor> {
    let name = match id {
        "1" => "cobalt",
        "2" => "green",
        "3" => "magenta",
        "4" => "red",
        "5" => "yellow",
        "6" => "orange",
        "7" => "blue",
        "8" => "pewter",
        "9" => "navy",
        "10" => "lime",
        "11" => "pink",
        _ => return None,
    };
    CalendarColor::from_name(name)
}

/// Maps Outlook's 25 category colour presets (`preset0`..=`preset24`) to [`CalendarColor`].
pub(crate) fn outlook_category_color(preset: u8) -> Option<CalendarColor> {
    let name = match preset {
        0 => "red",
        1 => "orange",
        2 => "cinnamon",
        3 => "yellow",
        4 => "green",
        5 => "storm",
        6 => "olive",
        7 => "blue",
        8 => "violet",
        9 => "pink",
        10 => "steel",
        11 => "slate",
        12 => "pewter",
        13 | 14 => "charcoal",
        15 => "blush",
        16 => "sienna",
        17 | 18 => "ochre",
        19 => "forest",
        20 => "storm",
        21 => "moss",
        22 => "navy",
        23 => "orchid",
        24 => "magenta",
        _ => return None,
    };
    CalendarColor::from_name(name)
}

/// An event's key: the calendar and the event, with an instance keyed by its series and original
/// start.
pub(crate) fn unique_key(stored: &StoredEvent, event: &CalendarEvent, start: &Zoned) -> String {
    let id = match &event.recurring_event_id {
        Some(series) => {
            let original = event
                .original_start
                .as_ref()
                .and_then(|original: &EventTime| original.timestamp(start.time_zone()))
                .unwrap_or(start.timestamp());
            format!("{series}_{}", original.as_millisecond())
        }
        None => event.id.clone().unwrap_or_default(),
    };
    format!("{}_{id}", stored.calendar_id)
}

/// Each calendar's colour, over every signed-in calendar account. That's the colour in the
/// settings, or else blue.
pub fn colors(settings: &Settings) -> BTreeMap<String, CalendarColor> {
    let mut colors = BTreeMap::new();
    for account in settings
        .calendar_accounts
        .values()
        .filter(|account| account.authed)
    {
        for (id, setting) in &account.calendars {
            colors.entry(id.clone()).or_insert_with(|| {
                setting
                    .color
                    .as_deref()
                    .and_then(CalendarColor::from_name)
                    .unwrap_or(CalendarColor::BLUE)
            });
        }
    }
    colors
}

#[cfg(test)]
mod tests {
    use exo_store::{Conference, EventAttendee, EventCategory, EventPerson};

    use super::*;

    fn tz() -> TimeZone {
        TimeZone::get("America/New_York").expect("zone")
    }

    fn timed(start: &str, end: &str) -> CalendarEvent {
        let at = |text: &str| EventTime::timed(text.parse().expect("ts"), None);
        CalendarEvent {
            id: Some("e1".into()),
            start: Some(at(start)),
            end: Some(at(end)),
            ..CalendarEvent::default()
        }
    }

    fn stored(event: CalendarEvent) -> StoredEvent {
        StoredEvent {
            calendar_id: "maya@brightmoor.co".into(),
            event,
            unsynced: false,
        }
    }

    #[test]
    fn answers_and_styles() {
        let mut event = timed("2026-11-19T14:00:00-05:00", "2026-11-19T15:00:00-05:00");
        event.attendees = vec![
            EventAttendee {
                email: Some("jordan@calloway.com".into()),
                response_status: RsvpStatus::Accepted,
                ..EventAttendee::default()
            },
            EventAttendee {
                email: Some("maya@brightmoor.co".into()),
                is_self: true,
                response_status: RsvpStatus::NeedsAction,
                ..EventAttendee::default()
            },
        ];
        let invite = Event::new(
            &stored(event.clone()),
            None,
            &BTreeMap::new(),
            "maya@brightmoor.co",
            &tz(),
        )
        .expect("event");
        assert_eq!(
            (invite.rsvp, invite.style(), invite.title.as_str()),
            (Some(Rsvp::NeedsAction), Style::Bordered, "(No title)")
        );
        event.attendees[1].response_status = RsvpStatus::Declined;
        let declined = Event::new(
            &stored(event),
            None,
            &BTreeMap::new(),
            "maya@brightmoor.co",
            &tz(),
        )
        .expect("event");
        assert!(!declined.shown(false) && declined.shown(true));

        // Without attendees, my own event is accepted.
        let mut own = timed("2026-11-19T19:30:00-05:00", "2026-11-19T21:30:00-05:00");
        own.creator = Some(EventPerson {
            email: Some("maya@brightmoor.co".into()),
            ..EventPerson::default()
        });
        let own = Event::new(
            &stored(own),
            None,
            &BTreeMap::new(),
            "maya@brightmoor.co",
            &tz(),
        )
        .expect("event");
        assert_eq!(own.busy, Rsvp::Accepted);
    }

    #[test]
    fn a_day_long_timed_event_uses_the_all_day_row_without_being_date_only() {
        let long = Event::new(
            &stored(timed(
                "2026-12-09T09:00:00-05:00",
                "2026-12-11T17:00:00-05:00",
            )),
            None,
            &BTreeMap::new(),
            "maya@brightmoor.co",
            &tz(),
        )
        .expect("event");
        assert!(
            !long.all_day,
            "not a date-only event for When/editing semantics"
        );
        assert!(
            long.in_all_day_row(),
            "placed in the header band because >= 24h"
        );
        assert!(long.on("2026-12-10".parse().expect("date")));
        assert!(!long.on("2026-12-12".parse().expect("date")));
    }

    #[test]
    fn extracts_meet_zoom_and_teams_calls() {
        let (_, zoom) = extract_conference_text("Join Zoom: https://acme.zoom.us/j/91234567890");
        assert_eq!(
            zoom,
            Some(ConferenceCall {
                label: "Zoom",
                url: "https://acme.zoom.us/j/91234567890".into(),
            })
        );
        let from_desc = CalendarEvent {
            description: Some("Join with Google Meet: https://meet.google.com/abc-defg-hij".into()),
            ..CalendarEvent::default()
        };
        let resolved = conference_of(&from_desc, "maya@brightmoor.co").expect("resolved");
        assert_eq!(resolved.join, "Join Google Meet");
        assert_eq!(
            resolved.uri,
            "https://meet.google.com/abc-defg-hij?authuser=maya%40brightmoor.co"
        );

        let teams_event = CalendarEvent {
            conference: Some(Conference {
                kind: ConferenceKind::Teams,
                join_url: Some("https://teams.microsoft.com/l/meetup-join/abc123".into()),
                label: Some("Microsoft Teams".into()),
                pending: false,
                dial_in: None,
            }),
            ..CalendarEvent::default()
        };
        let teams = conference_of(&teams_event, "maya@brightmoor.co").expect("teams");
        assert_eq!(teams.join, "Join Teams Call");
        assert_eq!(teams.label, "teams.microsoft.com/l/meetup-join/…");
        assert_eq!(
            teams.uri,
            "https://teams.microsoft.com/l/meetup-join/abc123"
        );
    }

    #[test]
    fn colours_come_from_the_event_or_category_then_the_calendar() {
        let mut event = timed("2026-11-19T14:00:00-05:00", "2026-11-19T15:00:00-05:00");
        let colors = BTreeMap::from([(
            "maya@brightmoor.co".to_owned(),
            CalendarColor::from_name("green").expect("colour"),
        )]);
        assert_eq!(
            Event::new(&stored(event.clone()), None, &colors, "me", &tz())
                .expect("event")
                .color
                .name(),
            "green"
        );
        event.color_id = Some("11".into());
        assert_eq!(
            Event::new(&stored(event.clone()), None, &colors, "me", &tz())
                .expect("event")
                .color
                .name(),
            "pink"
        );
        event.color_id = None;
        event.categories = vec![EventCategory {
            name: "Red category".into(),
            preset: Some(0),
        }];
        assert_eq!(
            Event::new(&stored(event), None, &colors, "me", &tz())
                .expect("event")
                .color
                .name(),
            "red"
        );
        for preset in 0..=24 {
            assert!(
                outlook_category_color(preset).is_some(),
                "preset{preset} maps to a calendar colour"
            );
        }
        assert_eq!(outlook_category_color(25), None);
    }
}
