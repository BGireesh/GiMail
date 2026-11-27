//! The seam between the store's calendar cache/outbox and a calendar backend (Google Calendar or
//! Microsoft Graph Calendar).

use std::sync::Arc;

use futures::future::BoxFuture;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use super::model::{
    Calendar, CalendarEvent, CalendarProviderKind, EventPatch, EventTime, FreeBusySlot, RsvpStatus,
};
use super::outbox::{Cut, InviteLinks};
use super::recurrence::Recurrence;
use crate::api::Failure;
use crate::connection::Outage;
use crate::error::{ProviderError, Result};
use crate::provider::Provider;

/// Reports the outcome of a primary-account calendar request on the store's mail connection
/// tracker. Secondary calendar accounts pass `None` so they never poison primary mail's outage
/// state.
pub(crate) fn report_primary_mail<T>(
    primary_mail: Option<&Arc<dyn Provider>>,
    token_url: &url::Url,
    result: &std::result::Result<T, Failure>,
) {
    let Some(mail) = primary_mail else {
        return;
    };
    match result {
        Ok(_) => mail.reached(false),
        Err(Failure::Offline(
            ProviderError::Gmail(gmail::Error::Http(http))
            | ProviderError::Outlook(outlook::Error::Http(http)),
        )) if http.url() == Some(token_url) => mail.down(Outage::Auth),
        Err(Failure::Offline(_)) => mail.down(Outage::Offline),
        Err(_) => {}
    }
}

/// Where a calendar account is reached, and the token provider that authorizes it.
#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "constructed once per calendar account added"
)]
pub enum CalendarRemote {
    /// A Google Calendar account.
    Gmail {
        /// Display email address of the Google account.
        email: String,
        /// Where Google's APIs are.
        endpoints: gmail::Endpoints,
        /// Access tokens for Google Calendar.
        tokens: Arc<dyn gmail::TokenProvider>,
    },
    /// A Microsoft Outlook / Microsoft 365 calendar account, through Microsoft Graph.
    Outlook {
        /// Display email address of the Microsoft account.
        email: String,
        /// Where Microsoft Graph is.
        endpoints: outlook::Endpoints,
        /// Access tokens for Graph Calendar.
        tokens: Arc<dyn outlook::TokenProvider>,
    },
}

impl CalendarRemote {
    /// A Google Calendar account for `email` at `endpoints`, reached with `tokens`.
    pub fn gmail(
        email: impl Into<String>,
        endpoints: gmail::Endpoints,
        tokens: impl gmail::TokenProvider + 'static,
    ) -> Self {
        Self::Gmail {
            email: email.into(),
            endpoints,
            tokens: Arc::new(tokens),
        }
    }

    /// A Microsoft Outlook calendar account for `email` at `endpoints`, reached with `tokens`.
    pub fn outlook(
        email: impl Into<String>,
        endpoints: outlook::Endpoints,
        tokens: impl outlook::TokenProvider + 'static,
    ) -> Self {
        Self::Outlook {
            email: email.into(),
            endpoints,
            tokens: Arc::new(tokens),
        }
    }

    /// Which calendar service this remote talks to.
    #[must_use]
    pub const fn provider(&self) -> CalendarProviderKind {
        match self {
            Self::Gmail { .. } => CalendarProviderKind::Google,
            Self::Outlook { .. } => CalendarProviderKind::Microsoft,
        }
    }

    /// Display email address of the account.
    #[must_use]
    pub fn email(&self) -> &str {
        match self {
            Self::Gmail { email, .. } | Self::Outlook { email, .. } => email,
        }
    }

    /// Canonical account key (`email` for Google, `"microsoft {email}"` for Microsoft).
    #[must_use]
    pub fn account_key(&self) -> String {
        self.provider().account_key(self.email())
    }
}

/// Result of syncing one calendar over a `[from, to)` window.
pub(crate) enum WindowEvents {
    /// Full window snapshot: replace all cached rows overlapping the window.
    Replace(Vec<CalendarEvent>),
    /// Incremental delta over the window: upsert `updated` and delete `removed` event IDs
    /// overlapping the window.
    Delta {
        updated: Vec<CalendarEvent>,
        removed: Vec<String>,
    },
}

/// A window sync result paired with an optional post-commit hook that advances provider sync state
/// only after SQLite commits cleanly.
pub(crate) struct WindowSync {
    pub events: WindowEvents,
    pub on_commit: Option<Box<dyn FnOnce() + Send>>,
}

/// Per-calendar free/busy results: `(calendar_id, Some(busy_slots) | None if unreadable)`.
pub(crate) type FreeBusyResults = Vec<(String, Option<Vec<FreeBusySlot>>)>;

/// Backend seam implemented by Google Calendar and Microsoft Graph Calendar.
pub(crate) trait CalendarProvider: Send + Sync {
    /// Which provider backs this account.
    fn kind(&self) -> CalendarProviderKind;

    /// Display email address of the account.
    fn email(&self) -> &str;

    /// Lists all non-hidden (plus primary) calendars on this account.
    fn list_calendars(&self) -> BoxFuture<'_, Result<Vec<Calendar>>>;

    /// Syncs events overlapping `[from, to)` for caching (full snapshot or incremental delta).
    fn sync_window<'a>(
        &'a self,
        calendar_id: &'a str,
        from: Timestamp,
        to: Timestamp,
        tz: &'a TimeZone,
    ) -> BoxFuture<'a, Result<WindowSync>>;

    /// Stateless listing of events overlapping `[from, to)` without modifying sync state.
    fn list_events<'a>(
        &'a self,
        calendar_id: &'a str,
        from: Timestamp,
        to: Timestamp,
        tz: &'a TimeZone,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>>>;

    /// Reads the recurrence rule of series master `series_id`, if representable.
    fn series_recurrence<'a>(
        &'a self,
        calendar_id: &'a str,
        series_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Recurrence>>>;

    /// Creates `event` on `calendar_id`, returning the server's representation (which may carry
    /// a server-assigned ID on Outlook).
    fn create_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event: &'a CalendarEvent,
        notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>>;

    /// Applies `patch` (and optional series wall-clock `shift`) to `event_id` on `calendar_id`.
    fn patch_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event_id: &'a str,
        patch: &'a EventPatch,
        shift: Option<(SignedDuration, SignedDuration)>,
        notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>>;

    /// Deletes (or cancels, when `comment` is supplied on Outlook) `event_id` on `calendar_id`.
    fn delete_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event_id: &'a str,
        notify: bool,
        comment: Option<&'a str>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Sends the user's RSVP `status` to `event` (or its series master when `all`). Returns the
    /// updated event when the provider echoes it back, or `None` when the caller should fold the
    /// local optimistic update into cache.
    fn respond<'a>(
        &'a self,
        calendar_id: &'a str,
        event: &'a CalendarEvent,
        status: RsvpStatus,
        all: bool,
    ) -> BoxFuture<'a, Result<Option<CalendarEvent>>>;

    /// Sends the user's RSVP `status` to an invitation identified by `invite.ical_uid`.
    fn respond_to_invite<'a>(
        &'a self,
        calendar_id: &'a str,
        invite: &'a CalendarEvent,
        status: RsvpStatus,
        links: Option<&'a InviteLinks>,
    ) -> BoxFuture<'a, Result<Option<CalendarEvent>>>;

    /// Ends `series_id` before `from`. Returns `true` if the entire series was deleted because
    /// `from` was at or before its first start.
    fn truncate_series<'a>(
        &'a self,
        calendar_id: &'a str,
        series_id: &'a str,
        from: Cut,
        notify: bool,
    ) -> BoxFuture<'a, Result<bool>>;

    /// Splits `series_id` at `from`, patching in place if `from` is at/before the first instance,
    /// otherwise creating the new series starting at `patch.start`/`start` and truncating the
    /// original master.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors CalendarChange::Split fields directly"
    )]
    fn split_series<'a>(
        &'a self,
        calendar_id: &'a str,
        series_id: &'a str,
        from: Cut,
        patch: &'a EventPatch,
        start: &'a EventTime,
        end: &'a EventTime,
        notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>>;

    /// Queries free/busy slots for `calendar_ids` over `[from, to)`.
    fn free_busy<'a>(
        &'a self,
        calendar_ids: &'a [String],
        from: Timestamp,
        to: Timestamp,
    ) -> BoxFuture<'a, Result<FreeBusyResults>>;

    /// Fetches `guest_email`'s events (or opaque busy blocks) over `[from, to)`, returning
    /// `Ok(None)` when this account has no visibility into the guest's calendar.
    fn attendee_events<'a>(
        &'a self,
        guest_email: &'a str,
        from: Timestamp,
        to: Timestamp,
    ) -> BoxFuture<'a, Result<Option<Vec<CalendarEvent>>>>;
}
