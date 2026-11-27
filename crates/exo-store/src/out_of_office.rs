//! Out of Office Reply: Gmail's vacation responder and Outlook's automatic replies, the copy of
//! them the account's settings keep (`outOfOfficeResponder`), and the calendar's out-of-office
//! events Exo offers to set a reply for (`outOfOfficeEventDetectionV2`).
//!
//! Exo reads and writes automatic replies through [`Store::auto_reply`] and
//! [`Store::update_auto_reply`]. The copy is what the status bar and the nudge go by between reads.
//!
//! ```no_run
//! # async fn demo(store: &exo_store::Store) -> exo_store::Result<()> {
//! use exo_store::{OutOfOfficeResponder, Settings};
//!
//! let reply = store.auto_reply().await?;
//! let mut settings: Settings = store.settings().await?;
//! let now = jiff::Timestamp::now();
//! if let Some(copy) = OutOfOfficeResponder::sync_auto_reply(&reply, settings.out_of_office_responder.as_ref(), now) {
//!     settings.out_of_office_responder = Some(copy);
//!     store.save_settings(&settings).await?;
//! }
//! # Ok(()) }
//! ```

use std::collections::BTreeMap;

use gmail::VacationSettings;
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ToSpan, Zoned};
use serde::{Deserialize, Serialize};

use crate::calendar::model::{CalendarEvent, EventStatus, EventTime};
use crate::error::Result;
use crate::store::Store;

/// How far ahead Exo looks at events (35 days).
pub const OUT_OF_OFFICE_LOOKAHEAD: SignedDuration = SignedDuration::from_hours(35 * 24);
/// The words a summary marks an out-of-office event with.
const WORDS: [&str; 8] = [
    "ooo",
    "out of office",
    "out-of-office",
    "paid time off",
    "unpaid time off",
    "pto",
    "vacation",
    "annual leave",
];
/// The shortest an out-of-office event can be (8 hours).
const LONG_EVENT: SignedDuration = SignedDuration::from_hours(8);

/// Provider-neutral automatic-reply settings for either Gmail or Outlook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum AutoReply {
    /// Gmail's vacation responder (`users.settings.vacation`).
    Gmail(VacationSettings),
    /// Outlook's automatic replies (`mailboxSettings.automaticRepliesSetting`).
    Outlook(OutlookAutoReply),
}

/// Outlook's automatic replies (`mailboxSettings.automaticRepliesSetting`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutlookAutoReply {
    /// Off, always on, or on between `start_time` and `end_time`.
    pub status: AutoReplyStatus,
    /// Who outside the organization receives `external_reply_html`.
    pub external_audience: ExternalAudience,
    /// HTML reply sent to people inside the organization.
    pub internal_reply_html: String,
    /// HTML reply sent to people outside the organization.
    pub external_reply_html: String,
    /// When a scheduled reply starts, in epoch milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// When a scheduled reply ends, in epoch milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
}

/// Whether Outlook's automatic replies are disabled, always enabled, or scheduled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoReplyStatus {
    /// Automatic replies are off.
    #[default]
    Disabled,
    /// Automatic replies are on until turned off.
    AlwaysEnabled,
    /// Automatic replies are on during the scheduled window.
    Scheduled,
}

/// Who outside the organization receives Outlook's external automatic reply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExternalAudience {
    /// Nobody outside the organization.
    #[default]
    None,
    /// Only the user's saved contacts outside the organization.
    ContactsOnly,
    /// Everyone outside the organization.
    All,
}

/// The account's cached copy of its automatic responder (`outOfOfficeResponder`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutOfOfficeResponder {
    /// `google`.
    pub google: GoogleResponder,
    /// `outlook`: populated for Outlook accounts; omitted when `None` so Gmail JSON stays
    /// unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outlook: Option<OutlookResponder>,
    /// When replies start, in epoch milliseconds (`startTime`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// When replies stop, in epoch milliseconds (`endTime`), or none for no last day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
}

/// `outOfOfficeResponder.outlook`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutlookResponder {
    /// When the copy was last written with replies enabled, as an ISO timestamp, or empty while
    /// off (`enabledAutoReplyAt`).
    pub enabled_auto_reply_at: String,
    /// Off, always on, or scheduled.
    pub status: AutoReplyStatus,
    /// Who outside the organization receives `external_reply_html`.
    pub external_audience: ExternalAudience,
    /// HTML reply sent inside the organization.
    pub internal_reply_html: String,
    /// HTML reply sent outside the organization.
    pub external_reply_html: String,
}

/// `outOfOfficeResponder.google`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GoogleResponder {
    /// When the copy was last written with the reply on, as an ISO timestamp, or empty while the
    /// reply is off (`enabledAutoReplyAt`).
    pub enabled_auto_reply_at: String,
    /// `responseSubject`.
    pub response_subject: String,
    /// `responseBodyHtml`.
    pub response_body_html: String,
    /// `restrictToContacts`.
    pub restrict_to_contacts: bool,
    /// `restrictToDomain`.
    pub restrict_to_domain: bool,
}

impl OutOfOfficeResponder {
    /// The copy of Gmail's `settings` as written at `now`.
    pub fn of(settings: &VacationSettings, now: Timestamp) -> Self {
        let enabled = settings.enable_auto_reply == Some(true);
        Self {
            google: GoogleResponder {
                enabled_auto_reply_at: if enabled {
                    now.to_string()
                } else {
                    String::new()
                },
                response_subject: settings.response_subject.clone().unwrap_or_default(),
                response_body_html: settings.response_body_html.clone().unwrap_or_default(),
                restrict_to_contacts: settings.restrict_to_contacts == Some(true),
                restrict_to_domain: settings.restrict_to_domain == Some(true),
            },
            outlook: None,
            start_time: settings.start_time,
            end_time: settings.end_time,
        }
    }

    /// The copy of either provider's [`AutoReply`] as written at `now`.
    pub fn from_auto_reply(reply: &AutoReply, now: Timestamp) -> Self {
        match reply {
            AutoReply::Gmail(settings) => Self::of(settings, now),
            AutoReply::Outlook(settings) => {
                let enabled = settings.status != AutoReplyStatus::Disabled;
                let (start_time, end_time) = if settings.status == AutoReplyStatus::Scheduled {
                    (settings.start_time, settings.end_time)
                } else {
                    (None, None)
                };
                Self {
                    google: GoogleResponder::default(),
                    outlook: Some(OutlookResponder {
                        enabled_auto_reply_at: if enabled {
                            now.to_string()
                        } else {
                            String::new()
                        },
                        status: settings.status,
                        external_audience: settings.external_audience,
                        internal_reply_html: settings.internal_reply_html.clone(),
                        external_reply_html: settings.external_reply_html.clone(),
                    }),
                    start_time,
                    end_time,
                }
            }
        }
    }

    /// The copy to keep after reading Gmail's `settings` at `now`, if `copy` no longer matches
    /// them. A plain-text body (one set in Gmail's plain-text mode) stands in for the HTML one.
    /// `None` when nothing changed.
    pub fn synced(
        settings: &VacationSettings,
        copy: Option<&Self>,
        now: Timestamp,
    ) -> Option<Self> {
        if !differs(settings, copy) {
            return None;
        }
        let mut settings = settings.clone();
        if let Some(plain) = settings
            .response_body_plain_text
            .take()
            .filter(|plain| !plain.is_empty())
            && copy.is_none_or(|copy| plain != copy.google.response_body_html)
        {
            settings.response_body_html = Some(plain);
        }
        Some(Self::of(&settings, now))
    }

    /// The copy to keep after reading either provider's [`AutoReply`] at `now`, or `None` when
    /// `copy` already matches.
    pub fn sync_auto_reply(reply: &AutoReply, copy: Option<&Self>, now: Timestamp) -> Option<Self> {
        match reply {
            AutoReply::Gmail(settings) => Self::synced(settings, copy, now),
            AutoReply::Outlook(settings) => {
                if !outlook_differs(settings, copy) {
                    return None;
                }
                Some(Self::from_auto_reply(reply, now))
            }
        }
    }

    /// Whether the reply is on.
    pub fn is_enabled(&self) -> bool {
        match &self.outlook {
            Some(outlook) => {
                outlook.status != AutoReplyStatus::Disabled
                    && !outlook.enabled_auto_reply_at.is_empty()
            }
            None => !self.google.enabled_auto_reply_at.is_empty(),
        }
    }

    /// Whether replies are going out at `now`: enabled (and within the scheduled window when
    /// scheduled, or always on for Outlook's `AlwaysEnabled`).
    pub fn is_out_of_office(&self, now: Timestamp) -> bool {
        match &self.outlook {
            Some(outlook) => match outlook.status {
                AutoReplyStatus::Disabled => false,
                AutoReplyStatus::AlwaysEnabled => self.is_enabled(),
                AutoReplyStatus::Scheduled => {
                    let now = now.as_millisecond();
                    self.is_enabled()
                        && self.start_time.is_some_and(|start| start <= now)
                        && self.end_time.is_none_or(|end| end > now)
                }
            },
            None => {
                let now = now.as_millisecond();
                self.is_enabled()
                    && self.start_time.is_some_and(|start| start < now)
                    && self.end_time.is_none_or(|end| end > now)
            }
        }
    }
}

/// Whether Outlook's `settings` differ from `copy`. Default disabled settings don't differ from a
/// missing or default copy.
fn outlook_differs(settings: &OutlookAutoReply, copy: Option<&OutOfOfficeResponder>) -> bool {
    let empty = *settings == OutlookAutoReply::default();
    if empty && copy.is_none_or(|copy| *copy == OutOfOfficeResponder::default()) {
        return false;
    }
    let Some(copy) = copy else {
        return true;
    };
    let Some(outlook) = &copy.outlook else {
        return true;
    };
    let (start_time, end_time) = if settings.status == AutoReplyStatus::Scheduled {
        (settings.start_time, settings.end_time)
    } else {
        (None, None)
    };
    copy.start_time != start_time
        || copy.end_time != end_time
        || outlook.status != settings.status
        || outlook.external_audience != settings.external_audience
        || outlook.internal_reply_html != settings.internal_reply_html
        || outlook.external_reply_html != settings.external_reply_html
        || (settings.status != AutoReplyStatus::Disabled)
            != !outlook.enabled_auto_reply_at.is_empty()
}

/// Whether Gmail's `settings` differ from `copy`. Empty settings don't differ from a missing or
/// empty copy.
fn differs(settings: &VacationSettings, copy: Option<&OutOfOfficeResponder>) -> bool {
    let empty = *settings == VacationSettings::default();
    if empty && copy.is_none_or(|copy| *copy == OutOfOfficeResponder::default()) {
        return false;
    }
    let google = copy.map(|copy| &copy.google);
    let plain = settings
        .response_body_plain_text
        .as_deref()
        .filter(|plain| !plain.is_empty());
    copy.and_then(|copy| copy.start_time) != settings.start_time
        || copy.and_then(|copy| copy.end_time) != settings.end_time
        || copy.is_some_and(OutOfOfficeResponder::is_enabled)
            != (settings.enable_auto_reply == Some(true))
        || (plain.is_none()
            && google.map_or("", |google| google.response_body_html.as_str())
                != settings.response_body_html.as_deref().unwrap_or(""))
        || google.map_or("", |google| google.response_subject.as_str())
            != settings.response_subject.as_deref().unwrap_or("")
        || google.is_some_and(|google| google.restrict_to_contacts)
            != (settings.restrict_to_contacts == Some(true))
        || google.is_some_and(|google| google.restrict_to_domain)
            != (settings.restrict_to_domain == Some(true))
        || plain.is_some_and(|plain| google.is_none_or(|google| plain != google.response_body_html))
}

/// The user's out-of-office events coming up, and whether they asked not to be offered a reply
/// for them (`outOfOfficeEventDetectionV2`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutOfOfficeEventDetection {
    /// `events`, by event id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub events: BTreeMap<String, DetectedEvent>,
    /// When `Don't show this again` was checked, as an ISO timestamp (`optedOutAt`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opted_out_at: Option<String>,
}

/// `outOfOfficeEventDetectionV2.events[id]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DetectedEvent {
    /// The event's id (`id`).
    pub id: String,
    /// `startTime`.
    pub start_time: DetectedEventTime,
    /// `endTime`.
    pub end_time: DetectedEventTime,
    /// When the nudge was shown for the event, as an ISO timestamp (`nudgedAt`).
    pub nudged_at: Option<String>,
}

/// An out-of-office event's persisted start or end in `outOfOfficeEventDetectionV2`, as a `date`
/// for an all-day event (`2026-11-26`) or a `datetime` (an ISO timestamp) for the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedEventTime {
    /// `date`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// `datetime`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datetime: Option<String>,
}

impl DetectedEventTime {
    fn of(time: &EventTime) -> Self {
        match time {
            EventTime::AllDay(date) => Self {
                date: Some(date.to_string()),
                datetime: None,
            },
            EventTime::Timed { at, .. } => Self {
                date: None,
                datetime: Some(at.to_string()),
            },
        }
    }

    /// The instant it starts at in `tz`, which for a date is its local midnight.
    fn start(&self, tz: &TimeZone) -> Option<Timestamp> {
        match (&self.datetime, &self.date) {
            (Some(at), _) => at.parse().ok(),
            (None, Some(date)) => date
                .parse::<Date>()
                .ok()?
                .to_zoned(tz.clone())
                .ok()
                .map(|zoned| zoned.timestamp()),
            (None, None) => None,
        }
    }

    /// The last instant it covers as an end in `tz`. For a timed end on a local midnight, that's
    /// the second before. For an all-day end, which is exclusive, it's the last millisecond of
    /// the day before.
    fn inclusive_end(&self, tz: &TimeZone) -> Option<Timestamp> {
        match (&self.datetime, &self.date) {
            (Some(at), _) => {
                let at: Timestamp = at.parse().ok()?;
                let local = at.to_zoned(tz.clone());
                Some(if local.hour() == 0 && local.minute() == 0 {
                    at - SignedDuration::from_secs(1)
                } else {
                    at
                })
            }
            (None, Some(date)) => Some(
                date.parse::<Date>()
                    .ok()?
                    .to_zoned(tz.clone())
                    .ok()?
                    .timestamp()
                    - SignedDuration::from_millis(1),
            ),
            (None, None) => None,
        }
    }

    /// The instant pruning compares with now, which is `datetime`, or else UTC midnight on
    /// `date`.
    fn as_written(&self) -> Option<Timestamp> {
        self.start(&TimeZone::UTC)
    }
}

/// Out-of-office days to offer a reply for, with consecutive events merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutOfOfficeDays {
    /// When the first event starts.
    pub start: Timestamp,
    /// The last instant the last event covers.
    pub end: Timestamp,
    /// The events' ids.
    pub ids: Vec<String>,
}

impl OutOfOfficeEventDetection {
    /// The events to keep for `me` at `now`, after Calendar listed `events` (the user's primary
    /// calendar, from now to [`OUT_OF_OFFICE_LOOKAHEAD`] ahead). They're the user's out-of-office
    /// events, with when each was offered, plus those offered that Calendar no longer lists.
    /// Events offered over a week ago are left out, and so are those not offered that have
    /// already started. `None` when that's what's kept already, or after opting out.
    pub fn detected(
        &self,
        events: &[CalendarEvent],
        me: &str,
        now: Timestamp,
    ) -> Option<BTreeMap<String, DetectedEvent>> {
        if self.opted_out_at.is_some() {
            return None;
        }
        let mut found = BTreeMap::new();
        for event in events {
            let Some(id) = event.id.clone().filter(|id| !id.is_empty()) else {
                continue;
            };
            let stored = self.events.get(&id);
            // An event kept that isn't the user's out-of-office event any more is dropped.
            if !is_owners_out_of_office(event, me) || event.status == EventStatus::Cancelled {
                continue;
            }
            let (Some(start), Some(end)) = (
                event.start.as_ref().map(DetectedEventTime::of),
                event.end.as_ref().map(DetectedEventTime::of),
            ) else {
                continue;
            };
            let nudged_at = stored.and_then(|stored| stored.nudged_at.clone());
            found.insert(
                id.clone(),
                DetectedEvent {
                    id,
                    start_time: start,
                    end_time: end,
                    nudged_at,
                },
            );
        }
        for (id, stored) in &self.events {
            if stored.nudged_at.is_some() && !found.contains_key(id) {
                found.insert(id.clone(), stored.clone());
            }
        }
        if found == self.events {
            return None;
        }
        let week_ago = now - SignedDuration::from_hours(7 * 24);
        found.retain(|_, event| match &event.nudged_at {
            Some(at) => at.parse::<Timestamp>().is_ok_and(|at| at >= week_ago),
            None => event
                .start_time
                .as_written()
                .is_some_and(|start| start >= now),
        });
        Some(found)
    }

    /// The days to offer a reply for at `now`, in its time zone, if any. They begin with the
    /// earliest event that is yet to start and not offered, if it starts by the end of the seventh
    /// day from today. Each following event is merged in while it starts at most a day after the
    /// merged days end, or on the Monday after a Friday they end.
    pub fn upcoming(&self, now: &Zoned) -> Option<OutOfOfficeDays> {
        let tz = now.time_zone();
        let mut candidates: Vec<(Timestamp, Timestamp, &str)> = self
            .events
            .values()
            .filter(|event| event.nudged_at.is_none())
            .filter_map(|event| {
                Some((
                    event.start_time.start(tz)?,
                    event.end_time.inclusive_end(tz)?,
                    event.id.as_str(),
                ))
            })
            .filter(|(start, ..)| *start > now.timestamp())
            .collect();
        candidates.sort_by_key(|(start, ..)| *start);
        let window = now
            .date()
            .checked_add(8.days())
            .ok()?
            .to_zoned(tz.clone())
            .ok()?
            .timestamp()
            - SignedDuration::from_millis(1);
        let mut days: Option<OutOfOfficeDays> = None;
        for (start, end, id) in candidates {
            match &mut days {
                None if start > window => break,
                None => {
                    days = Some(OutOfOfficeDays {
                        start,
                        end,
                        ids: vec![id.to_owned()],
                    })
                }
                Some(days) => {
                    let (last, next) = (
                        days.end.to_zoned(tz.clone()).date(),
                        start.to_zoned(tz.clone()).date(),
                    );
                    let adjacent = (next - last).get_days() <= 1
                        || (last.weekday() == Weekday::Friday
                            && next.weekday() == Weekday::Monday
                            && start < days.end + SignedDuration::from_hours(4 * 24));
                    if !adjacent {
                        break;
                    }
                    days.end = days.end.max(end);
                    days.ids.push(id.to_owned());
                }
            }
        }
        days
    }

    /// Whether the nudge may show at `now` for `days`. It may when the user hasn't opted out, no
    /// nudge showed in the last day, and no more than one showed in the last week. When it may,
    /// this marks the events in `days` as offered.
    pub fn nudge(&mut self, days: &OutOfOfficeDays, now: Timestamp) -> bool {
        if !self.may_nudge(days, now) {
            return false;
        }
        for id in &days.ids {
            if let Some(event) = self.events.get_mut(id) {
                event.nudged_at = Some(now.to_string());
            }
        }
        true
    }

    /// [`nudge`](Self::nudge) for an out-of-office event the user just made in Exo, which the
    /// calendar hasn't listed yet. The event is kept, marked as offered at `now`.
    pub fn nudge_created(&mut self, days: &OutOfOfficeDays, now: Timestamp) -> bool {
        if !self.may_nudge(days, now) {
            return false;
        }
        let at = |instant: Timestamp| DetectedEventTime {
            date: None,
            datetime: Some(instant.to_string()),
        };
        for id in &days.ids {
            self.events.insert(
                id.clone(),
                DetectedEvent {
                    id: id.clone(),
                    start_time: at(days.start),
                    end_time: at(days.end),
                    nudged_at: Some(now.to_string()),
                },
            );
        }
        true
    }

    fn may_nudge(&self, days: &OutOfOfficeDays, now: Timestamp) -> bool {
        if self.opted_out_at.is_some() || days.ids.is_empty() {
            return false;
        }
        let nudged: Vec<Timestamp> = self
            .events
            .values()
            .filter_map(|event| event.nudged_at.as_deref()?.parse().ok())
            .collect();
        let since = |hours: i64| {
            nudged
                .iter()
                .filter(|at| **at > now - SignedDuration::from_hours(hours))
                .count()
        };
        since(24) == 0 && since(7 * 24) <= 1
    }
}

impl OutOfOfficeDays {
    /// The out-of-office days of `event`, which `me` just made, if it starts within a week of
    /// `now`. They run from its start to its end, and the end moves a second earlier when it's a
    /// local midnight.
    pub fn created(event: &CalendarEvent, me: &str, now: &Zoned) -> Option<Self> {
        if !is_owners_out_of_office(event, me) {
            return None;
        }
        let id = event.id.clone().filter(|id| !id.is_empty())?;
        let tz = now.time_zone();
        let start = DetectedEventTime::of(event.start.as_ref()?).start(tz)?;
        let end = DetectedEventTime::of(event.end.as_ref()?);
        let end = match end.datetime {
            Some(_) => end.inclusive_end(tz)?,
            // An all-day event ends at the local midnight after its last day.
            None => end.start(tz)? - SignedDuration::from_secs(1),
        };
        (start < now.timestamp() + SignedDuration::from_hours(7 * 24)).then(|| Self {
            start,
            end,
            ids: vec![id],
        })
    }
}

/// Whether `event` is the user's own out-of-office event. That means the user organizes it, it
/// doesn't recur, it's at least 8 hours long, and it's out of office — either marked out of
/// office by the provider (`event_type == OutOfOffice` on Google or `show_as == OutOfOffice` on
/// Outlook) or with a summary that says so. (Whether an invitee accepted doesn't come into it,
/// since the organizer is the user.)
fn is_owners_out_of_office(event: &CalendarEvent, me: &str) -> bool {
    let organizes = event.organizer.is_none() || event.is_me_organizer(me);
    if !organizes || event.is_recurring() || event.status == EventStatus::Cancelled {
        return false;
    }
    let tz = TimeZone::UTC;
    let (Some(start), Some(end)) = (
        event.start.as_ref().and_then(|time| time.timestamp(&tz)),
        event.end.as_ref().and_then(|time| time.timestamp(&tz)),
    ) else {
        return false;
    };
    if end.duration_since(start) < LONG_EVENT {
        return false;
    }
    let summary = event.summary.as_deref().unwrap_or_default().to_lowercase();
    event.is_out_of_office() || WORDS.iter().any(|word| has_word(&summary, word))
}

/// Whether `text` has `word` between word boundaries (`\b{word}\b`, ASCII word characters).
fn has_word(text: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        before.is_none_or(|c| !is_word(c)) && after.is_none_or(|c| !is_word(c))
    })
}

impl Store {
    /// The account's automatic-reply settings from either Gmail (`users.settings.vacation`) or
    /// Outlook (`mailboxSettings.automaticRepliesSetting`).
    ///
    /// # Errors
    /// [`crate::Error::Offline`] without a network; [`crate::Error::Gmail`] or
    /// [`crate::Error::Outlook`] when the provider refuses.
    pub async fn auto_reply(&self) -> Result<AutoReply> {
        self.inner.mail.auto_reply().await
    }

    /// Updates the account's automatic-reply settings on Gmail or Outlook and returns what the
    /// provider kept.
    ///
    /// # Errors
    /// As [`Store::auto_reply`], or [`crate::Error::Unsupported`] if the [`AutoReply`] variant
    /// doesn't match this account's provider.
    pub async fn update_auto_reply(&self, reply: &AutoReply) -> Result<AutoReply> {
        self.inner.mail.update_auto_reply(reply).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::model::{EventPerson, EventType, ShowAs};

    const ME: &str = "maya@brightmoor.co";

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a timestamp")
    }

    fn now() -> Zoned {
        "2026-11-17T09:00:00-05:00[America/New_York]"
            .parse()
            .expect("a time")
    }

    fn vacation(enabled: bool) -> VacationSettings {
        VacationSettings {
            enable_auto_reply: Some(enabled),
            response_subject: Some("Out of office".into()),
            response_body_html: Some("<div>Back Monday.</div>".into()),
            restrict_to_contacts: Some(false),
            restrict_to_domain: Some(false),
            start_time: Some(at("2026-11-17T05:00:00Z").as_millisecond()),
            end_time: Some(at("2026-11-21T05:00:00Z").as_millisecond()),
            response_body_plain_text: None,
        }
    }

    #[test]
    fn the_copy_is_out_of_office_between_its_days_while_on() {
        let copy = OutOfOfficeResponder::of(&vacation(true), now().timestamp());
        assert!(copy.is_enabled());
        assert!(copy.is_out_of_office(now().timestamp()));
        assert!(
            !copy.is_out_of_office(at("2026-11-17T04:00:00Z")),
            "before the first day"
        );
        assert!(
            !copy.is_out_of_office(at("2026-11-21T05:00:00Z")),
            "the end is exclusive"
        );
        let open = OutOfOfficeResponder {
            end_time: None,
            ..copy.clone()
        };
        assert!(
            open.is_out_of_office(at("2027-06-01T00:00:00Z")),
            "no last day"
        );
        assert!(
            !OutOfOfficeResponder::of(&vacation(false), now().timestamp())
                .is_out_of_office(now().timestamp())
        );
    }

    #[test]
    fn reading_gmail_rewrites_the_copy_only_when_it_changed() {
        let now = now().timestamp();
        let gmail = vacation(true);
        let copy = OutOfOfficeResponder::synced(&gmail, None, now).expect("nothing kept yet");
        assert_eq!(copy.google.enabled_auto_reply_at, now.to_string());
        assert_eq!(OutOfOfficeResponder::synced(&gmail, Some(&copy), now), None);
        let off = VacationSettings {
            enable_auto_reply: Some(false),
            ..gmail.clone()
        };
        assert_eq!(
            OutOfOfficeResponder::synced(&off, Some(&copy), now).map(|copy| copy.is_enabled()),
            Some(false)
        );
        let moved = VacationSettings {
            end_time: None,
            ..gmail.clone()
        };
        assert_eq!(
            OutOfOfficeResponder::synced(&moved, Some(&copy), now).and_then(|copy| copy.end_time),
            None
        );
        assert_eq!(
            OutOfOfficeResponder::synced(&VacationSettings::default(), None, now),
            None,
            "nothing, and nothing kept"
        );

        // Omitted (`None`) subject or HTML body from Gmail normalizes to `""` and does not
        // trigger spurious rewrites on subsequent syncs.
        let no_subject = VacationSettings {
            response_subject: None,
            ..gmail.clone()
        };
        let copy_no_subject =
            OutOfOfficeResponder::synced(&no_subject, None, now).expect("initial sync");
        assert_eq!(copy_no_subject.google.response_subject, "");
        assert_eq!(
            OutOfOfficeResponder::synced(&no_subject, Some(&copy_no_subject), now),
            None,
            "None subject matches stored empty string"
        );

        let no_body = VacationSettings {
            response_body_html: None,
            response_body_plain_text: None,
            ..gmail
        };
        let copy_no_body = OutOfOfficeResponder::synced(&no_body, None, now).expect("initial sync");
        assert_eq!(copy_no_body.google.response_body_html, "");
        assert_eq!(
            OutOfOfficeResponder::synced(&no_body, Some(&copy_no_body), now),
            None,
            "None HTML body matches stored empty string"
        );
    }

    #[test]
    fn a_plain_text_body_stands_in_for_the_html_one() {
        let now = now().timestamp();
        let plain = VacationSettings {
            response_body_html: None,
            response_body_plain_text: Some("Back Monday.\n\nMaya".into()),
            ..vacation(false)
        };
        let copy = OutOfOfficeResponder::synced(&plain, None, now).expect("new");
        assert_eq!(copy.google.response_body_html, "Back Monday.\n\nMaya");
        assert_eq!(
            OutOfOfficeResponder::synced(&plain, Some(&copy), now),
            None,
            "the same text again"
        );
    }

    #[test]
    fn outlook_auto_reply_syncs_cleanly_and_gmail_json_stays_backward_compatible() {
        let now = now().timestamp();
        let gmail_copy = OutOfOfficeResponder::of(&vacation(true), now);
        let json = serde_json::to_value(&gmail_copy).expect("serializes");
        assert!(
            json.get("outlook").is_none(),
            "Gmail copy omits outlook field: {json}"
        );
        let decoded: OutOfOfficeResponder = serde_json::from_value(json).expect("deserializes");
        assert_eq!(decoded, gmail_copy);

        let start = at("2026-11-17T05:00:00Z").as_millisecond();
        let end = at("2026-11-21T05:00:00Z").as_millisecond();
        let scheduled = AutoReply::Outlook(OutlookAutoReply {
            status: AutoReplyStatus::Scheduled,
            external_audience: ExternalAudience::ContactsOnly,
            internal_reply_html: "<p>Internal OOO</p>".into(),
            external_reply_html: "<p>External OOO</p>".into(),
            start_time: Some(start),
            end_time: Some(end),
        });
        let copy = OutOfOfficeResponder::sync_auto_reply(&scheduled, None, now).expect("new copy");
        assert!(copy.is_enabled());
        assert!(copy.is_out_of_office(now));
        assert!(!copy.is_out_of_office(at("2026-11-17T04:00:00Z")));
        assert!(!copy.is_out_of_office(at("2026-11-21T05:00:00Z")));
        assert_eq!(
            OutOfOfficeResponder::sync_auto_reply(&scheduled, Some(&copy), now),
            None,
            "unchanged poll does not rewrite"
        );

        let always = AutoReply::Outlook(OutlookAutoReply {
            status: AutoReplyStatus::AlwaysEnabled,
            external_audience: ExternalAudience::All,
            internal_reply_html: "<p>Always on</p>".into(),
            external_reply_html: "<p>Always on</p>".into(),
            start_time: Some(start),
            end_time: Some(end),
        });
        let always_copy = OutOfOfficeResponder::from_auto_reply(&always, now);
        assert!(always_copy.is_enabled());
        assert!(always_copy.is_out_of_office(at("2030-01-01T00:00:00Z")));
        assert_eq!(always_copy.start_time, None);
        assert_eq!(always_copy.end_time, None);

        let disabled = AutoReply::Outlook(OutlookAutoReply::default());
        assert_eq!(
            OutOfOfficeResponder::sync_auto_reply(&disabled, None, now),
            None
        );
        let turned_off =
            OutOfOfficeResponder::sync_auto_reply(&disabled, Some(&always_copy), now).expect("off");
        assert!(!turned_off.is_enabled());
        assert!(!turned_off.is_out_of_office(now));
    }

    fn event(
        id: &str,
        summary: &str,
        kind: EventType,
        start: EventTime,
        end: EventTime,
    ) -> CalendarEvent {
        CalendarEvent {
            id: Some(id.into()),
            summary: Some(summary.into()),
            event_type: kind,
            start: Some(start),
            end: Some(end),
            ..CalendarEvent::default()
        }
    }

    fn day(date: &str) -> EventTime {
        EventTime::AllDay(date.parse().expect("date"))
    }

    fn timed(text: &str) -> EventTime {
        EventTime::Timed {
            at: at(text),
            time_zone: None,
        }
    }

    #[test]
    fn the_owners_long_out_of_office_events_are_found() {
        let now = now().timestamp();
        let events = [
            event(
                "ooo",
                "Out of office",
                EventType::OutOfOffice,
                timed("2026-11-19T00:00:00-05:00"),
                timed("2026-11-21T00:00:00-05:00"),
            ),
            CalendarEvent {
                show_as: ShowAs::OutOfOffice,
                ..event(
                    "outlook-oof",
                    "Blocked",
                    EventType::Default,
                    day("2026-11-22"),
                    day("2026-11-23"),
                )
            },
            event(
                "pto",
                "PTO (Lisbon)",
                EventType::Default,
                day("2026-11-24"),
                day("2026-11-25"),
            ),
            event(
                "short",
                "Vacation planning",
                EventType::Default,
                timed("2026-11-18T10:00:00-05:00"),
                timed("2026-11-18T11:00:00-05:00"),
            ),
            event(
                "photo",
                "Photoshoot",
                EventType::Default,
                day("2026-11-24"),
                day("2026-11-25"),
            ),
            CalendarEvent {
                recurring_event_id: Some("weekly".into()),
                ..event(
                    "recurring",
                    "OOO",
                    EventType::Default,
                    day("2026-11-26"),
                    day("2026-11-27"),
                )
            },
            CalendarEvent {
                organizer: Some(EventPerson {
                    email: Some("sam@brightmoor.co".into()),
                    ..EventPerson::default()
                }),
                ..event(
                    "invited",
                    "Team offsite (out of office)",
                    EventType::Default,
                    day("2026-11-27"),
                    day("2026-11-28"),
                )
            },
        ];
        let detection = OutOfOfficeEventDetection::default();
        let found = detection.detected(&events, ME, now).expect("new events");
        assert_eq!(
            found.keys().map(String::as_str).collect::<Vec<_>>(),
            ["ooo", "outlook-oof", "pto"]
        );
        assert_eq!(
            found["ooo"].start_time.datetime.as_deref(),
            Some("2026-11-19T05:00:00Z")
        );
        assert_eq!(
            found["outlook-oof"].start_time.date.as_deref(),
            Some("2026-11-22")
        );
        assert_eq!(found["pto"].start_time.date.as_deref(), Some("2026-11-24"));
        let kept = OutOfOfficeEventDetection {
            events: found,
            opted_out_at: None,
        };
        assert_eq!(kept.detected(&events, ME, now), None, "nothing new");
        let opted_out = OutOfOfficeEventDetection {
            opted_out_at: Some(now.to_string()),
            ..OutOfOfficeEventDetection::default()
        };
        assert_eq!(opted_out.detected(&events, ME, now), None);
    }

    #[test]
    fn events_offered_stay_a_week_and_the_rest_until_they_start() {
        let now = now().timestamp();
        let offered = |id: &str, at: &str| DetectedEvent {
            id: id.into(),
            start_time: date("2026-11-10"),
            nudged_at: Some(at.into()),
            ..DetectedEvent::default()
        };
        let events = BTreeMap::from([
            (
                "recent".to_owned(),
                offered("recent", "2026-11-15T12:00:00Z"),
            ),
            ("old".to_owned(), offered("old", "2026-11-01T12:00:00Z")),
        ]);
        let detection = OutOfOfficeEventDetection {
            events,
            opted_out_at: None,
        };
        // What's kept is cleaned only when something else changed.
        assert_eq!(detection.detected(&[], ME, now), None);
        let new = [event(
            "pto",
            "PTO",
            EventType::Default,
            day("2026-11-24"),
            day("2026-11-25"),
        )];
        let kept = detection.detected(&new, ME, now).expect("a new event");
        assert_eq!(
            kept.keys().map(String::as_str).collect::<Vec<_>>(),
            ["pto", "recent"],
            "the old one goes"
        );
    }

    fn stored(
        id: &str,
        start: DetectedEventTime,
        end: DetectedEventTime,
    ) -> (String, DetectedEvent) {
        (
            id.to_owned(),
            DetectedEvent {
                id: id.into(),
                start_time: start,
                end_time: end,
                nudged_at: None,
            },
        )
    }

    fn date(text: &str) -> DetectedEventTime {
        DetectedEventTime {
            date: Some(text.into()),
            datetime: None,
        }
    }

    #[test]
    fn consecutive_days_and_a_weekend_merge_into_one_offer() {
        let now = now();
        let detection = OutOfOfficeEventDetection {
            events: [
                stored("thu", date("2026-11-19"), date("2026-11-20")),
                stored("fri", date("2026-11-20"), date("2026-11-21")),
                stored("mon", date("2026-11-23"), date("2026-11-24")),
                stored("later", date("2026-11-26"), date("2026-11-27")),
            ]
            .into(),
            opted_out_at: None,
        };
        let days = detection.upcoming(&now).expect("days this week");
        assert_eq!(days.ids, ["thu", "fri", "mon"]);
        assert_eq!(days.start, at("2026-11-19T05:00:00Z"));
        assert_eq!(
            days.end,
            at("2026-11-24T04:59:59.999Z"),
            "the Monday's last millisecond"
        );
    }

    #[test]
    fn only_days_starting_within_a_week_are_offered() {
        let now = now();
        let next_week = OutOfOfficeEventDetection {
            events: [stored("far", date("2026-11-25"), date("2026-11-26"))].into(),
            opted_out_at: None,
        };
        assert_eq!(
            next_week.upcoming(&now),
            None,
            "the 25th is past the 24th's end"
        );
        let in_a_week = OutOfOfficeEventDetection {
            events: [stored("near", date("2026-11-24"), date("2026-11-25"))].into(),
            opted_out_at: None,
        };
        assert!(in_a_week.upcoming(&now).is_some());
        let timed = OutOfOfficeEventDetection {
            events: [stored(
                "ooo",
                DetectedEventTime {
                    date: None,
                    datetime: Some("2026-11-19T05:00:00Z".into()),
                },
                DetectedEventTime {
                    date: None,
                    datetime: Some("2026-11-21T05:00:00Z".into()),
                },
            )]
            .into(),
            opted_out_at: None,
        };
        assert_eq!(
            timed.upcoming(&now).map(|days| days.end),
            Some(at("2026-11-21T04:59:59Z")),
            "a midnight end is the second before"
        );
    }

    #[test]
    fn the_nudge_shows_at_most_once_a_day_and_twice_a_week() {
        let now = now().timestamp();
        let mut detection = OutOfOfficeEventDetection {
            events: [stored("thu", date("2026-11-19"), date("2026-11-20"))].into(),
            opted_out_at: None,
        };
        let days = OutOfOfficeDays {
            start: now,
            end: now,
            ids: vec!["thu".into()],
        };
        assert!(detection.nudge(&days, now));
        assert_eq!(
            detection.events["thu"].nudged_at.as_deref(),
            Some(now.to_string().as_str())
        );
        detection
            .events
            .extend([stored("fri", date("2026-11-20"), date("2026-11-21"))]);
        let friday = OutOfOfficeDays {
            ids: vec!["fri".into()],
            ..days.clone()
        };
        assert!(
            !detection.nudge(&friday, now + SignedDuration::from_hours(12)),
            "within a day"
        );
        assert!(detection.nudge(&friday, now + SignedDuration::from_hours(25)));
        detection
            .events
            .extend([stored("sat", date("2026-11-21"), date("2026-11-22"))]);
        let saturday = OutOfOfficeDays {
            ids: vec!["sat".into()],
            ..days
        };
        assert!(
            !detection.nudge(&saturday, now + SignedDuration::from_hours(50)),
            "two this week"
        );
    }

    #[test]
    fn an_event_made_in_exo_is_offered_when_it_starts_this_week() {
        let now = now();
        let mut vacation = event(
            "new",
            "Vacation",
            EventType::Default,
            day("2026-11-20"),
            day("2026-11-22"),
        );
        let days = OutOfOfficeDays::created(&vacation, ME, &now).expect("this week");
        assert_eq!(
            (days.start, days.end),
            (at("2026-11-20T05:00:00Z"), at("2026-11-22T04:59:59Z"))
        );
        let mut detection = OutOfOfficeEventDetection::default();
        assert!(detection.nudge_created(&days, now.timestamp()));
        assert_eq!(
            detection.events["new"].nudged_at.as_deref(),
            Some(now.timestamp().to_string().as_str())
        );
        vacation.start = Some(day("2026-11-30"));
        vacation.end = Some(day("2026-12-01"));
        assert_eq!(
            OutOfOfficeDays::created(&vacation, ME, &now),
            None,
            "next week"
        );
        assert_eq!(
            OutOfOfficeDays::created(
                &event(
                    "lunch",
                    "Lunch",
                    EventType::Default,
                    day("2026-11-20"),
                    day("2026-11-21")
                ),
                ME,
                &now
            ),
            None
        );
    }

    #[test]
    fn summary_words_match_whole_words() {
        assert!(has_word("pto (lisbon)", "pto"));
        assert!(has_word("out-of-office", "out-of-office"));
        assert!(!has_word("photo shoot", "pto"));
        assert!(!has_word("ooops", "ooo"));
    }
}
