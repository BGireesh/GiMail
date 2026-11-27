//! Calendar calls and models for Microsoft Graph v1.0: calendars, events, calendar views and
//! deltas, series instances, recurrence, meeting responses, organizer cancellations and free/busy
//! schedules.
//!
//! Only the properties Exo reads or writes are modeled; serde skips the rest. Scalars are
//! `Option` and collections default to empty (also accepting `null` on read), so the same
//! [`Event`] type handles `$select`, delta removals and creation bodies. Updates use
//! [`EventPatch`] so collections can be cleared with `Some(vec![])` and recurrence removed with
//! `Some(None)`.

use jiff::Timestamp;
use jiff::civil::{Date, Time, Weekday};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_with::{DefaultOnNull, serde_as, skip_serializing_none};

use crate::client::{Call, Client};
use crate::model::{
    Attachment, DateTimeTimeZone, EmailAddress, Importance, ItemBody, Location, OpenTypeExtension,
    Recipient,
};
use crate::odata::{Page, Removed};

pub use crate::model::{EventType, LocationType, LocationUniqueIdType};

/// A calendar (learn.microsoft.com/graph/api/resources/calendar).
#[serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    /// The calendar's ID.
    pub id: Option<String>,
    /// Its display name (`Calendar` for the default calendar).
    pub name: Option<String>,
    /// Its theme color preset.
    pub color: Option<CalendarColor>,
    /// Its hex color (`#rrggbb`), read-only; empty when `color` is [`CalendarColor::Auto`].
    pub hex_color: Option<String>,
    /// Whether this is the user's default calendar.
    pub is_default_calendar: Option<bool>,
    /// Its version, which changes with every edit.
    pub change_key: Option<String>,
    /// Whether the user can share this calendar.
    pub can_share: Option<bool>,
    /// Whether the user can view private events on this calendar.
    pub can_view_private_items: Option<bool>,
    /// Whether another user shared this calendar with the signed-in user.
    pub is_shared_with_me: Option<bool>,
    /// Whether the user can write events to this calendar.
    pub can_edit: Option<bool>,
    /// Online meeting providers allowed on events in this calendar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub allowed_online_meeting_providers: Vec<OnlineMeetingProviderType>,
    /// Default provider used when `isOnlineMeeting` is set without specifying a provider.
    pub default_online_meeting_provider: Option<OnlineMeetingProviderType>,
    /// Whether the calendar tallies attendee responses.
    pub is_tallying_responses: Option<bool>,
    /// Whether the calendar can be deleted (`false` for the default calendar).
    pub is_removable: Option<bool>,
    /// The calendar's owner.
    pub owner: Option<EmailAddress>,
}

/// [`Calendar::color`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CalendarColor {
    /// Assigned automatically by Outlook.
    Auto,
    /// Light blue.
    LightBlue,
    /// Light green.
    LightGreen,
    /// Light orange.
    LightOrange,
    /// Light gray.
    LightGray,
    /// Light yellow.
    LightYellow,
    /// Light teal.
    LightTeal,
    /// Light pink.
    LightPink,
    /// Light brown.
    LightBrown,
    /// Light red.
    LightRed,
    /// Maximum color value sentinel.
    MaxColor,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// An event (learn.microsoft.com/graph/api/resources/event).
#[serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// `#microsoft.graph.event` in calendar view deltas.
    #[serde(rename = "@odata.type")]
    pub odata_type: Option<String>,
    /// The event's immutable ID.
    pub id: Option<String>,
    /// When the event was created.
    pub created_date_time: Option<Timestamp>,
    /// When the event last changed.
    pub last_modified_date_time: Option<Timestamp>,
    /// Its version, which changes with every edit.
    pub change_key: Option<String>,
    /// Categories assigned to the event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub categories: Vec<String>,
    /// Client-chosen idempotency key for creates (ignored on updates).
    pub transaction_id: Option<String>,
    /// Time zone the start was created in (Windows or IANA name).
    pub original_start_time_zone: Option<String>,
    /// Time zone the end was created in (Windows or IANA name).
    pub original_end_time_zone: Option<String>,
    /// iCalendar UID shared across all instances of a series.
    #[serde(rename = "iCalUId")]
    pub ical_uid: Option<String>,
    /// Minutes before start that the reminder fires.
    pub reminder_minutes_before_start: Option<u32>,
    /// Whether a reminder is enabled.
    pub is_reminder_on: Option<bool>,
    /// Whether the event has attachments.
    pub has_attachments: Option<bool>,
    /// The title.
    pub subject: Option<String>,
    /// Plain-text preview of the body (up to 255 characters).
    pub body_preview: Option<String>,
    /// Its importance.
    pub importance: Option<Importance>,
    /// Privacy sensitivity.
    pub sensitivity: Option<Sensitivity>,
    /// Whether the event spans whole days (midnights in the requested zone).
    pub is_all_day: Option<bool>,
    /// Whether the event was cancelled.
    pub is_cancelled: Option<bool>,
    /// Whether the calendar owner organized the event.
    pub is_organizer: Option<bool>,
    /// Whether the organizer requested attendee responses.
    pub response_requested: Option<bool>,
    /// For occurrences and exceptions, the ID of their series master.
    pub series_master_id: Option<String>,
    /// Free/busy status shown on the calendar.
    pub show_as: Option<FreeBusyStatus>,
    /// Single instance, occurrence, exception or series master.
    #[serde(rename = "type")]
    pub kind: Option<EventType>,
    /// Outlook on the web link to the event.
    pub web_link: Option<String>,
    /// Legacy online meeting join URL.
    pub online_meeting_url: Option<String>,
    /// Whether the event has an online meeting attached.
    pub is_online_meeting: Option<bool>,
    /// Provider for the online meeting (e.g. Teams or Skype).
    pub online_meeting_provider: Option<OnlineMeetingProviderType>,
    /// Whether invitees may propose a new time.
    pub allow_new_time_proposals: Option<bool>,
    /// Whether the event is an unsent draft.
    pub is_draft: Option<bool>,
    /// Whether attendees are hidden from one another.
    pub hide_attendees: Option<bool>,
    /// The signed-in user's response status.
    pub response_status: Option<ResponseStatus>,
    /// The description body.
    pub body: Option<ItemBody>,
    /// Start date and time.
    pub start: Option<DateTimeTimeZone>,
    /// End date and time.
    pub end: Option<DateTimeTimeZone>,
    /// Primary location.
    pub location: Option<Location>,
    /// All locations associated with the event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub locations: Vec<Location>,
    /// Recurrence pattern and range for a series master.
    pub recurrence: Option<PatternedRecurrence>,
    /// Invited attendees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub attendees: Vec<Attendee>,
    /// Who organized the event.
    pub organizer: Option<Recipient>,
    /// Online meeting join details (Teams, Skype or third-party link).
    pub online_meeting: Option<OnlineMeetingInfo>,
    /// For occurrences and exceptions, the original start instant of the instance.
    pub original_start: Option<Timestamp>,
    /// Occurrence IDs cancelled from a series (`$select=cancelledOccurrences`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub cancelled_occurrences: Vec<String>,
    /// The owning calendar (`$expand=calendar`).
    pub calendar: Option<Box<Calendar>>,
    /// Event attachments (`$expand=attachments`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub attachments: Vec<Attachment>,
    /// Open extensions (`$expand=extensions`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub extensions: Vec<OpenTypeExtension>,
    /// In a calendar view delta: the event was deleted, cancelled or left the window.
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

/// A partial update to an event (`PATCH`, learn.microsoft.com/graph/api/event-update).
///
/// Only fields set to `Some` are sent. Setting `attendees`, `categories` or `locations` to
/// `Some(Vec::new())` clears them on the server. Setting `recurrence` to `Some(None)` emits
/// `"recurrence": null`, turning a series master into a single event, while `None` leaves it
/// untouched.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPatch {
    /// Updated title.
    pub subject: Option<String>,
    /// Updated description.
    pub body: Option<ItemBody>,
    /// Updated start time.
    pub start: Option<DateTimeTimeZone>,
    /// Updated end time.
    pub end: Option<DateTimeTimeZone>,
    /// Whether the event is all-day.
    pub is_all_day: Option<bool>,
    /// Updated primary location.
    pub location: Option<Location>,
    /// Full replacement list of locations (`Some(vec![])` clears them).
    pub locations: Option<Vec<Location>>,
    /// Full replacement list of attendees (`Some(vec![])` clears all invitees).
    pub attendees: Option<Vec<Attendee>>,
    /// `None` leaves recurrence unchanged; `Some(None)` clears it (`null`); `Some(Some(r))` sets it.
    pub recurrence: Option<Option<PatternedRecurrence>>,
    /// Updated free/busy status.
    pub show_as: Option<FreeBusyStatus>,
    /// Updated sensitivity.
    pub sensitivity: Option<Sensitivity>,
    /// Updated importance.
    pub importance: Option<Importance>,
    /// Full replacement list of categories (`Some(vec![])` clears them).
    pub categories: Option<Vec<String>>,
    /// Enable or disable the reminder.
    pub is_reminder_on: Option<bool>,
    /// Minutes before start that the reminder fires.
    pub reminder_minutes_before_start: Option<u32>,
    /// Whether the organizer requests attendee responses.
    pub response_requested: Option<bool>,
    /// Whether attendees may propose a new time.
    pub allow_new_time_proposals: Option<bool>,
    /// Whether attendees are hidden from one another.
    pub hide_attendees: Option<bool>,
    /// Attach an online meeting (once online, Graph ignores turning it off).
    pub is_online_meeting: Option<bool>,
    /// Online meeting provider when enabling an online meeting.
    pub online_meeting_provider: Option<OnlineMeetingProviderType>,
}

/// [`Event::sensitivity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Sensitivity {
    /// Normal.
    Normal,
    /// Personal.
    Personal,
    /// Private.
    Private,
    /// Confidential.
    Confidential,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// [`Event::show_as`] and [`ScheduleItem::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FreeBusyStatus {
    /// Unknown.
    Unknown,
    /// Free.
    Free,
    /// Tentative.
    Tentative,
    /// Busy.
    Busy,
    /// Out of office.
    Oof,
    /// Working elsewhere.
    WorkingElsewhere,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// [`Calendar::default_online_meeting_provider`] and [`Event::online_meeting_provider`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OnlineMeetingProviderType {
    /// No Graph-managed provider (or third-party link).
    Unknown,
    /// Skype for Business.
    SkypeForBusiness,
    /// Skype for consumer accounts.
    SkypeForConsumer,
    /// Microsoft Teams for work or school accounts.
    TeamsForBusiness,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// Online meeting join information attached to an event
/// (learn.microsoft.com/graph/api/resources/onlinemeetinginfo).
#[serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineMeetingInfo {
    /// URL to join the online meeting.
    pub join_url: Option<String>,
    /// Conference ID for dial-in.
    pub conference_id: Option<String>,
    /// Quick-dial string for mobile devices.
    pub quick_dial: Option<String>,
    /// Toll dial-in number.
    pub toll_number: Option<String>,
    /// Toll-free dial-in numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub toll_free_numbers: Vec<String>,
    /// All dial-in phone numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub phones: Vec<Phone>,
}

/// A dial-in phone number on [`OnlineMeetingInfo`].
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phone {
    /// Phone number.
    pub number: Option<String>,
    /// Kind of phone number.
    #[serde(rename = "type")]
    pub kind: Option<PhoneType>,
    /// Language code.
    pub language: Option<String>,
    /// Region name.
    pub region: Option<String>,
}

/// [`Phone::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PhoneType {
    /// Home.
    Home,
    /// Business.
    Business,
    /// Mobile.
    Mobile,
    /// Other.
    Other,
    /// Assistant.
    Assistant,
    /// Home fax.
    HomeFax,
    /// Business fax.
    BusinessFax,
    /// Other fax.
    OtherFax,
    /// Pager.
    Pager,
    /// Radio.
    Radio,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// An event attendee (learn.microsoft.com/graph/api/resources/attendee).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    /// Attendee's name and address.
    pub email_address: Option<EmailAddress>,
    /// Required, optional or resource (room).
    #[serde(rename = "type")]
    pub kind: Option<AttendeeType>,
    /// Their response status.
    pub status: Option<ResponseStatus>,
    /// An alternate time proposed by this attendee.
    pub proposed_new_time: Option<TimeSlot>,
}

impl Attendee {
    /// An attendee with `address`, optional display `name` and `kind`.
    #[must_use]
    pub fn new(address: &str, name: Option<&str>, kind: AttendeeType) -> Self {
        Self {
            email_address: Some(EmailAddress {
                name: name.map(str::to_owned),
                address: Some(address.to_owned()),
            }),
            kind: Some(kind),
            status: None,
            proposed_new_time: None,
        }
    }
}

/// [`Attendee::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttendeeType {
    /// Required attendee.
    Required,
    /// Optional attendee.
    Optional,
    /// Room or equipment resource.
    Resource,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// An attendee's or organizer's response status
/// (learn.microsoft.com/graph/api/resources/responsestatus).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseStatus {
    /// The response type.
    pub response: Option<ResponseType>,
    /// When the response was recorded (`0001-01-01T00:00:00Z` when unset).
    pub time: Option<Timestamp>,
}

impl ResponseStatus {
    /// The response timestamp, omitting Graph's `0001-01-01T00:00:00Z` placeholder.
    #[must_use]
    pub fn effective_time(&self) -> Option<Timestamp> {
        self.time
            .filter(|at| at.to_zoned(jiff::tz::TimeZone::UTC).year() > 1)
    }
}

/// [`ResponseStatus::response`] and [`crate::Message::response_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseType {
    /// No response yet.
    None,
    /// The user is the organizer.
    Organizer,
    /// Tentatively accepted.
    TentativelyAccepted,
    /// Accepted.
    Accepted,
    /// Declined.
    Declined,
    /// Has not responded.
    NotResponded,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// A start/end time range (learn.microsoft.com/graph/api/resources/timeslot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeSlot {
    /// Start of the time slot.
    pub start: DateTimeTimeZone,
    /// End of the time slot.
    pub end: DateTimeTimeZone,
}

/// A recurring series' pattern and range
/// (learn.microsoft.com/graph/api/resources/patternedrecurrence).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternedRecurrence {
    /// Frequency and days of the pattern.
    pub pattern: Option<RecurrencePattern>,
    /// Start date and termination rule.
    pub range: Option<RecurrenceRange>,
}

impl PatternedRecurrence {
    /// Combines `pattern` and `range`.
    #[must_use]
    pub fn new(pattern: RecurrencePattern, range: RecurrenceRange) -> Self {
        Self {
            pattern: Some(pattern),
            range: Some(range),
        }
    }
}

/// How often a recurring series repeats
/// (learn.microsoft.com/graph/api/resources/recurrencepattern).
#[serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecurrencePattern {
    /// Daily, weekly, absolute/relative monthly or yearly.
    #[serde(rename = "type")]
    pub kind: Option<RecurrencePatternType>,
    /// Units between occurrences (at least 1).
    pub interval: Option<u32>,
    /// Month of the year (1–12 for yearly patterns; Graph sends `0` when unused).
    pub month: Option<u32>,
    /// Day of the month (1–31 for absolute patterns; Graph sends `0` when unused).
    pub day_of_month: Option<u32>,
    /// Weekdays on which occurrences fall (weekly and relative patterns).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub days_of_week: Vec<DayOfWeek>,
    /// First day of the week (`sunday` by default).
    pub first_day_of_week: Option<DayOfWeek>,
    /// Ordinal week in the month for relative patterns (`first` through `fourth`, or `last`).
    pub index: Option<WeekIndex>,
}

/// [`RecurrencePattern::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecurrencePatternType {
    /// Every `interval` days.
    Daily,
    /// Every `interval` weeks on `days_of_week`.
    Weekly,
    /// Every `interval` months on `day_of_month`.
    AbsoluteMonthly,
    /// Every `interval` months on the `index`-th of `days_of_week`.
    RelativeMonthly,
    /// Every `interval` years on `day_of_month` of `month`.
    AbsoluteYearly,
    /// Every `interval` years on the `index`-th of `days_of_week` in `month`.
    RelativeYearly,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// A day of the week (`sunday` through `saturday`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DayOfWeek {
    /// Sunday.
    Sunday,
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
}

impl From<Weekday> for DayOfWeek {
    fn from(weekday: Weekday) -> Self {
        match weekday {
            Weekday::Sunday => Self::Sunday,
            Weekday::Monday => Self::Monday,
            Weekday::Tuesday => Self::Tuesday,
            Weekday::Wednesday => Self::Wednesday,
            Weekday::Thursday => Self::Thursday,
            Weekday::Friday => Self::Friday,
            Weekday::Saturday => Self::Saturday,
        }
    }
}

impl From<DayOfWeek> for Weekday {
    fn from(day: DayOfWeek) -> Self {
        match day {
            DayOfWeek::Sunday => Self::Sunday,
            DayOfWeek::Monday => Self::Monday,
            DayOfWeek::Tuesday => Self::Tuesday,
            DayOfWeek::Wednesday => Self::Wednesday,
            DayOfWeek::Thursday => Self::Thursday,
            DayOfWeek::Friday => Self::Friday,
            DayOfWeek::Saturday => Self::Saturday,
        }
    }
}

/// [`RecurrencePattern::index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WeekIndex {
    /// First matching weekday in the month.
    First,
    /// Second.
    Second,
    /// Third.
    Third,
    /// Fourth.
    Fourth,
    /// Last.
    Last,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// When a recurring series starts and stops
/// (learn.microsoft.com/graph/api/resources/recurrencerange).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecurrenceRange {
    /// End date, no end, or numbered occurrence count.
    #[serde(rename = "type")]
    pub kind: Option<RecurrenceRangeType>,
    /// Date of the first occurrence (`YYYY-MM-DD`).
    pub start_date: Option<Date>,
    /// Inclusive end date (`YYYY-MM-DD`). Graph sends `0001-01-01` when `kind` is not `EndDate`.
    pub end_date: Option<Date>,
    /// Time zone of `start_date` and `end_date` (Windows or IANA name).
    pub recurrence_time_zone: Option<String>,
    /// Occurrence count for [`RecurrenceRangeType::Numbered`] (`0` otherwise).
    pub number_of_occurrences: Option<u32>,
}

impl RecurrenceRange {
    /// The end date when [`Self::kind`] is [`RecurrenceRangeType::EndDate`] and [`Self::end_date`]
    /// isn't Graph's `0001-01-01` placeholder.
    #[must_use]
    pub fn effective_end_date(&self) -> Option<Date> {
        (self.kind == Some(RecurrenceRangeType::EndDate))
            .then_some(self.end_date?)
            .filter(|date| date.year() > 1)
    }
}

/// [`RecurrenceRange::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecurrenceRangeType {
    /// Repeats through `end_date`.
    EndDate,
    /// Repeats indefinitely.
    NoEnd,
    /// Repeats for `number_of_occurrences` instances.
    Numbered,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// Body for [`Calendars::accept`] (learn.microsoft.com/graph/api/event-accept).
///
/// Unlike `tentativelyAccept` and `decline`, Graph rejects `proposedNewTime` on `accept`, so this
/// type omits it statically.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptRequest {
    /// Optional note included in the reply to the organizer.
    pub comment: Option<String>,
    /// Whether to send a response email to the organizer (Graph defaults to `true`).
    pub send_response: Option<bool>,
}

/// Body for [`Calendars::tentatively_accept`] and [`Calendars::decline`]
/// (learn.microsoft.com/graph/api/event-tentativelyaccept).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondRequest {
    /// Optional note included in the reply to the organizer.
    pub comment: Option<String>,
    /// Whether to send a response email to the organizer (Graph defaults to `true`).
    pub send_response: Option<bool>,
    /// Alternate meeting time to propose to the organizer (requires `send_response != Some(false)`).
    pub proposed_new_time: Option<TimeSlot>,
}

/// Body for [`Calendars::get_schedule`] (learn.microsoft.com/graph/api/calendar-getschedule).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRequest {
    /// Email addresses (users or rooms) whose schedules to query.
    pub schedules: Vec<String>,
    /// Start of the queried window.
    pub start_time: DateTimeTimeZone,
    /// End of the queried window.
    pub end_time: DateTimeTimeZone,
    /// Length of each `availability_view` bucket in minutes (5–1440; Graph defaults to 30).
    pub availability_view_interval: Option<u32>,
}

/// Free/busy schedule for one address returned by [`Calendars::get_schedule`]
/// (learn.microsoft.com/graph/api/resources/scheduleinformation).
#[serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInformation {
    /// The queried user or room email address.
    pub schedule_id: Option<String>,
    /// Merged availability string: one digit per interval (`0` free, `1` tentative, `2` busy,
    /// `3` out of office, `4` working elsewhere).
    pub availability_view: Option<String>,
    /// Individual busy items in the window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub schedule_items: Vec<ScheduleItem>,
    /// The schedule owner's working hours.
    pub working_hours: Option<WorkingHours>,
    /// Set when the schedule could not be resolved (e.g. unknown recipient).
    pub error: Option<ScheduleError>,
}

/// One busy block in [`ScheduleInformation::schedule_items`]
/// (learn.microsoft.com/graph/api/resources/scheduleitem).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleItem {
    /// Whether the underlying event is private (private items omit `subject` and `location`).
    pub is_private: Option<bool>,
    /// Free/busy status of this item.
    pub status: Option<FreeBusyStatus>,
    /// Event subject (absent for private items).
    pub subject: Option<String>,
    /// Event location (absent for private items).
    pub location: Option<String>,
    /// Start time (in the `Prefer: outlook.timezone` zone, else UTC).
    pub start: Option<DateTimeTimeZone>,
    /// End time (in the `Prefer: outlook.timezone` zone, else UTC).
    pub end: Option<DateTimeTimeZone>,
}

/// Error reported for an individual address in [`ScheduleInformation::error`]
/// (Graph's `freeBusyError`).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleError {
    /// Human-readable error message.
    pub message: Option<String>,
    /// Machine-readable Graph response code (e.g. `ErrorMailRecipientNotFound`).
    pub response_code: Option<String>,
}

/// Working days, hours and time zone (learn.microsoft.com/graph/api/resources/workinghours).
#[serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingHours {
    /// Days of the week included in the work schedule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub days_of_week: Vec<DayOfWeek>,
    /// Start of the working day (`08:00:00.0000000`).
    pub start_time: Option<Time>,
    /// End of the working day (`17:00:00.0000000`).
    pub end_time: Option<Time>,
    /// Time zone of the working hours.
    pub time_zone: Option<TimeZoneBase>,
}

/// A named time zone (`timeZoneBase`).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeZoneBase {
    /// Windows or IANA time zone name (e.g. `Eastern Standard Time`).
    pub name: Option<String>,
}

/// Body for `POST .../cancel`.
#[skip_serializing_none]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelBody<'a> {
    comment: Option<&'a str>,
}

/// Calendar calls. Get one with [`Client::calendar`].
#[derive(Debug, Clone, Copy)]
pub struct Calendars<'a> {
    client: &'a Client,
}

impl Client {
    /// Calendar and scheduling calls for the signed-in user.
    #[must_use]
    pub fn calendar(&self) -> Calendars<'_> {
        Calendars { client: self }
    }
}

impl<'a> Calendars<'a> {
    fn scoped<T>(&self, method: Method, calendar_id: Option<&str>, tail: &[&str]) -> Call<'a, T> {
        match calendar_id {
            Some(id) => self.client.me_call(
                method,
                ["calendars", id].into_iter().chain(tail.iter().copied()),
            ),
            None => self.client.me_call(method, tail.iter().copied()),
        }
    }

    // Calendars

    /// Lists the user's calendars (`GET /me/calendars`).
    pub fn calendars(&self) -> Call<'a, Page<Calendar>> {
        self.client.me_call(Method::GET, ["calendars"])
    }

    /// Reads the user's default calendar (`GET /me/calendar`).
    pub fn default_calendar(&self) -> Call<'a, Calendar> {
        self.client.me_call(Method::GET, ["calendar"])
    }

    /// Reads a calendar by ID (`GET /me/calendars/{id}`).
    pub fn calendar(&self, id: &str) -> Call<'a, Calendar> {
        self.client.me_call(Method::GET, ["calendars", id])
    }

    /// Creates a new calendar (`POST /me/calendars`).
    pub fn create_calendar(&self, calendar: &Calendar) -> Call<'a, Calendar> {
        self.client
            .me_call(Method::POST, ["calendars"])
            .json(calendar)
    }

    /// Updates a calendar's properties such as name or color (`PATCH /me/calendars/{id}`).
    pub fn update_calendar(&self, id: &str, calendar: &Calendar) -> Call<'a, Calendar> {
        self.client
            .me_call(Method::PATCH, ["calendars", id])
            .json(calendar)
    }

    /// Deletes a secondary calendar (`DELETE /me/calendars/{id}`).
    pub fn delete_calendar(&self, id: &str) -> Call<'a, ()> {
        self.client.me_call(Method::DELETE, ["calendars", id])
    }

    // Events and views

    /// Lists single events and series masters in the default calendar (`None`) or `calendar_id`.
    pub fn events(&self, calendar_id: Option<&str>) -> Call<'a, Page<Event>> {
        self.scoped(Method::GET, calendar_id, &["events"])
    }

    /// Finds events matching `ical_uid` (`$filter=iCalUId eq '...'`) in the default calendar
    /// (`None`) or `calendar_id`.
    pub fn events_by_ical_uid(
        &self,
        calendar_id: Option<&str>,
        ical_uid: &str,
    ) -> Call<'a, Page<Event>> {
        self.events(calendar_id)
            .filter(&format!("iCalUId eq {}", crate::odata::quote(ical_uid)))
    }

    /// Reads one event by ID, optionally scoped to `calendar_id`.
    pub fn event(&self, calendar_id: Option<&str>, event_id: &str) -> Call<'a, Event> {
        self.scoped(Method::GET, calendar_id, &["events", event_id])
    }

    /// Creates an event in the default calendar (`None`) or `calendar_id`.
    pub fn create_event(&self, calendar_id: Option<&str>, event: &Event) -> Call<'a, Event> {
        self.scoped(Method::POST, calendar_id, &["events"])
            .json(event)
    }

    /// Applies `patch` to an event (`PATCH`). Updating an occurrence promotes it to an exception.
    pub fn update_event(
        &self,
        calendar_id: Option<&str>,
        event_id: &str,
        patch: &EventPatch,
    ) -> Call<'a, Event> {
        self.scoped(Method::PATCH, calendar_id, &["events", event_id])
            .json(patch)
    }

    /// Deletes an event (`DELETE`). Deleting a series removes its exceptions; deleting an
    /// occurrence cancels that instance; an organizer deleting a meeting notifies invitees.
    pub fn delete_event(&self, calendar_id: Option<&str>, event_id: &str) -> Call<'a, ()> {
        self.scoped(Method::DELETE, calendar_id, &["events", event_id])
    }

    /// Lists all events (single events, occurrences and exceptions) overlapping `[start, end)` in
    /// the default calendar (`None`) or `calendar_id` (`GET .../calendarView`).
    pub fn calendar_view(
        &self,
        calendar_id: Option<&str>,
        start: Timestamp,
        end: Timestamp,
    ) -> Call<'a, Page<Event>> {
        self.scoped(Method::GET, calendar_id, &["calendarView"])
            .query("startDateTime", start)
            .query("endDateTime", end)
    }

    /// Starts a delta sync over `[start, end)` for the default calendar (`None`) or `calendar_id`
    /// (`GET .../calendarView/delta`, learn.microsoft.com/graph/api/event-delta).
    ///
    /// Graph rejects `$select`, `$expand`, `$filter`, `$orderby`, `$search` and `$top` on calendar
    /// view deltas; control page size with [`Call::max_page_size`] and response zone with
    /// [`Call::timezone`], then follow [`Page::delta_link`] with [`Client::follow`].
    pub fn calendar_view_delta(
        &self,
        calendar_id: Option<&str>,
        start: Timestamp,
        end: Timestamp,
    ) -> Call<'a, Page<Event>> {
        self.scoped(Method::GET, calendar_id, &["calendarView", "delta"])
            .query("startDateTime", start)
            .query("endDateTime", end)
    }

    /// Lists occurrences and exceptions of `series_master_id` overlapping `[start, end)`
    /// (`GET .../events/{id}/instances`).
    pub fn instances(
        &self,
        calendar_id: Option<&str>,
        series_master_id: &str,
        start: Timestamp,
        end: Timestamp,
    ) -> Call<'a, Page<Event>> {
        self.scoped(
            Method::GET,
            calendar_id,
            &["events", series_master_id, "instances"],
        )
        .query("startDateTime", start)
        .query("endDateTime", end)
    }

    // Meeting actions

    /// Accepts a meeting invitation (`POST .../events/{id}/accept`).
    pub fn accept(
        &self,
        calendar_id: Option<&str>,
        event_id: &str,
        request: &AcceptRequest,
    ) -> Call<'a, ()> {
        self.scoped(Method::POST, calendar_id, &["events", event_id, "accept"])
            .json(request)
    }

    /// Tentatively accepts a meeting invitation, optionally proposing a new time
    /// (`POST .../events/{id}/tentativelyAccept`).
    pub fn tentatively_accept(
        &self,
        calendar_id: Option<&str>,
        event_id: &str,
        request: &RespondRequest,
    ) -> Call<'a, ()> {
        self.scoped(
            Method::POST,
            calendar_id,
            &["events", event_id, "tentativelyAccept"],
        )
        .json(request)
    }

    /// Declines a meeting invitation, optionally proposing a new time
    /// (`POST .../events/{id}/decline`).
    pub fn decline(
        &self,
        calendar_id: Option<&str>,
        event_id: &str,
        request: &RespondRequest,
    ) -> Call<'a, ()> {
        self.scoped(Method::POST, calendar_id, &["events", event_id, "decline"])
            .json(request)
    }

    /// Cancels a meeting as the organizer, notifying attendees with an optional `comment`
    /// (`POST .../events/{id}/cancel`).
    pub fn cancel(
        &self,
        calendar_id: Option<&str>,
        event_id: &str,
        comment: Option<&str>,
    ) -> Call<'a, ()> {
        self.scoped(Method::POST, calendar_id, &["events", event_id, "cancel"])
            .json(&CancelBody { comment })
    }

    // Scheduling

    /// Queries free/busy availability for users and rooms (`POST /me/calendar/getSchedule`).
    /// Supported on work and school accounts; personal accounts return 403.
    pub fn get_schedule(&self, request: &ScheduleRequest) -> Call<'a, Page<ScheduleInformation>> {
        self.client
            .me_call(Method::POST, ["calendar", "getSchedule"])
            .json(request)
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::{date, datetime, time};
    use serde_json::json;

    use super::*;
    use crate::{Endpoints, StaticToken};

    #[test]
    fn event_patch_distinguishes_absent_empty_and_null() {
        let minimal = EventPatch {
            subject: Some("Standup".into()),
            ..EventPatch::default()
        };
        assert_eq!(
            serde_json::to_value(&minimal).unwrap(),
            json!({ "subject": "Standup" })
        );

        let clear = EventPatch {
            attendees: Some(Vec::new()),
            categories: Some(Vec::new()),
            locations: Some(Vec::new()),
            recurrence: Some(None),
            ..EventPatch::default()
        };
        assert_eq!(
            serde_json::to_value(&clear).unwrap(),
            json!({
                "attendees": [],
                "categories": [],
                "locations": [],
                "recurrence": null,
            })
        );

        let recurring = EventPatch {
            recurrence: Some(Some(PatternedRecurrence::new(
                RecurrencePattern {
                    kind: Some(RecurrencePatternType::Daily),
                    interval: Some(2),
                    ..RecurrencePattern::default()
                },
                RecurrenceRange {
                    kind: Some(RecurrenceRangeType::NoEnd),
                    start_date: Some(date(2026, 11, 18)),
                    ..RecurrenceRange::default()
                },
            ))),
            ..EventPatch::default()
        };
        assert_eq!(
            serde_json::to_value(&recurring).unwrap(),
            json!({
                "recurrence": {
                    "pattern": { "type": "daily", "interval": 2 },
                    "range": { "type": "noEnd", "startDate": "2026-11-18" },
                }
            })
        );
    }

    #[test]
    fn recurrence_round_trips_and_handles_null_collections_and_sentinel_dates() {
        let wire = json!({
            "pattern": {
                "type": "relativeYearly",
                "interval": 1,
                "month": 11,
                "dayOfMonth": 0,
                "daysOfWeek": ["thursday"],
                "firstDayOfWeek": "sunday",
                "index": "fourth"
            },
            "range": {
                "type": "noEnd",
                "startDate": "2026-11-26",
                "endDate": "0001-01-01",
                "recurrenceTimeZone": "Eastern Standard Time",
                "numberOfOccurrences": 0
            }
        });
        let parsed: PatternedRecurrence = serde_json::from_value(wire).unwrap();
        let pattern = parsed.pattern.as_ref().unwrap();
        assert_eq!(pattern.kind, Some(RecurrencePatternType::RelativeYearly));
        assert_eq!(pattern.days_of_week, [DayOfWeek::Thursday]);
        assert_eq!(pattern.index, Some(WeekIndex::Fourth));
        let range = parsed.range.as_ref().unwrap();
        assert_eq!(range.kind, Some(RecurrenceRangeType::NoEnd));
        assert_eq!(range.start_date, Some(date(2026, 11, 26)));
        assert_eq!(range.end_date, Some(date(1, 1, 1)));
        assert_eq!(range.effective_end_date(), None);

        // `daysOfWeek: null` on daily patterns deserializes as empty.
        let daily: RecurrencePattern = serde_json::from_value(json!({
            "type": "daily",
            "interval": 1,
            "daysOfWeek": null
        }))
        .unwrap();
        assert!(daily.days_of_week.is_empty());

        let bounded = RecurrenceRange {
            kind: Some(RecurrenceRangeType::EndDate),
            start_date: Some(date(2026, 11, 16)),
            end_date: Some(date(2026, 12, 31)),
            ..RecurrenceRange::default()
        };
        assert_eq!(bounded.effective_end_date(), Some(date(2026, 12, 31)));
    }

    #[test]
    fn response_status_handles_sentinel_timestamp() {
        let unset: ResponseStatus = serde_json::from_value(json!({
            "response": "none",
            "time": "0001-01-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(unset.response, Some(ResponseType::None));
        assert!(unset.time.is_some());
        assert_eq!(unset.effective_time(), None);

        let answered: ResponseStatus = serde_json::from_value(json!({
            "response": "accepted",
            "time": "2026-11-17T15:30:00Z"
        }))
        .unwrap();
        assert_eq!(
            answered.effective_time(),
            Some("2026-11-17T15:30:00Z".parse().unwrap())
        );
    }

    #[test]
    fn evolvable_calendar_enums_read_future_values() {
        assert_eq!(
            serde_json::from_value::<CalendarColor>(json!("neonBlue")).unwrap(),
            CalendarColor::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<FreeBusyStatus>(json!("focus")).unwrap(),
            FreeBusyStatus::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<Sensitivity>(json!("secret")).unwrap(),
            Sensitivity::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<OnlineMeetingProviderType>(json!("teamsForConsumer")).unwrap(),
            OnlineMeetingProviderType::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<AttendeeType>(json!("delegate")).unwrap(),
            AttendeeType::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<ResponseType>(json!("forwarded")).unwrap(),
            ResponseType::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<RecurrencePatternType>(json!("hourly")).unwrap(),
            RecurrencePatternType::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<RecurrenceRangeType>(json!("infinite")).unwrap(),
            RecurrenceRangeType::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<WeekIndex>(json!("fifth")).unwrap(),
            WeekIndex::UnknownFutureValue
        );
        assert_eq!(
            serde_json::from_value::<PhoneType>(json!("satellite")).unwrap(),
            PhoneType::UnknownFutureValue
        );
    }

    #[test]
    fn day_of_week_round_trips_with_jiff_weekday() {
        for weekday in [
            Weekday::Sunday,
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
            Weekday::Saturday,
        ] {
            let day = DayOfWeek::from(weekday);
            assert_eq!(Weekday::from(day), weekday);
        }
    }

    #[test]
    fn working_hours_and_schedule_wire_types() {
        let info: ScheduleInformation = serde_json::from_value(json!({
            "scheduleId": "maya@brightmoor.co",
            "availabilityView": "0020",
            "scheduleItems": null,
            "workingHours": {
                "daysOfWeek": ["monday", "friday"],
                "startTime": "08:00:00.0000000",
                "endTime": "17:30:00.0000000",
                "timeZone": { "name": "Eastern Standard Time" }
            }
        }))
        .unwrap();
        assert!(info.schedule_items.is_empty());
        let hours = info.working_hours.unwrap();
        assert_eq!(hours.days_of_week, [DayOfWeek::Monday, DayOfWeek::Friday]);
        assert_eq!(hours.start_time, Some(time(8, 0, 0, 0)));
        assert_eq!(hours.end_time, Some(time(17, 30, 0, 0)));
        assert_eq!(
            hours.time_zone.and_then(|tz| tz.name).as_deref(),
            Some("Eastern Standard Time")
        );
    }

    #[test]
    fn calendar_crud_and_action_requests_build_expected_paths_and_bodies() {
        let client = Client::new(Endpoints::microsoft(), StaticToken("t".into()));
        let cal = client.calendar();

        let create = cal.create_calendar(&Calendar {
            name: Some("Travel".into()),
            color: Some(CalendarColor::LightTeal),
            ..Calendar::default()
        });
        assert_eq!(create.request.method, Method::POST);
        assert_eq!(
            create.request.url.as_str(),
            "https://graph.microsoft.com/v1.0/me/calendars"
        );
        let crate::client::Body::Json(raw) = &create.request.body else {
            panic!("expected JSON body");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(raw.get()).unwrap(),
            json!({ "name": "Travel", "color": "lightTeal" })
        );

        let update = cal.update_calendar(
            "cal-1",
            &Calendar {
                name: Some("Trips".into()),
                color: Some(CalendarColor::LightBlue),
                ..Calendar::default()
            },
        );
        assert_eq!(update.request.method, Method::PATCH);
        assert_eq!(
            update.request.url.as_str(),
            "https://graph.microsoft.com/v1.0/me/calendars/cal-1"
        );

        let delete = cal.delete_calendar("cal-1");
        assert_eq!(delete.request.method, Method::DELETE);
        assert_eq!(
            delete.request.url.as_str(),
            "https://graph.microsoft.com/v1.0/me/calendars/cal-1"
        );

        let accept = cal.accept(None, "ev-1", &AcceptRequest::default());
        assert_eq!(
            accept.request.url.as_str(),
            "https://graph.microsoft.com/v1.0/me/events/ev-1/accept"
        );
        let crate::client::Body::Json(raw) = &accept.request.body else {
            panic!("expected JSON body");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(raw.get()).unwrap(),
            json!({})
        );

        let cancel = cal.cancel(Some("cal-1"), "ev-1", None);
        assert_eq!(
            cancel.request.url.as_str(),
            "https://graph.microsoft.com/v1.0/me/calendars/cal-1/events/ev-1/cancel"
        );
        let crate::client::Body::Json(raw) = &cancel.request.body else {
            panic!("expected JSON body");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(raw.get()).unwrap(),
            json!({})
        );

        let propose = cal.decline(
            None,
            "ev-1",
            &RespondRequest {
                comment: Some("Busy".into()),
                send_response: Some(true),
                proposed_new_time: Some(TimeSlot {
                    start: DateTimeTimeZone::new(datetime(2026, 11, 20, 14, 0, 0, 0), "UTC"),
                    end: DateTimeTimeZone::new(datetime(2026, 11, 20, 15, 0, 0, 0), "UTC"),
                }),
            },
        );
        let crate::client::Body::Json(raw) = &propose.request.body else {
            panic!("expected JSON body");
        };
        let value: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
        assert_eq!(value["comment"], "Busy");
        assert_eq!(value["sendResponse"], true);
        assert_eq!(value["proposedNewTime"]["start"]["timeZone"], "UTC");

        let by_uid = cal.events_by_ical_uid(Some("cal-1"), "uid'1@example.com");
        assert_eq!(by_uid.request.url.path(), "/v1.0/me/calendars/cal-1/events");
        let filter: Vec<_> = by_uid
            .request
            .url
            .query_pairs()
            .filter(|(k, _)| k == "$filter")
            .map(|(_, v)| v.into_owned())
            .collect();
        assert_eq!(filter, ["iCalUId eq 'uid''1@example.com'"]);
    }
}
