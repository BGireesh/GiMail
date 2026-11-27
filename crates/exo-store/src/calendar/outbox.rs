//! The calendar's changes until the provider has them: new events, edits, deletions and answers,
//! queued in the store so they work offline, outlast a quit, and flush once the connection is
//! back.
//!
//! [`Store::queue_calendar_change`] writes a row and [`Store::events`] shows it at once by
//! overlaying queued changes over the cached events. A change can be held for Undo, and
//! [`Store::unqueue_calendar_change`] takes back one that hasn't gone (or queues a compensating
//! deletion if a `Create` was already in flight, using the provider's server-assigned ID on
//! Outlook).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use toasty::{Executor, Json};

use super::model::{
    CalendarEvent, CalendarProviderKind, EventPatch, EventTime, RsvpStatus, StoredEvent,
};
use super::serde_compat::Compat;
use super::{delete_cached_event, put};
use crate::db::{self, CalendarChangeRow, EventRow, IN_LIST_CHUNK};
use crate::error::{Error, ProviderError, Result};
use crate::queue::{self, Disposition, FlushGate};
use crate::store::Store;

/// Per-store flush and in-flight row tracking for the calendar outbox, so a concurrent Undo or
/// Delete of an in-flight `Create` waits for the send to finish and queues a compensating
/// `Delete` using the server-assigned event ID instead of leaving a live event on the server.
#[derive(Default)]
pub(crate) struct CalendarOutboxSync {
    flush: FlushGate,
    in_flight: Mutex<InFlight>,
}

#[derive(Default)]
struct InFlight {
    sending: Option<u64>,
    /// Row ID of a watched in-flight `Create` -> `Some(server_id)` once committed on the server,
    /// `None` if it has not committed.
    watched_creates: HashMap<u64, Option<String>>,
}

/// Clears `in_flight.sending` if `send_calendar_change` exits early on an error or cancellation.
struct SendingGuard<'a> {
    sync: &'a CalendarOutboxSync,
    active: bool,
}

impl Drop for SendingGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let mut state = self
                .sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sending = None;
        }
    }
}

pub(crate) fn fresh_event_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = u128::try_from(Timestamp::now().as_nanosecond()).unwrap_or(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("e{:016x}{:08x}", nanos as u64, seq as u32)
}

/// A change to the calendar, queued for the provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalendarChange {
    /// Adds `event` (with a client-chosen local ID) to `calendar`, inviting its guests.
    Create {
        calendar: String,
        event: CalendarEvent,
    },
    /// Applies `patch` to event `id` on `calendar` (an instance or series master), telling guests
    /// when `notify`. For a series, `shift` moves its start and end by the instance's civil
    /// wall-clock delta. `shown` holds how affected cached events look optimistically.
    Edit {
        calendar: String,
        id: String,
        /// Series master ID when editing a recurring occurrence/exception, so it shares its
        /// series' queue without parsing provider ID strings.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_id: Option<String>,
        patch: EventPatch,
        notify: bool,
        shift: Option<(SignedDuration, SignedDuration)>,
        shown: Vec<CalendarEvent>,
    },
    /// Deletes (or cancels, when `comment` is set on Outlook) event `id` of `calendar`.
    Delete {
        calendar: String,
        id: String,
        /// Series master ID when deleting a single recurring occurrence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_id: Option<String>,
        notify: bool,
        /// Optional cancellation note for attendees (Outlook `POST .../cancel`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
    },
    /// Ends series `series` before its instance at `from`.
    Truncate {
        calendar: String,
        series: String,
        from: Cut,
        notify: bool,
    },
    /// Edits series `series` from its instance at `from` onward. The series ends before `from`
    /// and a new series (with `patch.id`) takes over with the remaining rule and `patch` overlaid,
    /// starting at `patch.start`/`start` and ending at `patch.end`/`end`.
    Split {
        calendar: String,
        series: String,
        from: Cut,
        patch: EventPatch,
        start: EventTime,
        end: EventTime,
        notify: bool,
        shown: Vec<CalendarEvent>,
    },
    /// Sets the user's RSVP `status` on `event` in `calendar` (or its whole series when `all`).
    Respond {
        calendar: String,
        event: CalendarEvent,
        status: RsvpStatus,
        all: bool,
    },
    /// Answers an email invitation on `calendar` (or `account`'s primary calendar) from
    /// conversation `thread`.
    RespondToInvite {
        calendar: String,
        /// Canonical account key owning the invite's calendar, when known (avoids assuming
        /// `calendar == email` on Outlook).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
        invite: CalendarEvent,
        status: RsvpStatus,
        thread: Option<String>,
        links: Option<InviteLinks>,
    },
}

/// The links of an invitation from a sender Google Calendar doesn't know, which it keeps off the
/// calendar: the one that marks the sender as known, and the answers' own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteLinks {
    pub mark_sender_known: String,
    pub accepted: Option<String>,
    pub declined: Option<String>,
    pub tentative: Option<String>,
}

impl InviteLinks {
    /// The link that answers `status`.
    #[must_use]
    pub fn answer(&self, status: RsvpStatus) -> Option<&str> {
        match status {
            RsvpStatus::Accepted => self.accepted.as_deref(),
            RsvpStatus::Declined => self.declined.as_deref(),
            RsvpStatus::Tentative => self.tentative.as_deref(),
            RsvpStatus::NeedsAction => None,
        }
    }
}

/// Why the provider refused a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The event it's for has been deleted or cancelled.
    Gone,
    /// An answer to an invite from a sender Google Calendar doesn't know, whose event it doesn't
    /// have ([`InviteLinks`]).
    UnknownSender,
    /// Only the event's organizer can make it.
    NonOrganizer,
    /// Anything else.
    Other,
}

/// Where "this and following" cuts a series: an all-day series at an instance's day, any other
/// series at the instance's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cut {
    /// The instance's day.
    Day(jiff::civil::Date),
    /// The instance's start.
    At(Timestamp),
}

/// A queued change whose turn it is, for [`Store::send_calendar_change`].
#[derive(Clone, Debug)]
pub struct QueuedCalendarChange {
    id: u64,
    /// What it was queued under.
    pub token: u64,
    pub change: CalendarChange,
}

/// How sending a queued change went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalendarOutcome {
    /// The provider has it, and the cache has the updated state.
    Sent,
    /// Offline, signed out, or rate limited, so the change waits and goes again later.
    Postponed,
    /// The provider refused it with this error, so it's given up and its overlay is gone.
    Refused { error: String, refusal: Refusal },
}

impl Cut {
    /// Whether an instance starting at `start` stays (falls strictly before the cut).
    #[must_use]
    pub fn keeps(self, start: &EventTime) -> bool {
        match self {
            Cut::Day(day) => start.date().is_none_or(|date| date < day),
            Cut::At(at) => start
                .timestamp(&TimeZone::UTC)
                .is_none_or(|start| start < at),
        }
    }

    /// The RFC 5545 `UNTIL` value ending before this cut: the day before (`YYYYMMDD`) for all-day,
    /// or the second before in UTC (`YYYYMMDDTHHMMSSZ`) for timed series.
    pub(crate) fn until(self) -> String {
        match self {
            Cut::Day(day) => day
                .yesterday()
                .unwrap_or(day)
                .strftime("%Y%m%d")
                .to_string(),
            Cut::At(at) => at
                .checked_sub(SignedDuration::from_secs(1))
                .unwrap_or(at)
                .strftime("%Y%m%dT%H%M%SZ")
                .to_string(),
        }
    }
}

/// Extracts the base series ID from a Google instance ID (`{series}_YYYYMMDD` or
/// `{series}_YYYYMMDDTHHMMSSZ`), leaving opaque Graph IDs (which may contain `_`) untouched.
fn google_instance_base(id: &str) -> &str {
    let Some((prefix, suffix)) = id.rsplit_once('_') else {
        return id;
    };
    let bytes = suffix.as_bytes();
    let is_date = bytes.len() == 8 && bytes.iter().all(u8::is_ascii_digit);
    let is_utc_datetime = bytes.len() == 16
        && bytes[8] == b'T'
        && bytes[15] == b'Z'
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[9..15].iter().all(u8::is_ascii_digit);
    if !prefix.is_empty() && (is_date || is_utc_datetime) {
        prefix
    } else {
        id
    }
}

impl CalendarChange {
    /// The calendar it changes.
    #[must_use]
    pub fn calendar(&self) -> &str {
        match self {
            Self::Create { calendar, .. }
            | Self::Edit { calendar, .. }
            | Self::Delete { calendar, .. }
            | Self::Truncate { calendar, .. }
            | Self::Split { calendar, .. }
            | Self::Respond { calendar, .. }
            | Self::RespondToInvite { calendar, .. } => calendar,
        }
    }

    /// Its FIFO queue key: `{calendar}/{series_or_id}` (or `{calendar}/{ical_uid}` for invites),
    /// using explicit `series_id`/`recurring_event_id` so Outlook occurrences share their
    /// `seriesMasterId` queue without splitting opaque Graph IDs on `_`.
    pub(crate) fn queue(&self) -> String {
        let key = match self {
            Self::Create { event, .. } => event.id.as_deref().unwrap_or_default(),
            Self::Edit { id, series_id, .. } | Self::Delete { id, series_id, .. } => series_id
                .as_deref()
                .unwrap_or_else(|| google_instance_base(id)),
            Self::Truncate { series, .. } | Self::Split { series, .. } => series.as_str(),
            Self::Respond { event, .. } => event
                .recurring_event_id
                .as_deref()
                .or_else(|| event.id.as_deref().map(google_instance_base))
                .unwrap_or_default(),
            Self::RespondToInvite { invite, .. } => {
                return format!(
                    "{}/{}",
                    self.calendar(),
                    invite.ical_uid.as_deref().unwrap_or_default()
                );
            }
        };
        format!("{}/{key}", self.calendar())
    }

    /// Rewrites references to optimistic `old_id` with server-assigned `new_id` after an Outlook
    /// `Create` succeeds.
    fn rewrite_id(&mut self, old_id: &str, new_id: &str) {
        let replace = |slot: &mut String| {
            if slot == old_id {
                *slot = new_id.to_owned();
            }
        };
        let replace_opt = |slot: &mut Option<String>| {
            if slot.as_deref() == Some(old_id) {
                *slot = Some(new_id.to_owned());
            }
        };
        let rewrite_event = |event: &mut CalendarEvent| {
            replace_opt(&mut event.id);
            replace_opt(&mut event.recurring_event_id);
        };
        match self {
            Self::Create { event, .. } => rewrite_event(event),
            Self::Edit {
                id,
                series_id,
                shown,
                ..
            } => {
                replace(id);
                replace_opt(series_id);
                for ev in shown {
                    rewrite_event(ev);
                }
            }
            Self::Delete { id, series_id, .. } => {
                replace(id);
                replace_opt(series_id);
            }
            Self::Truncate { series, .. } => replace(series),
            Self::Split { series, shown, .. } => {
                replace(series);
                for ev in shown {
                    rewrite_event(ev);
                }
            }
            Self::Respond { event, .. } => rewrite_event(event),
            Self::RespondToInvite { invite, .. } => rewrite_event(invite),
        }
    }

    /// Overlays this change onto `events`. A new event, or one an edit moves, joins them when
    /// `shown` says it belongs. For a new series, its instances starting in `window` join.
    /// `me_for` returns the email address for a given `calendar_id` (so RSVP matching works even
    /// when `calendar_id` is an opaque Outlook ID).
    pub(crate) fn apply<'a>(
        &self,
        events: &mut Vec<StoredEvent>,
        shown: &dyn Fn(&str, &CalendarEvent) -> bool,
        window: Option<(Timestamp, Timestamp)>,
        me_for: &'a dyn Fn(&str) -> &'a str,
    ) {
        let on = |stored: &StoredEvent, calendar: &str| stored.calendar_id == calendar;
        match self {
            Self::Create { calendar, event } if event.is_series_master() => {
                if let Some((from, to)) = window {
                    for instance in super::recurrence::instances(event, from, to) {
                        upsert(events, calendar, &instance, true, shown);
                    }
                }
            }
            Self::Create { calendar, event } => upsert(events, calendar, event, true, shown),
            Self::Edit {
                calendar,
                shown: changed,
                ..
            } => {
                for event in changed {
                    upsert(events, calendar, event, false, shown);
                }
            }
            Self::Delete { calendar, id, .. } => events.retain(|stored| {
                !on(stored, calendar)
                    || (stored.event.id.as_deref() != Some(id)
                        && stored.event.recurring_event_id.as_deref() != Some(id))
            }),
            Self::Truncate {
                calendar,
                series,
                from,
                ..
            } => truncate(events, calendar, series, *from),
            Self::Split {
                calendar,
                series,
                from,
                shown: changed,
                ..
            } => {
                truncate(events, calendar, series, *from);
                for event in changed {
                    upsert(events, calendar, event, false, shown);
                }
            }
            Self::Respond {
                calendar,
                event,
                status,
                all,
            } => {
                let series = event.recurring_event_id.as_deref().filter(|_| *all);
                let me = me_for(calendar);
                for stored in events.iter_mut().filter(|stored| on(stored, calendar)) {
                    if stored.event.id == event.id
                        || (series.is_some()
                            && stored.event.recurring_event_id.as_deref() == series)
                    {
                        stored.event.set_my_rsvp(me, *status);
                    }
                }
            }
            Self::RespondToInvite {
                calendar,
                account,
                invite,
                status,
                ..
            } => {
                let me = account
                    .as_deref()
                    .map(|key| CalendarProviderKind::parse_account_key(key).1)
                    .unwrap_or_else(|| me_for(calendar));
                for stored in events.iter_mut().filter(|stored| {
                    on(stored, calendar)
                        && stored.event.ical_uid.is_some()
                        && stored.event.ical_uid == invite.ical_uid
                }) {
                    stored.event.set_my_rsvp(me, *status);
                }
            }
        }
    }
}

/// `events` without the instances of series `series` on `calendar` from `from` onward.
fn truncate(events: &mut Vec<StoredEvent>, calendar: &str, series: &str, from: Cut) {
    events.retain(|stored| {
        stored.calendar_id != calendar
            || stored.event.recurring_event_id.as_deref() != Some(series)
            || stored
                .event
                .start
                .as_ref()
                .is_none_or(|start| from.keeps(start))
    });
}

/// `event` in place of its copy in `events`, or added when `shown` says so.
fn upsert(
    events: &mut Vec<StoredEvent>,
    calendar: &str,
    event: &CalendarEvent,
    unsynced: bool,
    shown: &dyn Fn(&str, &CalendarEvent) -> bool,
) {
    match events
        .iter_mut()
        .find(|stored| stored.calendar_id == calendar && stored.event.id == event.id)
    {
        Some(stored) => stored.event = event.clone(),
        None if shown(calendar, event) => events.push(StoredEvent {
            calendar_id: calendar.to_owned(),
            event: event.clone(),
            unsynced,
        }),
        None => {}
    }
}

/// Every queued change, oldest first.
pub(crate) async fn queued(exec: &mut dyn Executor) -> Result<Vec<CalendarChange>> {
    let rows = CalendarChangeRow::all()
        .order_by(CalendarChangeRow::fields().id().asc())
        .exec(exec)
        .await?;
    Ok(rows.into_iter().map(|row| row.change.0.0).collect())
}

enum MakeResult {
    Done,
    Created {
        calendar: String,
        local_id: Option<String>,
        created: Box<CalendarEvent>,
    },
}

impl Store {
    /// Queues `change` under `token` for the calendar provider, shown by [`events`](Self::events)
    /// at once and sent once `hold` has passed. Deleting a new event still queued takes it back
    /// instead; if that new event is already in flight, it waits for the send and queues a
    /// compensating deletion using the server-assigned ID if needed.
    ///
    /// # Errors
    /// On database failures.
    pub async fn queue_calendar_change(
        &self,
        token: u64,
        mut change: CalendarChange,
        hold: Option<Timestamp>,
    ) -> Result<()> {
        if let CalendarChange::Split { patch, .. } = &mut change
            && patch.id.is_none()
        {
            patch.id = Some(fresh_event_id());
        }
        let sync = &self.inner.calendar_outbox;
        let queue = change.queue();
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        if let Some(row_id) = find_queued_create(write.exec(), &queue, &change).await? {
            let is_sending = {
                let mut state = sync
                    .in_flight
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if state.sending == Some(row_id) {
                    state.watched_creates.entry(row_id).or_insert(None);
                    true
                } else {
                    false
                }
            };
            if !is_sending {
                CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&queue))
                    .delete()
                    .exec(write.exec())
                    .await?;
                return write.publish().await;
            }
            drop(write);
            let _flush = sync.flush.lock().await;
            let server_id = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .watched_creates
                .remove(&row_id)
                .flatten();
            let mut write = self.inner.begin(&mut db).await?;
            CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&queue))
                .delete()
                .exec(write.exec())
                .await?;
            if let Some(actual_id) = server_id {
                if let CalendarChange::Delete { id, .. } = &change {
                    let old_id = id.clone();
                    change.rewrite_id(&old_id, &actual_id);
                }
                let new_queue = change.queue();
                if new_queue != queue {
                    CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&new_queue))
                        .delete()
                        .exec(write.exec())
                        .await?;
                }
                CalendarChangeRow::create()
                    .token(token)
                    .queue(new_queue)
                    .change(Json(Compat(change)))
                    .failures(0)
                    .not_before(hold)
                    .exec(write.exec())
                    .await?;
            }
            return write.publish().await;
        }
        CalendarChangeRow::create()
            .token(token)
            .queue(queue)
            .change(Json(Compat(change)))
            .failures(0)
            .not_before(hold)
            .exec(write.exec())
            .await?;
        write.publish().await
    }

    /// Takes back the change queued under `token`, unless it's gone. For a new event, it also
    /// takes back follow-up changes queued for it; if the creation is already in flight and lands
    /// on the provider, a compensating deletion is queued using the server-assigned ID. Returns
    /// whether the change was taken back.
    ///
    /// # Errors
    /// On database failures.
    pub async fn unqueue_calendar_change(&self, token: u64) -> Result<bool> {
        let sync = &self.inner.calendar_outbox;
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let rows = CalendarChangeRow::filter(CalendarChangeRow::fields().token().eq(token))
            .exec(write.exec())
            .await?;
        if rows.is_empty() {
            return Ok(false);
        }
        let in_flight = {
            let mut state = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            rows.iter().find_map(|row| {
                if state.sending != Some(row.id) {
                    return None;
                }
                if matches!(row.change.0.0, CalendarChange::Create { .. }) {
                    state.watched_creates.entry(row.id).or_insert(None);
                }
                Some((row.id, row.queue.clone(), row.change.0.0.clone()))
            })
        };
        if let Some((row_id, queue_key, in_flight_change)) = in_flight {
            drop(write);
            let _flush = sync.flush.lock().await;
            let server_id = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .watched_creates
                .remove(&row_id)
                .flatten();
            if let Some(actual_id) = server_id
                && let CalendarChange::Create { calendar, event } = in_flight_change
            {
                let me = self.email_for_calendar(&calendar).await?;
                let notify = event.has_other_guests(&me);
                let compensating = CalendarChange::Delete {
                    calendar,
                    id: actual_id,
                    series_id: None,
                    notify,
                    comment: None,
                };
                let new_queue = compensating.queue();
                let mut write = self.inner.begin(&mut db).await?;
                CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&queue_key))
                    .delete()
                    .exec(write.exec())
                    .await?;
                if new_queue != queue_key {
                    CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&new_queue))
                        .delete()
                        .exec(write.exec())
                        .await?;
                }
                CalendarChangeRow::create()
                    .token(0)
                    .queue(new_queue)
                    .change(Json(Compat(compensating)))
                    .failures(0)
                    .not_before(None)
                    .exec(write.exec())
                    .await?;
                write.publish().await?;
                return Ok(true);
            }
            let mut write = self.inner.begin(&mut db).await?;
            let remaining =
                CalendarChangeRow::filter(CalendarChangeRow::fields().token().eq(token))
                    .exec(write.exec())
                    .await?;
            if remaining.is_empty() {
                return Ok(false);
            }
            delete_unstarted_rows(write.exec(), &remaining, token).await?;
            write.publish().await?;
            return Ok(true);
        }
        delete_unstarted_rows(write.exec(), &rows, token).await?;
        write.publish().await?;
        Ok(true)
    }

    /// The queued changes whose turn it is, oldest first: each queue's first change unless it's
    /// held or backing off. With `held_too`, returns each queue's first change regardless of hold.
    ///
    /// # Errors
    /// On database failures.
    pub async fn due_calendar_changes(
        &self,
        held_too: bool,
    ) -> Result<(Vec<QueuedCalendarChange>, Option<Timestamp>)> {
        let mut db = self.inner.db.clone();
        let rows = CalendarChangeRow::all()
            .order_by(CalendarChangeRow::fields().id().asc())
            .exec(&mut db)
            .await?;
        let batch = queue::select_due(
            rows,
            Timestamp::now(),
            held_too,
            |row| row.queue.clone(),
            |row| row.not_before,
        );
        let due = batch
            .due
            .into_iter()
            .map(|row| QueuedCalendarChange {
                id: row.id,
                token: row.token,
                change: row.change.0.0,
            })
            .collect();
        Ok((due, batch.retry_at))
    }

    /// How many queued calendar changes haven't reached the provider yet.
    ///
    /// # Errors
    /// On database failures.
    pub async fn calendar_pending(&self) -> Result<usize> {
        let mut db = self.inner.db.clone();
        Ok(
            usize::try_from(CalendarChangeRow::all().count().exec(&mut db).await?)
                .unwrap_or(usize::MAX),
        )
    }

    /// Sends `queued` to the calendar provider, updates the cache (including atomic local-to-server
    /// ID rewriting when Outlook assigns a new event ID on `Create`), and drops the change from
    /// the queue. Otherwise postpones or gives up on refusal.
    ///
    /// # Errors
    /// On database failures.
    pub async fn send_calendar_change(
        &self,
        queued: &QueuedCalendarChange,
    ) -> Result<CalendarOutcome> {
        let sync = &self.inner.calendar_outbox;
        let _flush = sync.flush.lock().await;
        let current_failures = {
            let mut db = self.inner.db.clone();
            let mut write = self.inner.begin(&mut db).await?;
            let Some(row) = CalendarChangeRow::filter_by_id(queued.id)
                .first()
                .exec(write.exec())
                .await?
            else {
                return Ok(CalendarOutcome::Sent);
            };
            sync.in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sending = Some(queued.id);
            row.failures
        };
        let mut guard = SendingGuard { sync, active: true };
        let finish = |created_id: Option<String>, guard: &mut SendingGuard<'_>| {
            guard.active = false;
            let mut state = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sending = None;
            if let Some(id) = created_id
                && let Some(slot) = state.watched_creates.get_mut(&queued.id)
            {
                *slot = Some(id);
            }
        };
        let failure = match self.make(&queued.change).await {
            Ok(MakeResult::Done) => {
                let outcome = self.dequeue(queued.id, CalendarOutcome::Sent).await?;
                finish(None, &mut guard);
                return Ok(outcome);
            }
            Ok(MakeResult::Created {
                calendar,
                local_id,
                created,
            }) => {
                let server_id = created.id.clone().or_else(|| local_id.clone());
                let old_queue = queued.change.queue();
                let mut db = self.inner.db.clone();
                let mut write = self.inner.begin(&mut db).await?;
                CalendarChangeRow::filter_by_id(queued.id)
                    .delete()
                    .exec(write.exec())
                    .await?;
                if let (Some(old_id), Some(new_id)) = (local_id.as_deref(), server_id.as_deref())
                    && old_id != new_id
                {
                    delete_cached_event(write.exec(), &calendar, old_id).await?;
                    let followups = CalendarChangeRow::filter(
                        CalendarChangeRow::fields().queue().eq(&old_queue),
                    )
                    .order_by(CalendarChangeRow::fields().id().asc())
                    .exec(write.exec())
                    .await?;
                    for row in followups {
                        let mut updated_change = row.change.0.0;
                        updated_change.rewrite_id(old_id, new_id);
                        let new_queue = updated_change.queue();
                        CalendarChangeRow::update_by_id(row.id)
                            .queue(new_queue)
                            .change(Json(Compat(updated_change)))
                            .exec(write.exec())
                            .await?;
                    }
                }
                if !created.is_series_master() {
                    put(write.exec(), &calendar, &created).await?;
                }
                write.publish().await?;
                finish(server_id, &mut guard);
                return Ok(CalendarOutcome::Sent);
            }
            Err(failure) => failure,
        };
        let failures = current_failures.saturating_add(1);
        let not_before = match queue::dispose_error(self.inner.backoff, failure, failures) {
            Ok(Disposition::Postponed(until)) => until,
            Ok(Disposition::Refused(error)) => {
                log::warn!("Calendar refused a change, giving it up: {error}");
                let refusal = classify_refusal(&error);
                let outcome = self
                    .dequeue(
                        queued.id,
                        CalendarOutcome::Refused {
                            error: error.to_string(),
                            refusal,
                        },
                    )
                    .await?;
                finish(None, &mut guard);
                return Ok(outcome);
            }
            Err(error @ (Error::EventCancelled | Error::UnknownSender)) => {
                log::warn!("giving up an answer to an invite: {error}");
                let refusal = if matches!(error, Error::EventCancelled) {
                    Refusal::Gone
                } else {
                    Refusal::UnknownSender
                };
                let outcome = self
                    .dequeue(
                        queued.id,
                        CalendarOutcome::Refused {
                            error: error.to_string(),
                            refusal,
                        },
                    )
                    .await?;
                finish(None, &mut guard);
                return Ok(outcome);
            }
            Err(error) => return Err(error),
        };
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        CalendarChangeRow::update_by_id(queued.id)
            .failures(failures)
            .not_before(not_before)
            .exec(write.exec())
            .await?;
        write.commit().await?;
        finish(None, &mut guard);
        Ok(CalendarOutcome::Postponed)
    }

    /// Drops change `id` from its queue, which ended in `outcome`.
    async fn dequeue(&self, id: u64, outcome: CalendarOutcome) -> Result<CalendarOutcome> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        CalendarChangeRow::filter_by_id(id)
            .delete()
            .exec(write.exec())
            .await?;
        write.publish().await?;
        Ok(outcome)
    }

    /// Sends `change` through the owning [`CalendarProvider`] and updates the cache.
    async fn make(&self, change: &CalendarChange) -> Result<MakeResult> {
        match change {
            CalendarChange::Create { calendar, event } => {
                let provider = self.provider_for_calendar(calendar).await?;
                let created = provider.create_event(calendar, event, true).await?;
                Ok(MakeResult::Created {
                    calendar: calendar.clone(),
                    local_id: event.id.clone(),
                    created: Box::new(created),
                })
            }
            CalendarChange::Edit {
                calendar,
                id,
                patch,
                notify,
                shift,
                ..
            } => {
                let provider = self.provider_for_calendar(calendar).await?;
                let answer = provider
                    .patch_event(calendar, id, patch, *shift, *notify)
                    .await?;
                if answer.is_series_master() {
                    self.fold_into_cache(change).await?;
                } else {
                    let mut db = self.inner.db.clone();
                    let mut write = self.inner.begin(&mut db).await?;
                    put(write.exec(), calendar, &answer).await?;
                    write.publish().await?;
                }
                Ok(MakeResult::Done)
            }
            CalendarChange::Delete {
                calendar,
                id,
                notify,
                comment,
                ..
            } => {
                let provider = self.provider_for_calendar(calendar).await?;
                provider
                    .delete_event(calendar, id, *notify, comment.as_deref())
                    .await?;
                let mut db = self.inner.db.clone();
                let mut write = self.inner.begin(&mut db).await?;
                delete_cached_event(write.exec(), calendar, id).await?;
                write.publish().await?;
                Ok(MakeResult::Done)
            }
            CalendarChange::Split {
                calendar,
                series,
                from,
                patch,
                start,
                end,
                notify,
                ..
            } => {
                let provider = self.provider_for_calendar(calendar).await?;
                let created = provider
                    .split_series(calendar, series, *from, patch, start, end, *notify)
                    .await?;
                // Only persist optimistic `shown` rows when the provider honored our client ID
                // (Google true-split). When Graph assigns opaque server IDs or split patched the
                // master in place, fold only the truncation of `series` so fake-ID ghost rows are
                // never left behind in SQLite under delta sync.
                if patch.id.is_some() && created.id.as_deref() == patch.id.as_deref() {
                    self.fold_into_cache_for(change, calendar, true).await?;
                } else {
                    let truncate_only = CalendarChange::Truncate {
                        calendar: calendar.clone(),
                        series: series.clone(),
                        from: *from,
                        notify: *notify,
                    };
                    self.fold_into_cache_for(&truncate_only, calendar, false)
                        .await?;
                }
                Ok(MakeResult::Done)
            }
            CalendarChange::Truncate {
                calendar,
                series,
                from,
                notify,
            } => {
                let provider = self.provider_for_calendar(calendar).await?;
                let deleted_all = provider
                    .truncate_series(calendar, series, *from, *notify)
                    .await?;
                if deleted_all {
                    let mut db = self.inner.db.clone();
                    let mut write = self.inner.begin(&mut db).await?;
                    delete_cached_event(write.exec(), calendar, series).await?;
                    write.publish().await?;
                } else {
                    self.fold_into_cache(change).await?;
                }
                Ok(MakeResult::Done)
            }
            CalendarChange::Respond {
                calendar,
                event,
                status,
                all,
            } => {
                let provider = self.provider_for_calendar(calendar).await?;
                match provider.respond(calendar, event, *status, *all).await? {
                    Some(updated) => {
                        let mut db = self.inner.db.clone();
                        let mut write = self.inner.begin(&mut db).await?;
                        put(write.exec(), calendar, &updated).await?;
                        write.publish().await?;
                    }
                    None => self.fold_into_cache(change).await?,
                }
                Ok(MakeResult::Done)
            }
            CalendarChange::RespondToInvite {
                calendar,
                account,
                invite,
                status,
                thread,
                links,
            } => {
                let (provider, resolved_cal) = match account.as_deref() {
                    Some(key) => {
                        let provider = self.provider_for_account(key)?;
                        let cal = if calendar == "primary"
                            || (provider.kind() == CalendarProviderKind::Microsoft
                                && calendar.eq_ignore_ascii_case(provider.email()))
                        {
                            self.primary_calendar_id(key)
                                .await?
                                .unwrap_or_else(|| "primary".to_owned())
                        } else {
                            calendar.clone()
                        };
                        (provider, cal)
                    }
                    None => (
                        self.provider_for_calendar(calendar).await?,
                        calendar.clone(),
                    ),
                };
                match provider
                    .respond_to_invite(&resolved_cal, invite, *status, links.as_ref())
                    .await?
                {
                    Some(created) if !created.is_series_master() => {
                        let mut db = self.inner.db.clone();
                        let mut write = self.inner.begin(&mut db).await?;
                        put(write.exec(), &resolved_cal, &created).await?;
                        write.publish().await?;
                    }
                    _ => {
                        let resolved_change = CalendarChange::RespondToInvite {
                            calendar: resolved_cal.clone(),
                            account: account.clone(),
                            invite: invite.clone(),
                            status: *status,
                            thread: thread.clone(),
                            links: links.clone(),
                        };
                        self.fold_into_cache_for(&resolved_change, &resolved_cal, false)
                            .await?;
                    }
                }
                Ok(MakeResult::Done)
            }
        }
    }

    /// Folds `change` into the cached events using indexed queries (`(calendar_id, id)` and
    /// `(calendar_id, series_id)`) without deserializing unrelated events on the calendar.
    async fn fold_into_cache(&self, change: &CalendarChange) -> Result<()> {
        let include_new = matches!(change, CalendarChange::Split { .. });
        self.fold_into_cache_for(change, change.calendar(), include_new)
            .await
    }

    async fn fold_into_cache_for(
        &self,
        change: &CalendarChange,
        calendar: &str,
        include_new: bool,
    ) -> Result<()> {
        let me = self.email_for_calendar(calendar).await?;
        let mut db = self.inner.db.clone();
        let rows = affected_rows(&mut db, calendar, change).await?;
        let before: HashMap<String, CalendarEvent> = rows
            .into_iter()
            .map(|row| (row.id, row.event.0.0))
            .collect();
        let mut after: Vec<StoredEvent> = before
            .values()
            .map(|event| StoredEvent {
                calendar_id: calendar.to_owned(),
                event: event.clone(),
                unsynced: false,
            })
            .collect();
        change.apply(&mut after, &|_, _| include_new, None, &|_| me.as_str());
        let kept: HashSet<&str> = after
            .iter()
            .filter_map(|stored| stored.event.id.as_deref())
            .collect();
        let gone: Vec<String> = before
            .keys()
            .filter(|id| !kept.contains(id.as_str()))
            .cloned()
            .collect();
        let mut write = self.inner.begin(&mut db).await?;
        for ids in gone.chunks(IN_LIST_CHUNK) {
            EventRow::filter(
                EventRow::fields()
                    .calendar_id()
                    .eq(calendar)
                    .and(EventRow::fields().id().in_list(ids)),
            )
            .delete()
            .exec(write.exec())
            .await?;
        }
        for stored in &after {
            if stored.event.id.as_ref().and_then(|id| before.get(id)) != Some(&stored.event) {
                put(write.exec(), calendar, &stored.event).await?;
            }
        }
        write.publish().await
    }
}

/// Loads only the cached [`EventRow`]s that `change` could read or mutate on `calendar`, avoiding
/// full-calendar deserialization.
async fn affected_rows(
    exec: &mut dyn Executor,
    calendar: &str,
    change: &CalendarChange,
) -> Result<Vec<EventRow>> {
    let fields = EventRow::fields();
    let mut by_id: HashMap<String, EventRow> = HashMap::new();
    let mut ids: Vec<String> = Vec::new();
    let mut series_keys: Vec<String> = Vec::new();

    match change {
        CalendarChange::Create { event, .. } => {
            if let Some(id) = &event.id {
                ids.push(id.clone());
            }
        }
        CalendarChange::Edit {
            id,
            series_id,
            shown,
            ..
        } => {
            ids.push(id.clone());
            series_keys.push(series_id.clone().unwrap_or_else(|| id.clone()));
            for ev in shown {
                if let Some(ev_id) = &ev.id {
                    ids.push(ev_id.clone());
                }
                if let Some(series) = &ev.recurring_event_id {
                    series_keys.push(series.clone());
                }
            }
        }
        CalendarChange::Delete { id, .. } => {
            ids.push(id.clone());
            series_keys.push(id.clone());
        }
        CalendarChange::Truncate { series, .. } => {
            ids.push(series.clone());
            series_keys.push(series.clone());
        }
        CalendarChange::Split { series, shown, .. } => {
            ids.push(series.clone());
            series_keys.push(series.clone());
            for ev in shown {
                if let Some(ev_id) = &ev.id {
                    ids.push(ev_id.clone());
                }
            }
        }
        CalendarChange::Respond { event, all, .. } => {
            if let Some(id) = &event.id {
                ids.push(id.clone());
            }
            if *all && let Some(series) = &event.recurring_event_id {
                ids.push(series.clone());
                series_keys.push(series.clone());
            }
        }
        CalendarChange::RespondToInvite { invite, .. } => {
            if let Some(uid) = &invite.ical_uid {
                let matched = db::query_col::<String>(
                    exec,
                    toasty::sql::query(
                        "SELECT id FROM events WHERE calendar_id = ?1 AND (json_extract(event, '$.data.icalUid') = ?2 OR json_extract(event, '$.iCalUID') = ?2)",
                    )
                    .bind(calendar)
                    .bind(uid.as_str()),
                )
                .await?;
                ids.extend(matched);
            }
        }
    }

    ids.sort();
    ids.dedup();
    for chunk in ids.chunks(IN_LIST_CHUNK) {
        for row in EventRow::filter(
            fields
                .calendar_id()
                .eq(calendar)
                .and(fields.id().in_list(chunk)),
        )
        .exec(&mut *exec)
        .await?
        {
            by_id.insert(row.id.clone(), row);
        }
    }

    series_keys.sort();
    series_keys.dedup();
    for series in &series_keys {
        for row in EventRow::filter(
            fields
                .calendar_id()
                .eq(calendar)
                .and(fields.series_id().eq(series.as_str())),
        )
        .exec(&mut *exec)
        .await?
        {
            by_id.insert(row.id.clone(), row);
        }
    }

    Ok(by_id.into_values().collect())
}

fn classify_refusal(error: &ProviderError) -> Refusal {
    let organizer_only = match error {
        ProviderError::Gmail(gmail::Error::Api(api)) => api
            .errors
            .iter()
            .any(|item| item.reason == "forbiddenForNonOrganizer"),
        ProviderError::Outlook(outlook::Error::Api(api)) => {
            api.error.message.to_ascii_lowercase().contains("organizer")
        }
        _ => false,
    };
    match error.status() {
        Some(404 | 410) => Refusal::Gone,
        Some(400 | 403) if organizer_only => Refusal::NonOrganizer,
        _ => Refusal::Other,
    }
}

/// Returns the row ID of a queued `Create` in `queue` matching a `Delete` `change`, if any.
async fn find_queued_create(
    exec: &mut dyn Executor,
    queue: &str,
    change: &CalendarChange,
) -> Result<Option<u64>> {
    let CalendarChange::Delete { calendar, id, .. } = change else {
        return Ok(None);
    };
    let rows = CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(queue))
        .exec(&mut *exec)
        .await?;
    Ok(rows.into_iter().find_map(|row| match &row.change.0.0 {
        CalendarChange::Create {
            calendar: on,
            event,
        } if on == calendar && event.id.as_deref() == Some(id.as_str()) => Some(row.id),
        _ => None,
    }))
}

async fn delete_unstarted_rows(
    exec: &mut dyn Executor,
    rows: &[CalendarChangeRow],
    token: u64,
) -> Result<()> {
    for row in rows
        .iter()
        .filter(|row| matches!(row.change.0.0, CalendarChange::Create { .. }))
    {
        let queue = CalendarChangeRow::fields().queue().eq(&row.queue);
        CalendarChangeRow::filter(queue.and(CalendarChangeRow::fields().id().gt(row.id)))
            .delete()
            .exec(&mut *exec)
            .await?;
    }
    CalendarChangeRow::filter(CalendarChangeRow::fields().token().eq(token))
        .delete()
        .exec(exec)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use jiff::civil::{Date, date};

    use super::*;
    use crate::calendar::google::{apply_shift, ended, remainder};
    use crate::calendar::model::{EventAttendee, EventReminders, ShowAs, Visibility};
    use crate::calendar::recurrence::{End, Frequency, Recurrence};

    fn at(text: &str) -> EventTime {
        EventTime::Timed {
            at: text.parse().expect("time"),
            time_zone: Some("America/New_York".into()),
        }
    }

    fn on_date(d: Date) -> EventTime {
        EventTime::AllDay(d)
    }

    #[test]
    fn a_series_moves_by_its_instance() {
        let master = CalendarEvent {
            start: Some(at("2026-11-03T11:00:00-05:00")),
            end: Some(at("2026-11-03T11:30:00-05:00")),
            ..CalendarEvent::default()
        };
        let mut patch = EventPatch::default();
        apply_shift(
            &mut patch,
            &master,
            (SignedDuration::from_mins(60), SignedDuration::from_mins(90)),
        );
        assert_eq!(patch.start, Some(at("2026-11-03T12:00:00-05:00")));
        assert_eq!(patch.end, Some(at("2026-11-03T13:00:00-05:00")));
    }

    #[test]
    fn series_shifts_preserve_wall_clock_across_dst_and_support_all_day_and_toggles() {
        // Master starts before fall-back (EDT, -04:00); shifting wall-clock +1h keeps 11:00 EDT.
        let master = CalendarEvent {
            start: Some(at("2026-10-27T10:00:00-04:00")),
            end: Some(at("2026-10-27T10:30:00-04:00")),
            raw_recurrence: vec!["RRULE:FREQ=WEEKLY;UNTIL=20261202T045959Z".into()],
            ..CalendarEvent::default()
        };
        let mut patch = EventPatch::default();
        apply_shift(
            &mut patch,
            &master,
            (SignedDuration::from_hours(1), SignedDuration::from_hours(1)),
        );
        assert_eq!(patch.start, Some(at("2026-10-27T11:00:00-04:00")));

        // Toggling timed -> all-day on `Scope::All` converts master to date and rewrites `UNTIL`.
        let mut to_all_day = EventPatch {
            start: Some(on_date(date(2026, 11, 17))),
            end: Some(on_date(date(2026, 11, 18))),
            ..EventPatch::default()
        };
        apply_shift(
            &mut to_all_day,
            &master,
            (
                SignedDuration::from_hours(-10),
                SignedDuration::from_mins(13 * 60 + 30),
            ),
        );
        assert_eq!(to_all_day.start, Some(on_date(date(2026, 10, 27))));
        assert_eq!(to_all_day.end, Some(on_date(date(2026, 10, 28))));
        assert_eq!(
            to_all_day.raw_recurrence.as_deref(),
            Some(["RRULE:FREQ=WEEKLY;UNTIL=20261201".to_owned()].as_slice()),
            "rewrites UNTIL in America/New_York without +1 day UTC drift"
        );

        // All-day series shifts civil dates.
        assert_eq!(
            on_date(date(2026, 10, 28)).shift_wall_clock(
                SignedDuration::from_hours(24),
                None,
                None
            ),
            Some(on_date(date(2026, 10, 29)))
        );
    }

    #[test]
    fn remainder_clears_attendees_when_requested_and_keeps_master_attendees_otherwise() {
        let guest = EventAttendee {
            email: Some("lena@brightmoor.co".into()),
            ..EventAttendee::default()
        };
        let master = CalendarEvent {
            id: Some("series1".into()),
            summary: Some("Sync".into()),
            start: Some(at("2026-11-10T10:00:00-05:00")),
            end: Some(at("2026-11-10T10:30:00-05:00")),
            attendees: vec![guest.clone()],
            raw_recurrence: vec!["RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL=20261209T045959Z".into()],
            ..CalendarEvent::default()
        };
        let cut = Cut::At("2026-11-17T15:00:00Z".parse().expect("time"));
        let untouched = remainder(
            &master,
            &EventPatch {
                id: Some("series2".into()),
                summary: Some("Sync, renamed".into()),
                ..EventPatch::default()
            },
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert_eq!(untouched.attendees, vec![guest]);
        assert_eq!(
            untouched.raw_recurrence,
            ["RRULE:FREQ=WEEKLY;BYDAY=TU;WKST=SU;UNTIL=20261209T045959Z"],
            "UNTIL stays Dec 8 in America/New_York rather than drifting +1 day"
        );

        let cleared = remainder(
            &master,
            &EventPatch {
                id: Some("series3".into()),
                attendees: Some(Vec::new()),
                ..EventPatch::default()
            },
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert!(cleared.attendees.is_empty());
    }

    #[test]
    fn a_series_ends_before_an_instance() {
        let from = Cut::At("2026-11-24T16:00:00Z".parse().expect("time"));
        let rules = ended(
            &[
                "RRULE:FREQ=WEEKLY;COUNT=10;BYDAY=TU".to_owned(),
                "EXDATE:20261110T160000Z".to_owned(),
            ],
            from,
        );
        assert_eq!(
            rules,
            [
                "RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL=20261124T155959Z",
                "EXDATE:20261110T160000Z"
            ]
        );
        assert_eq!(
            ended(
                &["RRULE:FREQ=WEEKLY".to_owned()],
                Cut::Day(Date::constant(2026, 11, 17))
            ),
            ["RRULE:FREQ=WEEKLY;UNTIL=20261116"],
            "an all-day series ends on a date"
        );
    }

    #[test]
    fn queued_changes_show_on_the_calendar() {
        let event = |id: &str, series: Option<&str>| StoredEvent {
            calendar_id: "me".into(),
            event: CalendarEvent {
                id: Some(id.into()),
                recurring_event_id: series.map(Into::into),
                ..CalendarEvent::default()
            },
            unsynced: false,
        };
        let everywhere = |_: &str, _: &CalendarEvent| true;
        let me_for = |_: &str| "me@example.com";
        let mut events = vec![
            event("a", None),
            event("s_1", Some("s")),
            event("s_2", Some("s")),
        ];
        CalendarChange::Delete {
            calendar: "me".into(),
            id: "s".into(),
            series_id: None,
            notify: false,
            comment: None,
        }
        .apply(&mut events, &everywhere, None, &me_for);
        assert_eq!(events.len(), 1, "a series takes its instances");
        let renamed = CalendarEvent {
            id: Some("a".into()),
            summary: Some("Renamed".into()),
            ..CalendarEvent::default()
        };
        CalendarChange::Edit {
            calendar: "me".into(),
            id: "a".into(),
            series_id: None,
            patch: EventPatch::default(),
            notify: false,
            shift: None,
            shown: vec![renamed.clone()],
        }
        .apply(&mut events, &everywhere, None, &me_for);
        assert_eq!(events[0].event, renamed);
        let new = CalendarChange::Create {
            calendar: "me".into(),
            event: CalendarEvent {
                id: Some("b".into()),
                ..CalendarEvent::default()
            },
        };
        new.apply(&mut events, &|_, _| false, None, &me_for);
        assert_eq!(events.len(), 1, "a new event off the days shown isn't");
        new.apply(&mut events, &everywhere, None, &me_for);
        assert_eq!(
            (events.len(), events[1].unsynced),
            (2, true),
            "a new event on them is, and Calendar hasn't got it"
        );
    }

    #[test]
    fn an_instance_shares_its_series_queue_without_splitting_opaque_ids() {
        let respond = |id: &str, series: Option<&str>| CalendarChange::Respond {
            calendar: "cal".into(),
            event: CalendarEvent {
                id: Some(id.into()),
                recurring_event_id: series.map(Into::into),
                ..CalendarEvent::default()
            },
            status: RsvpStatus::Accepted,
            all: false,
        };
        let delete = CalendarChange::Delete {
            calendar: "cal".into(),
            id: "s".into(),
            series_id: None,
            notify: false,
            comment: None,
        };
        assert_eq!(respond("s_20261117T150000Z", None).queue(), delete.queue());
        assert_ne!(respond("t", None).queue(), delete.queue());

        // Outlook occurrences share their `seriesMasterId` queue via `series_id`, while
        // distinct opaque IDs containing `_` do not collide.
        let outlook_del_series = CalendarChange::Delete {
            calendar: "AAMkCal".into(),
            id: "AAMk_master".into(),
            series_id: None,
            notify: false,
            comment: None,
        };
        let outlook_edit_occ = CalendarChange::Edit {
            calendar: "AAMkCal".into(),
            id: "AAMk_occ_1".into(),
            series_id: Some("AAMk_master".into()),
            patch: EventPatch::default(),
            notify: false,
            shift: None,
            shown: Vec::new(),
        };
        let outlook_other = CalendarChange::Delete {
            calendar: "AAMkCal".into(),
            id: "AAMk_other".into(),
            series_id: None,
            notify: false,
            comment: None,
        };
        assert_eq!(outlook_edit_occ.queue(), outlook_del_series.queue());
        assert_ne!(outlook_other.queue(), outlook_del_series.queue());
    }

    #[tokio::test]
    async fn in_flight_creates_wait_and_queue_compensating_deletes_with_server_id_and_splits_get_deterministic_ids()
     {
        let dir = std::env::temp_dir().join(format!(
            "exo-cal-outbox-test-{}-{}",
            std::process::id(),
            fresh_event_id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            crate::Remote::gmail(
                gmail::Endpoints::google(),
                gmail::StaticToken("token".into()),
            ),
        )
        .await
        .expect("opens store");

        // 1. Queueing a Split without a `patch.id` assigns a deterministic id in the row.
        store
            .queue_calendar_change(
                1,
                CalendarChange::Split {
                    calendar: "maya@brightmoor.co".into(),
                    series: "series1".into(),
                    from: Cut::Day(Date::constant(2026, 11, 17)),
                    patch: EventPatch::default(),
                    start: on_date(date(2026, 11, 17)),
                    end: on_date(date(2026, 11, 18)),
                    notify: false,
                    shown: Vec::new(),
                },
                None,
            )
            .await
            .expect("queues split");
        let (due_first, _) = store.due_calendar_changes(true).await.expect("reads");
        let (due_second, _) = store.due_calendar_changes(true).await.expect("reads again");
        let CalendarChange::Split { patch: p1, .. } = &due_first[0].change else {
            panic!("expected Split");
        };
        let CalendarChange::Split { patch: p2, .. } = &due_second[0].change else {
            panic!("expected Split");
        };
        assert!(p1.id.is_some());
        assert_eq!(p1.id, p2.id);
        assert!(store.unqueue_calendar_change(1).await.expect("unqueues"));

        // `ended` is idempotent on retry even if `master` already carries `UNTIL`.
        let once = ended(
            &["RRULE:FREQ=WEEKLY;COUNT=6;BYDAY=TU".into()],
            Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
        );
        let twice = ended(
            &once,
            Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
        );
        assert_eq!(once, twice);

        // 2. Simulate an in-flight `Create` where the server returns a distinct ID (`server-id-99`)
        // when `unqueue_calendar_change` runs concurrently.
        store
            .queue_calendar_change(
                2,
                CalendarChange::Create {
                    calendar: "maya@brightmoor.co".into(),
                    event: CalendarEvent {
                        id: Some("e0inflight1".into()),
                        summary: Some("Sync".into()),
                        start: Some(at("2026-11-20T09:00:00-05:00")),
                        end: Some(at("2026-11-20T10:00:00-05:00")),
                        ..CalendarEvent::default()
                    },
                },
                None,
            )
            .await
            .expect("queues create");
        let (due, _) = store.due_calendar_changes(true).await.expect("reads");
        let row_id = due[0].id;
        let sync = &store.inner.calendar_outbox;
        let flush_guard = sync.flush.lock().await;
        sync.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sending = Some(row_id);

        let store_for_undo = store.clone();
        let undo_task = tokio::spawn(async move {
            store_for_undo
                .unqueue_calendar_change(2)
                .await
                .expect("undoes")
        });
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;

        // Finish the simulated in-flight `Create` with a server-assigned ID.
        store
            .dequeue(row_id, CalendarOutcome::Sent)
            .await
            .expect("dequeues");
        {
            let mut state = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sending = None;
            if let Some(slot) = state.watched_creates.get_mut(&row_id) {
                *slot = Some("server-id-99".to_owned());
            }
        }
        drop(flush_guard);

        assert!(undo_task.await.expect("joins"));
        let (compensating, _) = store.due_calendar_changes(true).await.expect("reads");
        assert_eq!(
            compensating.len(),
            1,
            "compensating Delete was queued instead of leaving a phantom event"
        );
        assert_eq!(
            compensating[0].change,
            CalendarChange::Delete {
                calendar: "maya@brightmoor.co".into(),
                id: "server-id-99".into(),
                series_id: None,
                notify: false,
                comment: None,
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remainder_overlays_patch_metadata_and_recurrence() {
        let master = CalendarEvent {
            id: Some("series1".into()),
            summary: Some("Weekly".into()),
            start: Some(at("2026-11-10T10:00:00-05:00")),
            end: Some(at("2026-11-10T10:30:00-05:00")),
            color_id: Some("1".into()),
            show_as: ShowAs::Busy,
            visibility: Visibility::Default,
            reminders: EventReminders::Default,
            guests_can_invite_others: Some(true),
            guests_can_modify: false,
            guests_can_see_other_guests: Some(true),
            raw_recurrence: vec!["RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=6".into()],
            ..CalendarEvent::default()
        };
        let patch = EventPatch {
            id: Some("series2".into()),
            color_id: Some("5".into()),
            show_as: Some(ShowAs::Free),
            visibility: Some(Visibility::Private),
            reminders: Some(EventReminders::None),
            guests_can_invite_others: Some(false),
            guests_can_modify: Some(true),
            guests_can_see_other_guests: Some(false),
            recurrence: Some(Some(Recurrence {
                frequency: Frequency::Daily,
                interval: 1,
                end: End::After(3),
            })),
            raw_recurrence: Some(vec!["RRULE:FREQ=DAILY;COUNT=3".into()]),
            ..EventPatch::default()
        };
        let cut = Cut::At("2026-11-17T15:00:00Z".parse().expect("time"));
        let built = remainder(
            &master,
            &patch,
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert_eq!(built.color_id.as_deref(), Some("5"));
        assert_eq!(built.show_as, ShowAs::Free);
        assert_eq!(built.visibility, Visibility::Private);
        assert_eq!(built.reminders, EventReminders::None);
        assert_eq!(built.guests_can_invite_others, Some(false));
        assert!(built.guests_can_modify);
        assert_eq!(built.guests_can_see_other_guests, Some(false));
        assert_eq!(built.raw_recurrence, ["RRULE:FREQ=DAILY;COUNT=3"]);

        // Omitted patch metadata falls back to master.
        let fallback = remainder(
            &master,
            &EventPatch {
                id: Some("series3".into()),
                ..EventPatch::default()
            },
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert_eq!(fallback.color_id.as_deref(), Some("1"));
        assert_eq!(fallback.show_as, ShowAs::Busy);
        assert_eq!(fallback.visibility, Visibility::Default);
        assert_eq!(fallback.reminders, EventReminders::Default);
        assert_eq!(fallback.guests_can_invite_others, Some(true));
        assert!(!fallback.guests_can_modify);
        assert_eq!(fallback.guests_can_see_other_guests, Some(true));
        assert_eq!(
            fallback.raw_recurrence,
            ["RRULE:FREQ=WEEKLY;BYDAY=TU;WKST=SU;COUNT=5"]
        );
    }

    #[tokio::test]
    async fn fold_into_cache_persists_split_replacements_using_indexed_series_id() {
        let dir = std::env::temp_dir().join(format!(
            "exo-cal-split-fold-{}-{}",
            std::process::id(),
            fresh_event_id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            crate::Remote::gmail(
                gmail::Endpoints::google(),
                gmail::StaticToken("token".into()),
            ),
        )
        .await
        .expect("opens store");
        let cal = "maya@brightmoor.co";

        let inst1 = CalendarEvent {
            id: Some("s1_20261110T150000Z".into()),
            recurring_event_id: Some("s1".into()),
            summary: Some("Standup".into()),
            start: Some(at("2026-11-10T10:00:00-05:00")),
            end: Some(at("2026-11-10T10:30:00-05:00")),
            ..CalendarEvent::default()
        };
        let inst2 = CalendarEvent {
            id: Some("s1_20261117T150000Z".into()),
            recurring_event_id: Some("s1".into()),
            summary: Some("Standup".into()),
            start: Some(at("2026-11-17T10:00:00-05:00")),
            end: Some(at("2026-11-17T10:30:00-05:00")),
            ..CalendarEvent::default()
        };
        let inst3 = CalendarEvent {
            id: Some("s1_20261124T150000Z".into()),
            recurring_event_id: Some("s1".into()),
            summary: Some("Standup".into()),
            start: Some(at("2026-11-24T10:00:00-05:00")),
            end: Some(at("2026-11-24T10:30:00-05:00")),
            ..CalendarEvent::default()
        };
        let unrelated = CalendarEvent {
            id: Some("other1".into()),
            summary: Some("Unrelated".into()),
            start: Some(at("2026-11-17T14:00:00-05:00")),
            end: Some(at("2026-11-17T15:00:00-05:00")),
            ..CalendarEvent::default()
        };
        {
            let mut db = store.inner.db.clone();
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            for ev in [&inst1, &inst2, &inst3, &unrelated] {
                put(write.exec(), cal, ev).await.expect("puts");
            }
            write.commit().await.expect("commits");
        }

        let repl2 = CalendarEvent {
            id: Some("s2_20261117T160000Z".into()),
            recurring_event_id: Some("s2".into()),
            summary: Some("Standup (moved)".into()),
            start: Some(at("2026-11-17T11:00:00-05:00")),
            end: Some(at("2026-11-17T11:30:00-05:00")),
            ..CalendarEvent::default()
        };
        let repl3 = CalendarEvent {
            id: Some("s2_20261124T160000Z".into()),
            recurring_event_id: Some("s2".into()),
            summary: Some("Standup (moved)".into()),
            start: Some(at("2026-11-24T11:00:00-05:00")),
            end: Some(at("2026-11-24T11:30:00-05:00")),
            ..CalendarEvent::default()
        };
        let split = CalendarChange::Split {
            calendar: cal.into(),
            series: "s1".into(),
            from: Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
            patch: EventPatch {
                id: Some("s2".into()),
                summary: Some("Standup (moved)".into()),
                ..EventPatch::default()
            },
            start: at("2026-11-17T11:00:00-05:00"),
            end: at("2026-11-17T11:30:00-05:00"),
            notify: false,
            shown: vec![repl2, repl3],
        };
        store.fold_into_cache(&split).await.expect("folds");

        let mut db = store.inner.db.clone();
        let mut cached: Vec<String> = EventRow::filter(EventRow::fields().calendar_id().eq(cal))
            .exec(&mut db)
            .await
            .expect("reads")
            .into_iter()
            .map(|row| row.id)
            .collect();
        cached.sort();
        assert_eq!(
            cached,
            vec![
                "other1".to_owned(),
                "s1_20261110T150000Z".to_owned(),
                "s2_20261117T160000Z".to_owned(),
                "s2_20261124T160000Z".to_owned(),
            ],
            "old instances from cut on are removed and replacement instances are persisted"
        );

        drop(db);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
