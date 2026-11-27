//! Provider-neutral calendar domain types shared across Google Calendar and Microsoft Outlook.
//!
//! Callers work exclusively with these types; wire conversions live in
//! [`super::google`] and [`super::outlook`].

use std::fmt;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::recurrence::Recurrence;

/// Prefix used in canonical account keys for Microsoft accounts (`"microsoft {email}"`).
pub(crate) const MICROSOFT_KEY_PREFIX: &str = "microsoft ";

/// Calendar service backing an account or calendar.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum CalendarProviderKind {
    /// Google Calendar API v3.
    #[default]
    Google,
    /// Microsoft Outlook / Microsoft 365 via Graph v1.0.
    Microsoft,
}

impl CalendarProviderKind {
    /// Lowercase wire name (`"google"` or `"microsoft"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
        }
    }

    /// Canonical account key for `email` under this provider (`email` for Google,
    /// `"microsoft {email}"` for Microsoft), preventing collisions when the same email exists on
    /// both providers.
    #[must_use]
    pub fn account_key(self, email: &str) -> String {
        match self {
            Self::Google => email.to_owned(),
            Self::Microsoft => format!("{MICROSOFT_KEY_PREFIX}{email}"),
        }
    }

    /// Splits a canonical account key into `(provider, display_email)`.
    #[must_use]
    pub fn parse_account_key(key: &str) -> (Self, &str) {
        match key.strip_prefix(MICROSOFT_KEY_PREFIX) {
            Some(email) => (Self::Microsoft, email),
            None => (Self::Google, key),
        }
    }
}

impl fmt::Display for CalendarProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The user's level of access to a [`Calendar`].
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum AccessRole {
    /// Full ownership of the calendar.
    Owner,
    /// Can create, edit, and delete events.
    Writer,
    /// Read-only access to event details.
    #[default]
    Reader,
    /// Free/busy visibility only.
    FreeBusyReader,
}

impl AccessRole {
    /// Whether the user can write events to a calendar with this role.
    #[must_use]
    pub const fn is_writable(self) -> bool {
        matches!(self, Self::Owner | Self::Writer)
    }

    /// Sort priority for choosing which account reads a calendar listed by more than one account
    /// (lower wins).
    pub(crate) const fn rank(self) -> u8 {
        match self {
            Self::Owner => 1,
            Self::Writer => 2,
            Self::Reader => 3,
            Self::FreeBusyReader => 4,
        }
    }
}

/// Online meeting provider supported for creating conference links on a calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OnlineMeetingKind {
    /// Google Meet (`hangoutsMeet`).
    GoogleMeet,
    /// Microsoft Teams for work or school accounts (`teamsForBusiness`).
    TeamsForBusiness,
    /// Skype for consumer Outlook.com accounts (`skypeForConsumer`).
    SkypeForConsumer,
}

/// A calendar on an account's list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    /// Provider calendar ID (email for Google primary calendars; opaque ID on Graph).
    pub id: String,
    /// Title reported by the provider.
    pub summary: String,
    /// User-chosen override title (Google `summaryOverride`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_override: Option<String>,
    /// Whether this is the account's primary / default calendar.
    #[serde(default)]
    pub primary: bool,
    /// Access level on this calendar.
    #[serde(default)]
    pub access_role: AccessRole,
    /// Whether private event details are visible on this calendar.
    #[serde(default = "default_true")]
    pub can_view_private_items: bool,
    /// Hex background color (`#rrggbb`), when provided by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Named provider color preset (such as Outlook's `lightBlue` when `hexColor` is empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_name: Option<String>,
    /// Calendar's IANA time zone name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Hidden from the provider's calendar list.
    #[serde(default)]
    pub hidden: bool,
    /// Selected (shown) by default in the provider's UI.
    #[serde(default)]
    pub selected: bool,
    /// Marked deleted by the provider.
    #[serde(default)]
    pub deleted: bool,
    /// Calendar service that owns this calendar.
    #[serde(default)]
    pub provider: CalendarProviderKind,
    /// Online meeting providers allowed when creating events on this calendar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_online_meetings: Vec<OnlineMeetingKind>,
    /// Default online meeting provider for this calendar, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_online_meeting: Option<OnlineMeetingKind>,
}

const fn default_true() -> bool {
    true
}

impl Calendar {
    /// Best human-readable title: `summary_override`, then `summary`, then `id`.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.summary_override
            .as_deref()
            .filter(|name| !name.is_empty())
            .or_else(|| (!self.summary.is_empty()).then_some(self.summary.as_str()))
            .unwrap_or(&self.id)
    }

    /// Whether events on this calendar can be created, edited, or deleted.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.access_role.is_writable()
    }

    /// Whether this calendar supports creating Microsoft Teams online meetings.
    #[must_use]
    pub fn supports_teams(&self) -> bool {
        self.allowed_online_meetings
            .contains(&OnlineMeetingKind::TeamsForBusiness)
    }

    /// Whether this calendar supports creating Google Meet conferences.
    #[must_use]
    pub fn supports_meet(&self) -> bool {
        self.allowed_online_meetings
            .contains(&OnlineMeetingKind::GoogleMeet)
    }

    /// Whether this calendar should appear in user-facing calendar lists (excludes deleted,
    /// weather, and room resource calendars).
    #[must_use]
    pub fn is_listable(&self) -> bool {
        !self.deleted
            && !self.id.contains("weather@group.v.calendar.google.com")
            && !self.id.contains("@resource.calendar.google.com")
    }
}

/// A calendar on a calendar account's list, paired with the account that owns/reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCalendar {
    /// Canonical account key (`email` for Google, `"microsoft {email}"` for Microsoft).
    pub account: String,
    /// Display email address of the account.
    pub email: String,
    /// Calendar provider backing this account.
    pub provider: CalendarProviderKind,
    /// The calendar's metadata.
    pub calendar: Calendar,
}

/// An event boundary: either a date-only all-day date or a timed instant with optional IANA zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventTime {
    /// Date-only boundary (`YYYY-MM-DD`).
    AllDay(Date),
    /// Timed instant with optional IANA time zone name.
    Timed {
        /// The instant in time.
        at: Timestamp,
        /// Optional IANA (or Windows) time zone name the event was written in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time_zone: Option<String>,
    },
}

impl EventTime {
    /// Constructs a date-only all-day boundary.
    #[must_use]
    pub const fn all_day(date: Date) -> Self {
        Self::AllDay(date)
    }

    /// Constructs a timed boundary.
    #[must_use]
    pub const fn timed(at: Timestamp, time_zone: Option<String>) -> Self {
        Self::Timed { at, time_zone }
    }

    /// Constructs an [`EventTime`] from `zoned`, as all-day or timed.
    #[must_use]
    pub fn from_zoned(zoned: &Zoned, all_day: bool) -> Self {
        if all_day {
            Self::AllDay(zoned.date())
        } else {
            Self::Timed {
                at: zoned.timestamp(),
                time_zone: zoned.time_zone().iana_name().map(str::to_owned),
            }
        }
    }

    /// Whether this boundary is a date-only all-day value.
    #[must_use]
    pub const fn is_all_day(&self) -> bool {
        matches!(self, Self::AllDay(_))
    }

    /// The civil date when this is [`EventTime::AllDay`].
    #[must_use]
    pub const fn date(&self) -> Option<Date> {
        match self {
            Self::AllDay(date) => Some(*date),
            Self::Timed { .. } => None,
        }
    }

    /// The named time zone when this is [`EventTime::Timed`].
    #[must_use]
    pub fn time_zone(&self) -> Option<&str> {
        match self {
            Self::AllDay(_) => None,
            Self::Timed { time_zone, .. } => time_zone.as_deref(),
        }
    }

    /// Resolves this boundary's own time zone, falling back to `fallback`.
    #[must_use]
    pub fn resolve_zone(&self, fallback: &TimeZone) -> TimeZone {
        match self {
            Self::AllDay(_) => fallback.clone(),
            Self::Timed { time_zone, .. } => time_zone
                .as_deref()
                .and_then(gmail::ics::resolve_timezone)
                .unwrap_or_else(|| fallback.clone()),
        }
    }

    /// The instant this denotes: local midnight in `tz` for all-day dates, or the exact instant
    /// for timed boundaries.
    #[must_use]
    pub fn timestamp(&self, tz: &TimeZone) -> Option<Timestamp> {
        match self {
            Self::AllDay(date) => date.to_zoned(tz.clone()).ok().map(|z| z.timestamp()),
            Self::Timed { at, .. } => Some(*at),
        }
    }

    /// This boundary expressed in viewer time zone `tz`.
    #[must_use]
    pub fn zoned(&self, tz: &TimeZone) -> Option<Zoned> {
        match self {
            Self::AllDay(date) => date.to_zoned(tz.clone()).ok(),
            Self::Timed { at, .. } => Some(at.to_zoned(tz.clone())),
        }
    }

    /// This boundary expressed in its own `time_zone` if valid, else `fallback`.
    #[must_use]
    pub fn own_zoned(&self, fallback: &TimeZone) -> Option<Zoned> {
        let tz = self.resolve_zone(fallback);
        self.zoned(&tz)
    }

    /// Civil wall-clock [`DateTime`] in the boundary's own time zone (falling back to `tz`).
    #[must_use]
    pub fn civil(&self, tz: &TimeZone) -> Option<DateTime> {
        match self {
            Self::AllDay(date) => Some(date.at(0, 0, 0, 0)),
            Self::Timed { at, .. } => Some(at.to_zoned(self.resolve_zone(tz)).datetime()),
        }
    }

    /// Shifts this boundary by civil wall-clock delta `by`, preserving local wall-clock time
    /// across DST transitions. When `all_day_target` is `Some(flag)`, converts to all-day or
    /// timed; `zone` overrides or supplies the target time zone for timed results.
    #[must_use]
    pub fn shift_wall_clock(
        &self,
        by: SignedDuration,
        all_day_target: Option<bool>,
        zone: Option<&str>,
    ) -> Option<Self> {
        let fallback_tz = zone
            .and_then(gmail::ics::resolve_timezone)
            .unwrap_or(TimeZone::UTC);
        let wall = self.civil(&fallback_tz)?.checked_add(by).ok()?;
        let target_all_day = all_day_target.unwrap_or_else(|| self.is_all_day());
        if target_all_day {
            return Some(Self::AllDay(wall.date()));
        }
        let target_zone_name = zone.or_else(|| self.time_zone());
        let target_tz = target_zone_name
            .and_then(gmail::ics::resolve_timezone)
            .unwrap_or(TimeZone::UTC);
        let zoned = wall.to_zoned(target_tz).ok()?;
        Some(Self::Timed {
            at: zoned.timestamp(),
            time_zone: target_zone_name.map(str::to_owned),
        })
    }

    /// Epoch milliseconds used for SQLite range indexing, plus whether it is all-day.
    /// All-day dates are stored as UTC midnight.
    #[must_use]
    pub(crate) fn stored_millis(&self) -> (i64, bool) {
        match self {
            Self::AllDay(date) => (
                date.to_zoned(TimeZone::UTC)
                    .map(|z| z.timestamp().as_millisecond())
                    .unwrap_or_default(),
                true,
            ),
            Self::Timed { at, .. } => (at.as_millisecond(), false),
        }
    }
}

/// An attendee's RSVP response status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RsvpStatus {
    /// Has not yet responded.
    #[default]
    NeedsAction,
    /// Accepted.
    Accepted,
    /// Tentatively accepted (Maybe).
    Tentative,
    /// Declined.
    Declined,
}

impl RsvpStatus {
    /// Google Calendar wire value (`needsAction`, `accepted`, `tentative`, `declined`).
    #[must_use]
    pub const fn as_google(self) -> &'static str {
        match self {
            Self::NeedsAction => "needsAction",
            Self::Accepted => "accepted",
            Self::Tentative => "tentative",
            Self::Declined => "declined",
        }
    }

    /// Parses a Google Calendar RSVP string.
    #[must_use]
    pub fn from_google(status: &str) -> Option<Self> {
        match status {
            "needsAction" => Some(Self::NeedsAction),
            "accepted" => Some(Self::Accepted),
            "tentative" => Some(Self::Tentative),
            "declined" => Some(Self::Declined),
            _ => None,
        }
    }
}

/// Lifecycle status of a [`CalendarEvent`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventStatus {
    /// Confirmed event.
    #[default]
    Confirmed,
    /// Tentatively scheduled.
    Tentative,
    /// Cancelled / deleted.
    Cancelled,
}

/// How an event shows on free/busy schedules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShowAs {
    /// Busy (Google `opaque`, Graph `busy`).
    #[default]
    Busy,
    /// Free (Google `transparent`, Graph `free`).
    Free,
    /// Tentative (Graph `tentative`).
    Tentative,
    /// Out of office (Graph `oof`).
    OutOfOffice,
    /// Working elsewhere (Graph `workingElsewhere`).
    WorkingElsewhere,
}

/// Event visibility / sensitivity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Visibility {
    /// Calendar default visibility (Google `default`, Graph `normal`).
    #[default]
    Default,
    /// Publicly visible (Google `public`).
    Public,
    /// Private (Google `private`, Graph `private` / `personal`).
    Private,
    /// Confidential (Google `confidential`, Graph `confidential`).
    Confidential,
}

/// Special event category/type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventType {
    /// Regular calendar event.
    #[default]
    Default,
    /// Out-of-office block.
    OutOfOffice,
    /// Focus time block.
    FocusTime,
    /// Working location entry.
    WorkingLocation,
    /// Birthday entry.
    Birthday,
    /// Automatically extracted from mail (Google `fromGmail`).
    FromGmail,
}

/// Reminder configuration for a [`CalendarEvent`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "minutes", rename_all = "camelCase")]
pub enum EventReminders {
    /// Use the calendar's default reminders.
    #[default]
    Default,
    /// Reminders explicitly disabled.
    None,
    /// Custom reminder lead times in minutes before event start.
    Custom(Vec<u32>),
}

/// Conference video call provider kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConferenceKind {
    /// Google Meet.
    GoogleMeet,
    /// Microsoft Teams.
    Teams,
    /// Zoom.
    Zoom,
    /// Any other conference provider name.
    Other(String),
}

/// Online conference / video meeting details attached to a [`CalendarEvent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conference {
    /// Provider kind (`GoogleMeet`, `Teams`, `Zoom`, or `Other`).
    pub kind: ConferenceKind,
    /// Video join URL, when assigned by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_url: Option<String>,
    /// Short display label (e.g. `meet.google.com/abc-defg-hij`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// True while a newly requested conference is awaiting server assignment.
    #[serde(default)]
    pub pending: bool,
    /// Optional dial-in phone number / URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dial_in: Option<String>,
}

impl Conference {
    /// Optimistic placeholder for a newly requested online meeting before the server assigns a
    /// join URL.
    #[must_use]
    pub fn pending(kind: OnlineMeetingKind) -> Self {
        let kind = match kind {
            OnlineMeetingKind::GoogleMeet => ConferenceKind::GoogleMeet,
            OnlineMeetingKind::TeamsForBusiness => ConferenceKind::Teams,
            OnlineMeetingKind::SkypeForConsumer => ConferenceKind::Other("Skype".to_owned()),
        };
        Self {
            kind,
            join_url: None,
            label: None,
            pending: true,
            dial_in: None,
        }
    }
}

/// Creator or organizer of an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPerson {
    /// Email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Whether this person is the authenticated calendar account.
    #[serde(default)]
    pub is_self: bool,
}

impl EventPerson {
    /// Whether this person represents `me` (either flagged `is_self` or matching `me`
    /// case-insensitively).
    #[must_use]
    pub fn is_me(&self, me: &str) -> bool {
        self.is_self
            || self
                .email
                .as_deref()
                .is_some_and(|email| email.eq_ignore_ascii_case(me))
    }
}

/// An attendee or room resource invited to an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttendee {
    /// Provider profile ID (Google).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Whether this attendee is the event organizer.
    #[serde(default)]
    pub organizer: bool,
    /// Whether this attendee is the authenticated calendar account.
    #[serde(default)]
    pub is_self: bool,
    /// Whether this attendee is a room or equipment resource.
    #[serde(default)]
    pub resource: bool,
    /// Whether attendance is optional.
    #[serde(default)]
    pub optional: bool,
    /// Attendee's RSVP status.
    #[serde(default)]
    pub response_status: RsvpStatus,
    /// Optional RSVP note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Additional guests count (Google).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_guests: Option<u32>,
}

impl EventAttendee {
    /// Whether this attendee represents `me`.
    #[must_use]
    pub fn is_me(&self, me: &str) -> bool {
        self.is_self
            || self
                .email
                .as_deref()
                .is_some_and(|email| email.eq_ignore_ascii_case(me))
    }
}

/// An Outlook event category resolved against the mailbox's master category list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCategory {
    /// Category display name.
    pub name: String,
    /// Outlook color preset index (`0..=24` for `preset0..preset24`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<u8>,
}

/// A file attached to a [`CalendarEvent`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttachment {
    /// URL of the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_url: Option<String>,
    /// Title / filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Icon URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_link: Option<String>,
    /// Provider file ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

/// A busy slot returned by [`crate::Store::free_busy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeBusySlot {
    /// Start of the busy interval.
    pub start: Timestamp,
    /// End of the busy interval.
    pub end: Timestamp,
    /// Free/busy classification (`Busy`, `Tentative`, `OutOfOffice`, etc.).
    #[serde(default)]
    pub status: ShowAs,
}

impl FreeBusySlot {
    /// Convenience `(start, end)` pair.
    #[must_use]
    pub const fn span(self) -> (Timestamp, Timestamp) {
        (self.start, self.end)
    }
}

/// Change to an event's online meeting link in an [`EventPatch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConferencePatch {
    /// Create a new conference link for this provider kind.
    Create(OnlineMeetingKind),
    /// Remove an existing conference link (supported on Google; Graph ignores post-create removal).
    Remove,
}

/// Partial update to a [`CalendarEvent`]. Only `Some` fields are modified.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPatch {
    /// Client-chosen event ID (or Graph `transactionId`) for the new series created by a split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Updated title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Updated start time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<EventTime>,
    /// Updated end time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<EventTime>,
    /// Updated location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Updated description / notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Full replacement attendee list (`Some(vec![])` clears all invitees; `None` leaves
    /// unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<EventAttendee>>,
    /// Conference link addition or removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference: Option<ConferencePatch>,
    /// `None` leaves recurrence unchanged; `Some(None)` clears recurrence; `Some(Some(r))` sets
    /// the recurrence rule.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub recurrence: Option<Option<Recurrence>>,
    /// Raw RFC 5545 recurrence lines (used when preserving non-form Google RRULE/EXDATE lines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_recurrence: Option<Vec<String>>,
    /// Updated Google event color ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_id: Option<String>,
    /// Updated Outlook categories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<EventCategory>>,
    /// Updated free/busy display status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_as: Option<ShowAs>,
    /// Updated visibility / sensitivity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// Updated reminders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<EventReminders>,
    /// Whether guests may invite others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_invite_others: Option<bool>,
    /// Whether guests may modify the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_modify: Option<bool>,
    /// Whether guests may see other guests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_see_other_guests: Option<bool>,
}

/// Provider-neutral calendar event (a single event, series master, or recurring instance).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    /// Event ID (opaque ID on Graph; client/server ID or `{series}_{start}` on Google).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Calendar provider that produced or owns this event.
    #[serde(default)]
    pub provider: CalendarProviderKind,
    /// Lifecycle status (`Confirmed`, `Tentative`, `Cancelled`).
    #[serde(default)]
    pub status: EventStatus,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Description / body (plain text or HTML).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Location text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Start (inclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<EventTime>,
    /// End (exclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<EventTime>,
    /// Who created the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<EventPerson>,
    /// Who organizes the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<EventPerson>,
    /// Invited attendees and resources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<EventAttendee>,
    /// Whether the provider omitted the full attendee list.
    #[serde(default)]
    pub attendees_omitted: bool,
    /// The signed-in user's own RSVP when reported outside `attendees` (Microsoft Graph
    /// `responseStatus`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_response: Option<RsvpStatus>,
    /// Whether guests may modify the event.
    #[serde(default)]
    pub guests_can_modify: bool,
    /// Whether guests may invite others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_invite_others: Option<bool>,
    /// Whether guests may see other guests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_see_other_guests: Option<bool>,
    /// Series master ID for recurring occurrences/exceptions (Google `recurringEventId`, Graph
    /// `seriesMasterId`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring_event_id: Option<String>,
    /// Original scheduled start for a recurring occurrence/exception.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_start: Option<EventTime>,
    /// Parsed recurrence rule for a series master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Recurrence>,
    /// Raw RFC 5545 recurrence lines (Google `RRULE`/`EXDATE` lines, preserved for lossless
    /// round-tripping and non-form rules).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_recurrence: Vec<String>,
    /// iCalendar UID shared across all instances of a series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ical_uid: Option<String>,
    /// Link to open the event in the provider's web calendar (Google `htmlLink`, Graph `webLink`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_link: Option<String>,
    /// Online meeting / video conference details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference: Option<Conference>,
    /// Google event color ID (`"1"`..=`"11"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_id: Option<String>,
    /// Outlook categories assigned to this event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<EventCategory>,
    /// Visibility / sensitivity.
    #[serde(default)]
    pub visibility: Visibility,
    /// Free/busy status shown on the calendar.
    #[serde(default)]
    pub show_as: ShowAs,
    /// Special event type.
    #[serde(default)]
    pub event_type: EventType,
    /// File attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<EventAttachment>,
    /// Reminders configured on this event.
    #[serde(default)]
    pub reminders: EventReminders,
    /// Client-chosen idempotency key for Graph creates (`transactionId`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
}

impl CalendarEvent {
    /// Whether `me` organizes this event.
    #[must_use]
    pub fn is_me_organizer(&self, me: &str) -> bool {
        self.organizer
            .as_ref()
            .is_some_and(|organizer| organizer.is_me(me))
    }

    /// The authenticated user's RSVP status on this event, checking their attendee entry first
    /// and falling back to [`Self::self_response`].
    #[must_use]
    pub fn my_rsvp(&self, me: &str) -> Option<RsvpStatus> {
        self.attendees
            .iter()
            .find(|attendee| attendee.is_self)
            .or_else(|| {
                self.attendees.iter().find(|attendee| {
                    attendee
                        .email
                        .as_deref()
                        .is_some_and(|email| email.eq_ignore_ascii_case(me))
                })
            })
            .map(|attendee| attendee.response_status)
            .or(self.self_response)
    }

    /// Updates the authenticated user's RSVP status in place (both any matching attendee entry and
    /// [`Self::self_response`]).
    pub fn set_my_rsvp(&mut self, me: &str, status: RsvpStatus) {
        let mut matched = false;
        for attendee in &mut self.attendees {
            if attendee.is_me(me) {
                attendee.response_status = status;
                matched = true;
            }
        }
        if !matched || self.self_response.is_some() {
            self.self_response = Some(status);
        }
    }

    /// Whether there is at least one non-resource guest other than `me`.
    #[must_use]
    pub fn has_other_guests(&self, me: &str) -> bool {
        self.attendees
            .iter()
            .any(|attendee| !attendee.resource && !attendee.is_me(me))
    }

    /// Whether this event represents an out-of-office block on either provider (Google
    /// `eventType == "outOfOffice"` or Outlook `showAs == "oof"`).
    #[must_use]
    pub const fn is_out_of_office(&self) -> bool {
        matches!(self.event_type, EventType::OutOfOffice)
            || matches!(self.show_as, ShowAs::OutOfOffice)
    }

    /// Series master ID when this is an occurrence/exception, else the event's own ID.
    #[must_use]
    pub fn series_or_id(&self) -> Option<&str> {
        self.recurring_event_id.as_deref().or(self.id.as_deref())
    }

    /// Whether this event is a recurring series master or an instance of one.
    #[must_use]
    pub fn is_recurring(&self) -> bool {
        self.recurring_event_id.is_some()
            || self.recurrence.is_some()
            || !self.raw_recurrence.is_empty()
    }

    /// Whether this event is a series master (carries its own recurrence rule and no parent).
    #[must_use]
    pub fn is_series_master(&self) -> bool {
        self.recurring_event_id.is_none()
            && (self.recurrence.is_some() || !self.raw_recurrence.is_empty())
    }

    /// Applies `patch` in place, returning the updated event for optimistic cache/UI display and
    /// series splits.
    pub fn apply_patch(&mut self, patch: &EventPatch) {
        if let Some(summary) = &patch.summary {
            self.summary = Some(summary.clone());
        }
        if let Some(start) = &patch.start {
            self.start = Some(start.clone());
        }
        if let Some(end) = &patch.end {
            self.end = Some(end.clone());
        }
        if let Some(location) = &patch.location {
            self.location = Some(location.clone());
        }
        if let Some(description) = &patch.description {
            self.description = Some(description.clone());
        }
        if let Some(attendees) = &patch.attendees {
            self.attendees = attendees.clone();
        }
        if let Some(conference) = &patch.conference {
            match conference {
                ConferencePatch::Create(kind) => self.conference = Some(Conference::pending(*kind)),
                ConferencePatch::Remove => self.conference = None,
            }
        }
        if let Some(recurrence) = &patch.recurrence {
            self.recurrence = recurrence.clone();
            if patch.raw_recurrence.is_none() {
                self.raw_recurrence.clear();
            }
        }
        if let Some(raw) = &patch.raw_recurrence {
            self.raw_recurrence = raw.clone();
        }
        if let Some(color_id) = &patch.color_id {
            self.color_id = Some(color_id.clone());
        }
        if let Some(categories) = &patch.categories {
            self.categories = categories.clone();
        }
        if let Some(show_as) = patch.show_as {
            self.show_as = show_as;
        }
        if let Some(visibility) = patch.visibility {
            self.visibility = visibility;
        }
        if let Some(reminders) = &patch.reminders {
            self.reminders = reminders.clone();
        }
        if let Some(flag) = patch.guests_can_invite_others {
            self.guests_can_invite_others = Some(flag);
        }
        if let Some(flag) = patch.guests_can_modify {
            self.guests_can_modify = flag;
        }
        if let Some(flag) = patch.guests_can_see_other_guests {
            self.guests_can_see_other_guests = Some(flag);
        }
    }
}

/// An event in the cache, paired with the calendar it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// The calendar's ID.
    pub calendar_id: String,
    /// The event (an instance, for a recurring series) with queued local changes overlaid.
    pub event: CalendarEvent,
    /// True while this is a newly created event not yet acknowledged by the calendar provider.
    pub unsynced: bool,
}

/// Preserves the three-way distinction of `Option<Option<T>>` in JSON:
/// absent (`None`), explicit `null` (`Some(None)`), and present (`Some(Some(value))`).
mod double_option {
    use super::*;

    pub fn serialize<T, S>(value: &Option<Option<T>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: Serialize,
        S: Serializer,
    {
        match value {
            Some(Some(inner)) => inner.serialize(serializer),
            Some(None) | None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;
    use crate::calendar::recurrence::{End, Frequency};

    #[test]
    fn account_keys_separate_google_and_microsoft_same_email() {
        let g = CalendarProviderKind::Google.account_key("maya@brightmoor.co");
        let m = CalendarProviderKind::Microsoft.account_key("maya@brightmoor.co");
        assert_ne!(g, m);
        assert_eq!(g, "maya@brightmoor.co");
        assert_eq!(m, "microsoft maya@brightmoor.co");
        assert_eq!(
            CalendarProviderKind::parse_account_key(&g),
            (CalendarProviderKind::Google, "maya@brightmoor.co")
        );
        assert_eq!(
            CalendarProviderKind::parse_account_key(&m),
            (CalendarProviderKind::Microsoft, "maya@brightmoor.co")
        );
    }

    #[test]
    fn event_patch_round_trips_three_way_recurrence_and_empty_attendees() {
        let untouched = EventPatch::default();
        let json = serde_json::to_value(&untouched).expect("serializes");
        assert_eq!(json, serde_json::json!({}));
        assert_eq!(
            serde_json::from_value::<EventPatch>(json).expect("deserializes"),
            untouched
        );

        let cleared = EventPatch {
            attendees: Some(Vec::new()),
            recurrence: Some(None),
            conference: Some(ConferencePatch::Remove),
            ..EventPatch::default()
        };
        let json = serde_json::to_value(&cleared).expect("serializes");
        assert_eq!(json["attendees"], serde_json::json!([]));
        assert_eq!(json["recurrence"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<EventPatch>(json).expect("deserializes"),
            cleared
        );

        let with_rule = EventPatch {
            recurrence: Some(Some(Recurrence {
                frequency: Frequency::Daily,
                interval: 2,
                end: End::After(5),
            })),
            ..EventPatch::default()
        };
        let json = serde_json::to_value(&with_rule).expect("serializes");
        assert_eq!(
            serde_json::from_value::<EventPatch>(json).expect("deserializes"),
            with_rule
        );
    }

    #[test]
    fn event_time_wall_clock_shift_preserves_local_time_across_dst_and_toggles() {
        let ny = TimeZone::get("America/New_York").expect("zone");
        // Oct 27 10:00 EDT (-04:00)
        let timed = EventTime::Timed {
            at: "2026-10-27T14:00:00Z".parse().expect("ts"),
            time_zone: Some("America/New_York".into()),
        };
        // +14 days crosses Nov 1 fall-back: stays 10:00 EST (-05:00 -> 15:00Z).
        let shifted = timed
            .shift_wall_clock(SignedDuration::from_hours(14 * 24), None, None)
            .expect("shifts");
        assert_eq!(
            shifted
                .zoned(&ny)
                .map(|z| z.strftime("%Y-%m-%d %H:%M %Z").to_string()),
            Some("2026-11-10 10:00 EST".to_owned())
        );

        // Toggle timed -> all-day.
        let as_day = timed
            .shift_wall_clock(SignedDuration::from_hours(-10), Some(true), None)
            .expect("toggles");
        assert_eq!(as_day, EventTime::AllDay(date(2026, 10, 27)));

        // Toggle all-day -> timed in NY.
        let back = as_day
            .shift_wall_clock(
                SignedDuration::from_hours(9),
                Some(false),
                Some("America/New_York"),
            )
            .expect("toggles back");
        assert_eq!(
            back.zoned(&ny)
                .map(|z| z.strftime("%Y-%m-%d %H:%M %Z").to_string()),
            Some("2026-10-27 09:00 EDT".to_owned())
        );
    }

    #[test]
    fn calendar_event_apply_patch_and_domain_helpers() {
        let mut event = CalendarEvent {
            id: Some("ev1".into()),
            summary: Some("Sync".into()),
            organizer: Some(EventPerson {
                email: Some("maya@brightmoor.co".into()),
                display_name: Some("Maya".into()),
                is_self: false,
            }),
            attendees: vec![
                EventAttendee {
                    email: Some("maya@brightmoor.co".into()),
                    response_status: RsvpStatus::Accepted,
                    ..EventAttendee::default()
                },
                EventAttendee {
                    email: Some("priya@brightmoor.co".into()),
                    response_status: RsvpStatus::NeedsAction,
                    ..EventAttendee::default()
                },
            ],
            show_as: ShowAs::OutOfOffice,
            ..CalendarEvent::default()
        };
        assert!(event.is_me_organizer("MAYA@brightmoor.co"));
        assert!(event.has_other_guests("maya@brightmoor.co"));
        assert_eq!(
            event.my_rsvp("maya@brightmoor.co"),
            Some(RsvpStatus::Accepted)
        );
        assert!(event.is_out_of_office());

        event.apply_patch(&EventPatch {
            summary: Some("Renamed".into()),
            attendees: Some(Vec::new()),
            conference: Some(ConferencePatch::Create(OnlineMeetingKind::TeamsForBusiness)),
            ..EventPatch::default()
        });
        assert_eq!(event.summary.as_deref(), Some("Renamed"));
        assert!(event.attendees.is_empty());
        assert!(!event.has_other_guests("maya@brightmoor.co"));
        assert_eq!(
            event.conference,
            Some(Conference::pending(OnlineMeetingKind::TeamsForBusiness))
        );
    }
}
