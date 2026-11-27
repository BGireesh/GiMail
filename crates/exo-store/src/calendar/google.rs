//! Google Calendar v3 backend implementing [`CalendarProvider`] and lossless wire conversions.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use gmail::calendar::{
    CalendarListEntry, CalendarListQuery, Event, EventAttachment as WireAttachment,
    EventAttendee as WireAttendee, EventDateTime, EventPerson as WirePerson,
    EventReminder as WireReminder, EventReminders as WireReminders, EventsQuery, FreeBusyRequest,
    FreeBusyRequestItem,
};
use gmail::{Call, Client};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::de::DeserializeOwned;

use super::model::{
    AccessRole, Calendar, CalendarEvent, CalendarProviderKind, Conference, ConferenceKind,
    ConferencePatch, EventAttachment, EventAttendee, EventPatch, EventPerson, EventReminders,
    EventStatus, EventTime, EventType, FreeBusySlot, OnlineMeetingKind, RsvpStatus, ShowAs,
    Visibility,
};
use super::outbox::{Cut, InviteLinks, fresh_event_id};
use super::provider::{CalendarProvider, WindowEvents, WindowSync, report_primary_mail};
use super::recurrence::{Recurrence, retime_until_if_toggled};
use crate::api::{Calls, Failure, classify};
use crate::connection::Debounce;
use crate::error::{Error, Result};
use crate::provider::Provider;
use crate::quota::Budget;

/// The event types the calendar shows, leaving out working locations and birthdays.
const EVENT_TYPES: [&str; 4] = ["default", "focusTime", "outOfOffice", "fromGmail"];
/// Calendar's largest page.
const PAGE: u32 = 2500;
/// Sync's retries for one call.
const ATTEMPTS: u32 = 3;
/// The `maxResults` for someone else's calendar, which is one page of it.
const ATTENDEE_PAGE: u32 = 500;

/// Google Calendar backend for one account (primary or secondary).
pub(crate) struct GoogleCalendar {
    email: String,
    client: Client,
    calls: Calls,
    /// Set only for the store's primary account so network outages report on its connection bar;
    /// `None` for secondary calendar accounts so they never poison primary mail's outage state.
    primary_mail: Option<Arc<dyn Provider>>,
}

impl GoogleCalendar {
    pub fn new(
        email: String,
        client: Client,
        backoff: Duration,
        debounce: Debounce,
        dir: &Path,
        primary_mail: Option<Arc<dyn Provider>>,
    ) -> Self {
        Self {
            email,
            client,
            calls: Calls::new(
                "Calendar",
                backoff,
                Budget::new("Calendar", None),
                debounce,
                dir,
            ),
            primary_mail,
        }
    }

    async fn send<'a, T: DeserializeOwned + 'a>(
        &'a self,
        attempts: u32,
        mut make: impl FnMut(&'a Client) -> Call<'a, T>,
    ) -> std::result::Result<T, Failure> {
        let result = self
            .calls
            .retry(None, attempts, || make(&self.client), Call::units, classify)
            .await;
        report_primary_mail(
            self.primary_mail.as_ref(),
            &self.client.endpoints().oauth.token,
            &result,
        );
        result
    }

    async fn fetch_wire_event(&self, calendar_id: &str, event_id: &str) -> Result<Event> {
        Ok(self
            .send(ATTEMPTS, |client| {
                client.calendar().event(calendar_id, event_id)
            })
            .await?)
    }

    async fn insert_wire_event(
        &self,
        calendar_id: &str,
        event: &Event,
        notify: bool,
    ) -> Result<Event> {
        let updates = if notify { "all" } else { "none" };
        match self
            .send(ATTEMPTS, |client| {
                client
                    .calendar()
                    .insert_event(calendar_id, event, Some(updates))
            })
            .await
        {
            Ok(created) => Ok(created),
            Err(failure)
                if has_status(&failure, 409)
                    && let Some(id) = &event.id =>
            {
                self.fetch_wire_event(calendar_id, id).await
            }
            Err(failure) => Err(failure.into()),
        }
    }

    async fn patch_wire_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        patch: &Event,
        notify: bool,
    ) -> Result<Event> {
        let updates = if notify { "all" } else { "none" };
        Ok(self
            .send(ATTEMPTS, |client| {
                client
                    .calendar()
                    .patch_event(calendar_id, event_id, patch, Some(updates))
            })
            .await?)
    }

    async fn respond_wire(
        &self,
        calendar_id: &str,
        event_id: &str,
        status: RsvpStatus,
    ) -> Result<CalendarEvent> {
        let patch = Event {
            attendees_omitted: Some(true),
            attendees: vec![WireAttendee {
                email: Some(self.email.clone()),
                response_status: Some(status.as_google().to_owned()),
                ..WireAttendee::default()
            }],
            ..Event::default()
        };
        let answered = self
            .patch_wire_event(calendar_id, event_id, &patch, true)
            .await?;
        Ok(event_from_google(answered))
    }

    async fn list_events_inner(
        &self,
        calendar_id: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<CalendarEvent>> {
        let mut events = Vec::new();
        let mut query = EventsQuery {
            time_min: Some(from.to_string()),
            time_max: Some(to.to_string()),
            single_events: true,
            max_results: Some(PAGE),
            event_types: EVENT_TYPES.map(str::to_owned).to_vec(),
            ..EventsQuery::default()
        };
        loop {
            let page = self
                .send(ATTEMPTS, |client| {
                    client.calendar().events(calendar_id, &query)
                })
                .await?;
            events.extend(page.items.into_iter().map(event_from_google));
            match page.next_page_token {
                Some(token) => query.page_token = Some(token),
                None => break,
            }
        }
        Ok(events)
    }

    async fn free_busy_inner(
        &self,
        calendar_ids: &[String],
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<(String, Option<Vec<FreeBusySlot>>)>> {
        if calendar_ids.is_empty() {
            return Ok(Vec::new());
        }
        let request = FreeBusyRequest {
            time_min: Some(from.to_string()),
            time_max: Some(to.to_string()),
            items: calendar_ids
                .iter()
                .map(|id| FreeBusyRequestItem { id: id.clone() })
                .collect(),
            ..FreeBusyRequest::default()
        };
        let response = self
            .send(ATTEMPTS, |client| client.calendar().free_busy(&request))
            .await?;
        Ok(calendar_ids
            .iter()
            .map(|id| {
                let spans = response
                    .calendars
                    .get(id)
                    .filter(|cal| cal.errors.is_empty())
                    .map(|cal| {
                        cal.busy
                            .iter()
                            .filter_map(|period| {
                                Some(FreeBusySlot {
                                    start: period.start.parse().ok()?,
                                    end: period.end.parse().ok()?,
                                    status: ShowAs::Busy,
                                })
                            })
                            .collect()
                    });
                (id.clone(), spans)
            })
            .collect())
    }
}

impl CalendarProvider for GoogleCalendar {
    fn kind(&self) -> CalendarProviderKind {
        CalendarProviderKind::Google
    }

    fn email(&self) -> &str {
        &self.email
    }

    fn list_calendars(&self) -> BoxFuture<'_, Result<Vec<Calendar>>> {
        Box::pin(async move {
            let mut entries = Vec::new();
            let mut query = CalendarListQuery {
                max_results: Some(1000),
                show_hidden: true,
                ..CalendarListQuery::default()
            };
            loop {
                let page = self
                    .send(ATTEMPTS, |client| client.calendar().calendar_list(&query))
                    .await?;
                entries.extend(
                    page.items
                        .into_iter()
                        .filter(|entry| {
                            entry.id.is_some()
                                && entry.summary.is_some()
                                && (entry.hidden != Some(true) || entry.primary == Some(true))
                        })
                        .map(calendar_from_google),
                );
                match page.next_page_token {
                    Some(token) => query.page_token = Some(token),
                    None => break,
                }
            }
            Ok(entries)
        })
    }

    fn sync_window<'a>(
        &'a self,
        calendar_id: &'a str,
        from: Timestamp,
        to: Timestamp,
        _tz: &'a TimeZone,
    ) -> BoxFuture<'a, Result<WindowSync>> {
        Box::pin(async move {
            let events = self.list_events_inner(calendar_id, from, to).await?;
            Ok(WindowSync {
                events: WindowEvents::Replace(events),
                on_commit: None,
            })
        })
    }

    fn list_events<'a>(
        &'a self,
        calendar_id: &'a str,
        from: Timestamp,
        to: Timestamp,
        _tz: &'a TimeZone,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>>> {
        Box::pin(self.list_events_inner(calendar_id, from, to))
    }

    fn series_recurrence<'a>(
        &'a self,
        calendar_id: &'a str,
        series_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Recurrence>>> {
        Box::pin(async move {
            let event = event_from_google(self.fetch_wire_event(calendar_id, series_id).await?);
            Ok(event.recurrence)
        })
    }

    fn create_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event: &'a CalendarEvent,
        notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>> {
        Box::pin(async move {
            let wire = event_to_google(event);
            let created = self.insert_wire_event(calendar_id, &wire, notify).await?;
            Ok(event_from_google(created))
        })
    }

    fn patch_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event_id: &'a str,
        patch: &'a EventPatch,
        shift: Option<(SignedDuration, SignedDuration)>,
        notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>> {
        Box::pin(async move {
            let mut effective = patch.clone();
            let needs_anchor = matches!(effective.recurrence, Some(Some(_)))
                && effective.start.is_none()
                && effective.raw_recurrence.is_none();
            let existing_start = if shift.is_some() || needs_anchor {
                let master = event_from_google(self.fetch_wire_event(calendar_id, event_id).await?);
                if let Some((start_delta, end_delta)) = shift {
                    apply_shift(&mut effective, &master, (start_delta, end_delta));
                }
                master.start
            } else {
                None
            };
            let anchor_start = effective.start.as_ref().or(existing_start.as_ref());
            let wire = patch_to_google(&effective, anchor_start);
            let updated = self
                .patch_wire_event(calendar_id, event_id, &wire, notify)
                .await?;
            Ok(event_from_google(updated))
        })
    }

    fn delete_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event_id: &'a str,
        notify: bool,
        _comment: Option<&'a str>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let updates = if notify { "all" } else { "none" };
            match self
                .send(ATTEMPTS, |client| {
                    client
                        .calendar()
                        .delete_event(calendar_id, event_id, Some(updates))
                })
                .await
            {
                // Already deleted by an attempt whose response was lost or elsewhere.
                Err(failure) if has_status(&failure, 410) => Ok(()),
                result => result.map_err(Into::into),
            }
        })
    }

    fn respond<'a>(
        &'a self,
        calendar_id: &'a str,
        event: &'a CalendarEvent,
        status: RsvpStatus,
        all: bool,
    ) -> BoxFuture<'a, Result<Option<CalendarEvent>>> {
        Box::pin(async move {
            let series = event.recurring_event_id.as_deref().filter(|_| all);
            let id = series.or(event.id.as_deref()).unwrap_or_default();
            let answered = self.respond_wire(calendar_id, id, status).await?;
            if series.is_some() || answered.is_series_master() {
                Ok(None)
            } else {
                Ok(Some(answered))
            }
        })
    }

    fn respond_to_invite<'a>(
        &'a self,
        calendar_id: &'a str,
        invite: &'a CalendarEvent,
        status: RsvpStatus,
        links: Option<&'a InviteLinks>,
    ) -> BoxFuture<'a, Result<Option<CalendarEvent>>> {
        Box::pin(async move {
            let uid = invite.ical_uid.clone().unwrap_or_default();
            let query = EventsQuery {
                ical_uid: Some(uid),
                show_deleted: true,
                ..EventsQuery::default()
            };
            let page = self
                .send(ATTEMPTS, |client| {
                    client.calendar().events(calendar_id, &query)
                })
                .await?;
            let found = page.items.into_iter().next();
            let event = match found {
                Some(event) if event.status.as_deref() == Some("cancelled") => {
                    return Err(Error::EventCancelled);
                }
                Some(event) => event,
                // Calendar won't take an unknown sender's event, which the mail answers.
                None if links.is_some() => return Err(Error::UnknownSender),
                None => {
                    let wire_invite = event_to_google(invite);
                    self.send(ATTEMPTS, |client| {
                        client.calendar().import_event(calendar_id, &wire_invite)
                    })
                    .await?
                }
            };
            let target_id = event
                .recurring_event_id
                .as_deref()
                .or(event.id.as_deref())
                .unwrap_or_default();
            let _ = self.respond_wire(calendar_id, target_id, status).await?;
            Ok(None)
        })
    }

    fn truncate_series<'a>(
        &'a self,
        calendar_id: &'a str,
        series_id: &'a str,
        from: Cut,
        notify: bool,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let master = event_from_google(self.fetch_wire_event(calendar_id, series_id).await?);
            if master.start.as_ref().is_none_or(|first| !from.keeps(first)) {
                self.delete_event(calendar_id, series_id, notify, None)
                    .await?;
                return Ok(true);
            }
            let patch = Event {
                recurrence: ended(&master.raw_recurrence, from),
                ..Event::default()
            };
            self.patch_wire_event(calendar_id, series_id, &patch, notify)
                .await?;
            Ok(false)
        })
    }

    fn split_series<'a>(
        &'a self,
        calendar_id: &'a str,
        series_id: &'a str,
        from: Cut,
        patch: &'a EventPatch,
        start: &'a EventTime,
        end: &'a EventTime,
        notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>> {
        Box::pin(async move {
            let master = event_from_google(self.fetch_wire_event(calendar_id, series_id).await?);
            let new_start = patch.start.clone().unwrap_or_else(|| start.clone());
            let new_end = patch.end.clone().unwrap_or_else(|| end.clone());
            if master
                .start
                .as_ref()
                .is_none_or(|first| !from.keeps(first) || first == &new_start)
            {
                // From the first instance, the series itself changes.
                let mut first_patch = EventPatch {
                    id: None,
                    start: Some(new_start.clone()),
                    end: Some(new_end),
                    ..patch.clone()
                };
                retime_until_if_toggled(&master, &new_start, &mut first_patch);
                let wire = patch_to_google(&first_patch, Some(&new_start));
                let updated = self
                    .patch_wire_event(calendar_id, series_id, &wire, notify)
                    .await?;
                return Ok(event_from_google(updated));
            }
            // Insert the new series first (with its chosen `patch.id`) so a retry after a
            // partial failure gets 409 Conflict instead of copying the truncated `UNTIL` from
            // `master`.
            let mut new_series = remainder(&master, patch, new_start, new_end, from);
            if new_series.id.is_none() {
                new_series.id = Some(fresh_event_id());
            }
            let wire_new = event_to_google(&new_series);
            let created = self
                .insert_wire_event(calendar_id, &wire_new, notify)
                .await?;
            let truncated = Event {
                recurrence: ended(&master.raw_recurrence, from),
                ..Event::default()
            };
            self.patch_wire_event(calendar_id, series_id, &truncated, notify)
                .await?;
            Ok(event_from_google(created))
        })
    }

    fn free_busy<'a>(
        &'a self,
        calendar_ids: &'a [String],
        from: Timestamp,
        to: Timestamp,
    ) -> BoxFuture<'a, Result<Vec<(String, Option<Vec<FreeBusySlot>>)>>> {
        Box::pin(self.free_busy_inner(calendar_ids, from, to))
    }

    fn attendee_events<'a>(
        &'a self,
        guest_email: &'a str,
        from: Timestamp,
        to: Timestamp,
    ) -> BoxFuture<'a, Result<Option<Vec<CalendarEvent>>>> {
        Box::pin(async move {
            let query = EventsQuery {
                time_min: Some(from.to_string()),
                time_max: Some(to.to_string()),
                single_events: true,
                max_results: Some(ATTENDEE_PAGE),
                ..EventsQuery::default()
            };
            match self
                .send(ATTEMPTS, |client| {
                    client.calendar().events(guest_email, &query)
                })
                .await
            {
                Ok(page) => Ok(Some(
                    page.items.into_iter().map(event_from_google).collect(),
                )),
                Err(Failure::Permanent(error)) if matches!(error.status(), Some(403 | 404)) => {
                    let busy = self
                        .free_busy_inner(&[guest_email.to_owned()], from, to)
                        .await?
                        .into_iter()
                        .next()
                        .and_then(|(_, spans)| spans);
                    Ok(busy.map(|spans| {
                        spans
                            .into_iter()
                            .map(|slot| CalendarEvent {
                                provider: CalendarProviderKind::Google,
                                start: Some(EventTime::timed(slot.start, None)),
                                end: Some(EventTime::timed(slot.end, None)),
                                show_as: slot.status,
                                ..CalendarEvent::default()
                            })
                            .collect()
                    }))
                }
                Err(failure) => Err(failure.into()),
            }
        })
    }
}

/// Applies `shift` (an instance's civil wall-clock move) to `patch` relative to `master`,
/// preserving wall-clock time across DST and retiming `UNTIL` if `all_day` toggles.
pub(crate) fn apply_shift(
    patch: &mut EventPatch,
    master: &CalendarEvent,
    (start_delta, end_delta): (SignedDuration, SignedDuration),
) {
    let start_template = patch.start.clone();
    let end_template = patch.end.clone();
    let new_start = master.start.as_ref().and_then(|base| {
        base.shift_wall_clock(
            start_delta,
            start_template.as_ref().map(EventTime::is_all_day),
            start_template.as_ref().and_then(EventTime::time_zone),
        )
    });
    let new_end = master.end.as_ref().and_then(|base| {
        base.shift_wall_clock(
            end_delta,
            end_template.as_ref().map(EventTime::is_all_day),
            end_template.as_ref().and_then(EventTime::time_zone),
        )
    });
    if let Some(target_start) = &new_start {
        retime_until_if_toggled(master, target_start, patch);
    }
    patch.start = new_start.or(start_template);
    patch.end = new_end.or(end_template);
}

/// `recurrence` with each `RRULE` ending before `from` (`UNTIL`, replacing any count or earlier
/// end).
pub(crate) fn ended(recurrence: &[String], from: Cut) -> Vec<String> {
    let until = from.until();
    recurrence
        .iter()
        .map(|line| match line.strip_prefix("RRULE:") {
            Some(rule) => {
                let mut parts: Vec<String> = rule
                    .split(';')
                    .filter(|part| !part.starts_with("COUNT=") && !part.starts_with("UNTIL="))
                    .map(str::to_owned)
                    .collect();
                parts.push(format!("UNTIL={until}"));
                format!("RRULE:{}", parts.join(";"))
            }
            None => line.clone(),
        })
        .collect()
}

/// The new series that takes over `master` from `from`, with `patch` applied via
/// [`CalendarEvent::apply_patch`] and its recurrence rule counting only remaining occurrences.
pub(crate) fn remainder(
    master: &CalendarEvent,
    patch: &EventPatch,
    start: EventTime,
    end: EventTime,
    from: Cut,
) -> CalendarEvent {
    let mut event = CalendarEvent {
        id: patch.id.clone(),
        provider: master.provider,
        summary: master.summary.clone(),
        description: master.description.clone(),
        location: master.location.clone(),
        attendees: master.attendees.clone(),
        conference: master.conference.clone(),
        color_id: master.color_id.clone(),
        categories: master.categories.clone(),
        show_as: master.show_as,
        visibility: master.visibility,
        reminders: master.reminders.clone(),
        guests_can_invite_others: master.guests_can_invite_others,
        guests_can_modify: master.guests_can_modify,
        guests_can_see_other_guests: master.guests_can_see_other_guests,
        recurrence: master.recurrence.clone(),
        raw_recurrence: master.raw_recurrence.clone(),
        ..CalendarEvent::default()
    };
    event.apply_patch(patch);
    let all_day = start.is_all_day();
    if patch.recurrence.is_none() && patch.raw_recurrence.is_none() {
        let first = master
            .start
            .as_ref()
            .and_then(|time| time.own_zoned(&TimeZone::UTC));
        let new_start = start.own_zoned(&TimeZone::UTC);
        if let (Some(first), Some(new_start)) = (first, new_start)
            && let Some(rule) = master
                .recurrence
                .clone()
                .or_else(|| Recurrence::from_rules_for(&master.raw_recurrence, &first))
        {
            let day = match from {
                Cut::Day(day) => day,
                Cut::At(at) => at.to_zoned(first.time_zone().clone()).date(),
            };
            let next_rule = rule.from_day(&first, day);
            let line = next_rule.rule(&new_start, all_day);
            event.recurrence = Some(next_rule);
            event.raw_recurrence = master
                .raw_recurrence
                .iter()
                .filter(|line| !line.starts_with("RRULE"))
                .cloned()
                .chain([line])
                .collect();
        }
    }
    event.start = Some(start);
    event.end = Some(end);
    event
}

fn has_status(failure: &Failure, status: u16) -> bool {
    matches!(failure, Failure::Permanent(error) if error.status() == Some(status))
}

fn meet_create_request() -> serde_json::Value {
    serde_json::json!({
        "createRequest": {
            "requestId": fresh_event_id(),
            "conferenceSolutionKey": { "type": "hangoutsMeet" }
        }
    })
}

/// Converts a Google [`CalendarListEntry`] into the provider-neutral [`Calendar`].
pub(crate) fn calendar_from_google(entry: CalendarListEntry) -> Calendar {
    let (access_role, can_view_private_items) = match entry.access_role.as_deref() {
        Some("owner") => (AccessRole::Owner, true),
        Some("writer") => (AccessRole::Writer, true),
        Some("writerWithoutPrivateAccess") => (AccessRole::Writer, false),
        Some("freeBusyReader") => (AccessRole::FreeBusyReader, false),
        _ => (AccessRole::Reader, false),
    };
    let mut allowed_online_meetings = Vec::new();
    if let Some(types) = entry
        .conference_properties
        .as_ref()
        .and_then(|props| props.get("allowedConferenceSolutionTypes"))
        .and_then(|value| value.as_array())
    {
        if types.iter().any(|v| v.as_str() == Some("hangoutsMeet")) {
            allowed_online_meetings.push(OnlineMeetingKind::GoogleMeet);
        }
    } else if access_role.is_writable() {
        allowed_online_meetings.push(OnlineMeetingKind::GoogleMeet);
    }
    let default_online_meeting = allowed_online_meetings.first().copied();
    Calendar {
        id: entry.id.unwrap_or_default(),
        summary: entry.summary.unwrap_or_default(),
        summary_override: entry.summary_override,
        primary: entry.primary == Some(true),
        access_role,
        can_view_private_items,
        background_color: entry.background_color.filter(|hex| !hex.is_empty()),
        color_name: None,
        time_zone: entry.time_zone,
        hidden: entry.hidden == Some(true),
        selected: entry.selected == Some(true),
        deleted: entry.deleted == Some(true),
        provider: CalendarProviderKind::Google,
        allowed_online_meetings,
        default_online_meeting,
    }
}

/// Converts a Google [`EventDateTime`] into [`EventTime`].
pub(crate) fn time_from_google(dt: &EventDateTime) -> Option<EventTime> {
    if let Some(date) = &dt.date {
        return Some(EventTime::AllDay(date.parse().ok()?));
    }
    let date_time = dt.date_time.as_deref()?;
    let at: Timestamp = date_time.parse().ok()?;
    Some(EventTime::Timed {
        at,
        time_zone: dt.time_zone.clone(),
    })
}

/// Converts [`EventTime`] into Google's [`EventDateTime`].
pub(crate) fn time_to_google(time: &EventTime) -> EventDateTime {
    match time {
        EventTime::AllDay(date) => EventDateTime {
            date: Some(date.to_string()),
            ..EventDateTime::default()
        },
        EventTime::Timed { at, time_zone } => {
            let tz = time.resolve_zone(&TimeZone::UTC);
            let zoned = at.to_zoned(tz);
            EventDateTime {
                date: None,
                date_time: Some(zoned.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()),
                time_zone: time_zone.clone(),
            }
        }
    }
}

/// Converts a Google [`WireAttendee`] into [`EventAttendee`].
pub(crate) fn attendee_from_google(attendee: WireAttendee) -> EventAttendee {
    EventAttendee {
        id: attendee.id,
        email: attendee.email,
        display_name: attendee.display_name,
        organizer: attendee.organizer == Some(true),
        is_self: attendee.is_self == Some(true),
        resource: attendee.resource == Some(true),
        optional: attendee.optional == Some(true),
        response_status: attendee
            .response_status
            .as_deref()
            .and_then(RsvpStatus::from_google)
            .unwrap_or(RsvpStatus::NeedsAction),
        comment: attendee.comment,
        additional_guests: attendee.additional_guests,
    }
}

fn attendee_to_google(attendee: &EventAttendee) -> WireAttendee {
    WireAttendee {
        id: attendee.id.clone(),
        email: attendee.email.clone(),
        display_name: attendee.display_name.clone(),
        organizer: attendee.organizer.then_some(true),
        is_self: attendee.is_self.then_some(true),
        resource: attendee.resource.then_some(true),
        optional: attendee.optional.then_some(true),
        response_status: Some(attendee.response_status.as_google().to_owned()),
        comment: attendee.comment.clone(),
        additional_guests: attendee.additional_guests,
    }
}

fn person_from_google(person: WirePerson) -> EventPerson {
    EventPerson {
        email: person.email,
        display_name: person.display_name,
        is_self: person.is_self == Some(true),
    }
}

fn person_to_google(person: &EventPerson) -> WirePerson {
    WirePerson {
        id: None,
        email: person.email.clone(),
        display_name: person.display_name.clone(),
        is_self: person.is_self.then_some(true),
    }
}

fn conference_from_google(
    data: Option<&serde_json::Value>,
    hangout_link: Option<&str>,
) -> Option<Conference> {
    if let Some(data) = data {
        if let Some(entries) = data.get("entryPoints").and_then(|v| v.as_array())
            && let Some(entry) = entries
                .iter()
                .find(|entry| entry.get("entryPointType").and_then(|v| v.as_str()) == Some("video"))
                .or_else(|| entries.first())
            && let Some(raw) = entry.get("uri").and_then(|v| v.as_str())
        {
            let label = entry
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .or_else(|| Some(raw.trim_start_matches("https://").to_owned()));
            let dial_in = entries
                .iter()
                .find(|entry| entry.get("entryPointType").and_then(|v| v.as_str()) == Some("phone"))
                .and_then(|entry| {
                    entry
                        .get("label")
                        .and_then(|v| v.as_str())
                        .or_else(|| entry.get("uri").and_then(|v| v.as_str()))
                })
                .map(str::to_owned);
            let solution = data.get("conferenceSolution");
            let solution_type = solution
                .and_then(|s| s.get("key"))
                .and_then(|k| k.get("type"))
                .and_then(|t| t.as_str());
            let solution_name = solution
                .and_then(|s| s.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or_default();
            let kind = if solution_type == Some("hangoutsMeet")
                || matches!(solution_name, "Google Meet" | "Hangouts Meet")
                || raw.contains("meet.google.com")
            {
                ConferenceKind::GoogleMeet
            } else if solution_name == "Microsoft Teams" || raw.contains("teams.microsoft.com") {
                ConferenceKind::Teams
            } else if solution_name == "Zoom Meeting" || raw.contains("zoom.us") {
                ConferenceKind::Zoom
            } else {
                ConferenceKind::Other(solution_name.to_owned())
            };
            return Some(Conference {
                kind,
                join_url: Some(raw.to_owned()),
                label,
                pending: false,
                dial_in,
            });
        }
        if data.get("createRequest").is_some() {
            return Some(Conference::pending(OnlineMeetingKind::GoogleMeet));
        }
    }
    hangout_link.map(|link| Conference {
        kind: ConferenceKind::GoogleMeet,
        join_url: Some(link.to_owned()),
        label: Some(link.trim_start_matches("https://").to_owned()),
        pending: false,
        dial_in: None,
    })
}

fn reminders_from_google(reminders: Option<WireReminders>) -> EventReminders {
    let Some(reminders) = reminders else {
        return EventReminders::Default;
    };
    if reminders.use_default == Some(true) {
        return EventReminders::Default;
    }
    let minutes: Vec<u32> = reminders
        .overrides
        .into_iter()
        .filter_map(|item| item.minutes)
        .collect();
    if minutes.is_empty() {
        EventReminders::None
    } else {
        EventReminders::Custom(minutes)
    }
}

fn reminders_to_google(reminders: &EventReminders) -> Option<WireReminders> {
    match reminders {
        EventReminders::Default => None,
        EventReminders::None => Some(WireReminders {
            use_default: Some(false),
            overrides: Vec::new(),
        }),
        EventReminders::Custom(minutes) => Some(WireReminders {
            use_default: Some(false),
            overrides: minutes
                .iter()
                .map(|&m| WireReminder {
                    method: Some("popup".to_owned()),
                    minutes: Some(m),
                })
                .collect(),
        }),
    }
}

/// Converts a Google [`Event`] into the provider-neutral [`CalendarEvent`].
pub(crate) fn event_from_google(event: Event) -> CalendarEvent {
    let start = event.start.as_ref().and_then(time_from_google);
    let end = event.end.as_ref().and_then(time_from_google);
    let original_start = event
        .original_start_time
        .as_ref()
        .and_then(time_from_google);
    let status = match event.status.as_deref() {
        Some("tentative") => EventStatus::Tentative,
        Some("cancelled") => EventStatus::Cancelled,
        _ => EventStatus::Confirmed,
    };
    let event_type = match event.event_type.as_deref() {
        Some("outOfOffice") => EventType::OutOfOffice,
        Some("focusTime") => EventType::FocusTime,
        Some("workingLocation") => EventType::WorkingLocation,
        Some("birthday") => EventType::Birthday,
        Some("fromGmail") => EventType::FromGmail,
        _ => EventType::Default,
    };
    let show_as = if event_type == EventType::OutOfOffice {
        ShowAs::OutOfOffice
    } else if event_type == EventType::WorkingLocation {
        ShowAs::WorkingElsewhere
    } else if event.transparency.as_deref() == Some("transparent") {
        ShowAs::Free
    } else {
        ShowAs::Busy
    };
    let visibility = match event.visibility.as_deref() {
        Some("public") => Visibility::Public,
        Some("private") => Visibility::Private,
        Some("confidential") => Visibility::Confidential,
        _ => Visibility::Default,
    };
    let recurrence = if event.recurrence.is_empty() {
        None
    } else {
        let tz = start
            .as_ref()
            .and_then(|s| s.time_zone())
            .and_then(gmail::ics::resolve_timezone)
            .or_else(|| {
                event
                    .start
                    .as_ref()
                    .and_then(|s| s.date_time.as_deref())
                    .and_then(super::recurrence::rfc3339_zone)
            })
            .unwrap_or(TimeZone::UTC);
        match start.as_ref().and_then(|s| s.zoned(&tz)) {
            Some(z) => Recurrence::from_rules_for(&event.recurrence, &z),
            None => Recurrence::from_rules(&event.recurrence, &tz),
        }
    };
    let conference = conference_from_google(
        event.conference_data.as_ref(),
        event.hangout_link.as_deref(),
    );
    let attachments = event
        .attachments
        .into_iter()
        .map(|att| EventAttachment {
            file_url: att.file_url,
            title: att.title,
            mime_type: att.mime_type,
            icon_link: att.icon_link,
            file_id: att.file_id,
        })
        .collect();
    CalendarEvent {
        id: event.id,
        provider: CalendarProviderKind::Google,
        status,
        summary: event.summary,
        description: event.description,
        location: event.location,
        start,
        end,
        creator: event.creator.map(person_from_google),
        organizer: event.organizer.map(person_from_google),
        attendees: event
            .attendees
            .into_iter()
            .map(attendee_from_google)
            .collect(),
        attendees_omitted: event.attendees_omitted == Some(true),
        self_response: None,
        guests_can_modify: event.guests_can_modify == Some(true),
        guests_can_invite_others: event.guests_can_invite_others,
        guests_can_see_other_guests: event.guests_can_see_other_guests,
        recurring_event_id: event.recurring_event_id,
        original_start,
        recurrence,
        raw_recurrence: event.recurrence,
        ical_uid: event.ical_uid,
        html_link: event.html_link,
        conference,
        color_id: event.color_id,
        categories: Vec::new(),
        visibility,
        show_as,
        event_type,
        attachments,
        reminders: reminders_from_google(event.reminders),
        transaction_id: None,
    }
}

/// Converts a provider-neutral [`CalendarEvent`] into Google's wire [`Event`] for insert/import.
pub(crate) fn event_to_google(event: &CalendarEvent) -> Event {
    let recurrence = if !event.raw_recurrence.is_empty() {
        event.raw_recurrence.clone()
    } else if let (Some(rule), Some(start)) = (&event.recurrence, &event.start) {
        start
            .own_zoned(&TimeZone::UTC)
            .map(|zoned| vec![rule.rule(&zoned, start.is_all_day())])
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let conference_data = event.conference.as_ref().and_then(|conf| {
        (conf.pending && conf.kind == ConferenceKind::GoogleMeet).then(meet_create_request)
    });
    let hangout_link = event.conference.as_ref().and_then(|conf| {
        (!conf.pending && conf.kind == ConferenceKind::GoogleMeet)
            .then(|| conf.join_url.clone())
            .flatten()
    });
    let transparency = match event.show_as {
        ShowAs::Free => Some("transparent".to_owned()),
        ShowAs::Busy | ShowAs::Tentative | ShowAs::OutOfOffice | ShowAs::WorkingElsewhere => None,
    };
    let visibility = match event.visibility {
        Visibility::Default => None,
        Visibility::Public => Some("public".to_owned()),
        Visibility::Private => Some("private".to_owned()),
        Visibility::Confidential => Some("confidential".to_owned()),
    };
    let event_type = match event.event_type {
        EventType::Default => None,
        EventType::OutOfOffice => Some("outOfOffice".to_owned()),
        EventType::FocusTime => Some("focusTime".to_owned()),
        EventType::WorkingLocation => Some("workingLocation".to_owned()),
        EventType::Birthday => Some("birthday".to_owned()),
        EventType::FromGmail => Some("fromGmail".to_owned()),
    };
    Event {
        id: event.id.clone(),
        summary: event.summary.clone(),
        description: event.description.clone(),
        location: event.location.clone(),
        start: event.start.as_ref().map(time_to_google),
        end: event.end.as_ref().map(time_to_google),
        creator: event.creator.as_ref().map(person_to_google),
        organizer: event.organizer.as_ref().map(person_to_google),
        attendees: event.attendees.iter().map(attendee_to_google).collect(),
        attendees_omitted: event.attendees_omitted.then_some(true),
        guests_can_modify: event.guests_can_modify.then_some(true),
        guests_can_invite_others: event.guests_can_invite_others,
        guests_can_see_other_guests: event.guests_can_see_other_guests,
        recurring_event_id: event.recurring_event_id.clone(),
        original_start_time: event.original_start.as_ref().map(time_to_google),
        recurrence,
        ical_uid: event.ical_uid.clone(),
        html_link: event.html_link.clone(),
        conference_data,
        hangout_link,
        color_id: event.color_id.clone(),
        transparency,
        visibility,
        event_type,
        reminders: reminders_to_google(&event.reminders),
        attachments: event
            .attachments
            .iter()
            .map(|att| WireAttachment {
                file_url: att.file_url.clone(),
                title: att.title.clone(),
                mime_type: att.mime_type.clone(),
                icon_link: att.icon_link.clone(),
                file_id: att.file_id.clone(),
            })
            .collect(),
        ..Event::default()
    }
}

/// Converts a legacy Google patch [`Event`] (plus `clear_attendees`) into [`EventPatch`].
pub(crate) fn patch_from_google(patch: Event, clear_attendees: bool) -> EventPatch {
    let attendees = if !patch.attendees.is_empty() {
        Some(
            patch
                .attendees
                .into_iter()
                .map(attendee_from_google)
                .collect(),
        )
    } else if clear_attendees {
        Some(Vec::new())
    } else {
        None
    };
    let conference = match &patch.conference_data {
        Some(serde_json::Value::Null) => Some(ConferencePatch::Remove),
        Some(val) if val.get("createRequest").is_some() => {
            Some(ConferencePatch::Create(OnlineMeetingKind::GoogleMeet))
        }
        _ => None,
    };
    let start = patch.start.as_ref().and_then(time_from_google);
    let end = patch.end.as_ref().and_then(time_from_google);
    let raw_recurrence = (!patch.recurrence.is_empty()).then_some(patch.recurrence);
    let recurrence = raw_recurrence.as_ref().and_then(|lines| {
        let tz = start
            .as_ref()
            .and_then(EventTime::time_zone)
            .and_then(gmail::ics::resolve_timezone)
            .unwrap_or(TimeZone::UTC);
        Recurrence::from_rules(lines, &tz).map(Some)
    });
    let show_as = match patch.transparency.as_deref() {
        Some("transparent") => Some(ShowAs::Free),
        Some("opaque") => Some(ShowAs::Busy),
        _ => None,
    };
    let visibility = match patch.visibility.as_deref() {
        Some("default") => Some(Visibility::Default),
        Some("public") => Some(Visibility::Public),
        Some("private") => Some(Visibility::Private),
        Some("confidential") => Some(Visibility::Confidential),
        _ => None,
    };
    let reminders = patch.reminders.map(|r| reminders_from_google(Some(r)));
    EventPatch {
        id: patch.id,
        summary: patch.summary,
        start,
        end,
        location: patch.location,
        description: patch.description,
        attendees,
        conference,
        recurrence,
        raw_recurrence,
        color_id: patch.color_id,
        categories: None,
        show_as,
        visibility,
        reminders,
        guests_can_invite_others: patch.guests_can_invite_others,
        guests_can_modify: patch.guests_can_modify,
        guests_can_see_other_guests: patch.guests_can_see_other_guests,
    }
}

/// Converts [`EventPatch`] into Google's wire patch [`Event`].
fn patch_to_google(patch: &EventPatch, start_for_rrule: Option<&EventTime>) -> Event {
    let (attendees, clear_attendees) = match &patch.attendees {
        Some(list) if list.is_empty() => (Vec::new(), true),
        Some(list) => (list.iter().map(attendee_to_google).collect(), false),
        None => (Vec::new(), false),
    };
    let conference_data = match &patch.conference {
        Some(ConferencePatch::Create(OnlineMeetingKind::GoogleMeet)) => Some(meet_create_request()),
        Some(ConferencePatch::Remove) => Some(serde_json::Value::Null),
        _ => None,
    };
    let recurrence = if let Some(raw) = &patch.raw_recurrence {
        raw.clone()
    } else if let Some(Some(rule)) = &patch.recurrence
        && let Some(start) = start_for_rrule.or(patch.start.as_ref())
        && let Some(zoned) = start.own_zoned(&TimeZone::UTC)
    {
        vec![rule.rule(&zoned, start.is_all_day())]
    } else {
        Vec::new()
    };
    let transparency = patch.show_as.map(|show_as| {
        if show_as == ShowAs::Free {
            "transparent".to_owned()
        } else {
            "opaque".to_owned()
        }
    });
    let visibility = patch.visibility.map(|vis| {
        match vis {
            Visibility::Default => "default",
            Visibility::Public => "public",
            Visibility::Private => "private",
            Visibility::Confidential => "confidential",
        }
        .to_owned()
    });
    let reminders = patch.reminders.as_ref().map(|rem| match rem {
        EventReminders::Default => WireReminders {
            use_default: Some(true),
            overrides: Vec::new(),
        },
        EventReminders::None => WireReminders {
            use_default: Some(false),
            overrides: Vec::new(),
        },
        EventReminders::Custom(minutes) => WireReminders {
            use_default: Some(false),
            overrides: minutes
                .iter()
                .map(|&m| WireReminder {
                    method: Some("popup".to_owned()),
                    minutes: Some(m),
                })
                .collect(),
        },
    });
    Event {
        summary: patch.summary.clone(),
        start: patch.start.as_ref().map(time_to_google),
        end: patch.end.as_ref().map(time_to_google),
        location: patch.location.clone(),
        description: patch.description.clone(),
        attendees,
        clear_attendees,
        conference_data,
        recurrence,
        color_id: patch.color_id.clone(),
        transparency,
        visibility,
        reminders,
        guests_can_invite_others: patch.guests_can_invite_others,
        guests_can_modify: patch.guests_can_modify,
        guests_can_see_other_guests: patch.guests_can_see_other_guests,
        ..Event::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_from_google_does_not_bypass_monthly_byday_weekday_guard() {
        // 2026-11-17 is a Tuesday, while BYDAY=3FR specifies Friday.
        let wire = Event {
            id: Some("ev_mismatch".into()),
            summary: Some("Mismatched monthly".into()),
            start: Some(EventDateTime {
                date_time: Some("2026-11-17T09:00:00-05:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date_time: Some("2026-11-17T10:00:00-05:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            recurrence: vec!["RRULE:FREQ=MONTHLY;BYDAY=3FR".into()],
            ..Event::default()
        };
        let converted = event_from_google(wire);
        assert_eq!(
            converted.recurrence, None,
            "mismatched monthly BYDAY stays None rather than falling back to from_rules"
        );
        assert_eq!(converted.raw_recurrence, ["RRULE:FREQ=MONTHLY;BYDAY=3FR"]);
    }
}
