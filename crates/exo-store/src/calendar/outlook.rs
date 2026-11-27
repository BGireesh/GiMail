//! Microsoft Graph v1.0 Calendar backend implementing [`CalendarProvider`].

use std::collections::{HashMap, HashSet};
use std::future::IntoFuture;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use outlook::calendar::{
    AcceptRequest, Attendee, AttendeeType, Calendar as GraphCalendar, CalendarColor,
    Event as GraphEvent, EventPatch as GraphEventPatch, EventType as GraphEventType,
    FreeBusyStatus, OnlineMeetingProviderType, PatternedRecurrence, RecurrenceRangeType,
    RespondRequest, ResponseType, ScheduleRequest, Sensitivity,
};
use outlook::settings::CategoryColor;
use outlook::{BodyType, Client, DateTimeTimeZone, ItemBody, Location};
use tokio::sync::RwLock;
use url::Url;

use super::model::{
    AccessRole, Calendar, CalendarEvent, CalendarProviderKind, Conference, ConferenceKind,
    ConferencePatch, EventAttachment, EventAttendee, EventCategory, EventPatch, EventPerson,
    EventReminders, EventStatus, EventTime, EventType, FreeBusySlot, OnlineMeetingKind, RsvpStatus,
    ShowAs, Visibility,
};
use super::outbox::{Cut, InviteLinks, fresh_event_id};
use super::provider::{CalendarProvider, WindowEvents, WindowSync, report_primary_mail};
use super::recurrence::Recurrence;
use crate::api::{Calls, Failure};
use crate::connection::Debounce;
use crate::error::{Error, ProviderError, Result};
use crate::provider::Provider;
use crate::provider::outlook::graph;
use crate::quota::Budget;

/// Sync retries for one Graph calendar call.
const ATTEMPTS: u32 = 3;
/// Page size for `calendarView/delta`.
const DELTA_PAGE_SIZE: u32 = 250;
/// Maximum cached delta links per account (bounded to keep memory small).
const MAX_DELTAS: usize = 32;
/// Maximum cached series masters per account.
const MAX_MASTERS: usize = 512;

type DeltaMap = HashMap<(String, Timestamp, Timestamp), Url>;
type CategoryMap = HashMap<String, Option<u8>>;

/// Microsoft Graph Calendar backend for one account (primary or secondary).
pub(crate) struct OutlookCalendar {
    account_key: String,
    email: String,
    client: Client,
    calls: Calls,
    /// Set only for the store's primary account so network outages report on its connection bar;
    /// `None` for secondary calendar accounts so they never poison primary mail's outage state.
    primary_mail: Option<Arc<dyn Provider>>,
    /// Best-effort cache of mailbox master categories (lowercased display name -> preset `0..=24`).
    categories: RwLock<Option<Arc<CategoryMap>>>,
    /// Active `calendarView/delta` links keyed by `(calendar_id, from, to)`.
    deltas: Arc<Mutex<DeltaMap>>,
    /// Series masters by id, which fill out the bare occurrences calendar view deltas send.
    masters: Mutex<HashMap<String, GraphEvent>>,
}

impl OutlookCalendar {
    pub fn new(
        account_key: String,
        email: String,
        client: Client,
        backoff: Duration,
        debounce: Debounce,
        dir: &Path,
        primary_mail: Option<Arc<dyn Provider>>,
    ) -> Self {
        Self {
            account_key,
            email,
            client,
            calls: Calls::new(
                "Outlook Calendar",
                backoff,
                Budget::new("Outlook Calendar", None),
                debounce,
                dir,
            ),
            primary_mail,
            categories: RwLock::new(None),
            deltas: Arc::new(Mutex::new(HashMap::new())),
            masters: Mutex::new(HashMap::new()),
        }
    }

    async fn send<'a, C, T>(
        &'a self,
        attempts: u32,
        mut make: impl FnMut(&'a Client) -> C,
    ) -> std::result::Result<T, Failure>
    where
        C: IntoFuture<Output = std::result::Result<T, outlook::Error>>,
    {
        let result = self
            .calls
            .retry(
                None,
                attempts,
                || make(&self.client),
                |_| 1,
                graph::classify,
            )
            .await;
        report_primary_mail(
            self.primary_mail.as_ref(),
            &self.client.endpoints().identity.token,
            &result,
        );
        result
    }

    /// Best-effort refresh of the mailbox's master categories. Never fails calendar operations.
    /// Caches an empty map on permanent 403/404 (e.g. secondary calendar accounts granted
    /// `Calendars.ReadWrite` without `MailboxSettings.Read`) so later operations do not repeat
    /// failing requests.
    async fn refresh_categories(&self) {
        match self
            .send(1, |client| client.settings().categories().all())
            .await
        {
            Ok(page) => {
                let map = page
                    .value
                    .into_iter()
                    .filter_map(|cat| {
                        let name = cat.display_name?.to_lowercase();
                        let preset = match cat.color {
                            Some(CategoryColor::Preset(n)) => Some(n),
                            _ => None,
                        };
                        Some((name, preset))
                    })
                    .collect();
                *self.categories.write().await = Some(Arc::new(map));
            }
            Err(error) => {
                log::debug!(
                    "skipping Outlook master categories for {}: {}",
                    self.account_key,
                    error.message()
                );
                if is_access_denied(&error) || is_not_found_or_gone(&error) {
                    *self.categories.write().await = Some(Arc::new(HashMap::new()));
                }
            }
        }
    }

    /// Returns cached master categories, attempting one best-effort load if not fetched yet.
    async fn category_map(&self) -> Arc<HashMap<String, Option<u8>>> {
        if let Some(map) = self.categories.read().await.as_ref() {
            return Arc::clone(map);
        }
        self.refresh_categories().await;
        self.categories
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::new(HashMap::new()))
    }

    /// Scopes a Graph call to `calendar_id`, using `/me/...` default calendar for `""` or
    /// `"primary"`.
    fn cal_scope(calendar_id: &str) -> Option<&str> {
        (!calendar_id.is_empty() && calendar_id != "primary").then_some(calendar_id)
    }

    async fn fetch_raw_event(&self, calendar_id: &str, event_id: &str) -> Result<GraphEvent> {
        let scope = Self::cal_scope(calendar_id);
        Ok(self
            .send(ATTEMPTS, |client| {
                client.calendar().event(scope, event_id).timezone("UTC")
            })
            .await?)
    }

    async fn list_view(
        &self,
        calendar_id: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<CalendarEvent>> {
        let scope = Self::cal_scope(calendar_id);
        let cats = self.category_map().await;
        let page = self
            .send(ATTEMPTS, |client| {
                client
                    .calendar()
                    .calendar_view(scope, from, to)
                    .timezone("UTC")
                    .max_page_size(DELTA_PAGE_SIZE)
                    .all()
            })
            .await?;
        Ok(page
            .value
            .into_iter()
            .filter(|ev| {
                ev.removed.is_none()
                    && ev.is_cancelled != Some(true)
                    && ev.kind != Some(GraphEventType::SeriesMaster)
            })
            .map(|ev| event_from_graph(ev, &self.email, &cats))
            .collect())
    }

    async fn sync_delta_full(
        &self,
        calendar_id: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<WindowSync> {
        let scope = Self::cal_scope(calendar_id);
        let cats = self.category_map().await;
        let page = self
            .send(ATTEMPTS, |client| {
                client
                    .calendar()
                    .calendar_view_delta(scope, from, to)
                    .timezone("UTC")
                    .max_page_size(DELTA_PAGE_SIZE)
                    .all()
            })
            .await?;
        let on_commit = self.delta_commit(calendar_id, from, to, page.delta_link);
        let latest = self.delta_events(calendar_id, page.value, &cats).await?;
        Ok(WindowSync {
            events: WindowEvents::Replace(latest.into_values().flatten().collect()),
            on_commit,
        })
    }

    /// A calendar view delta's events by id, `None` for those gone. Graph sends a series'
    /// occurrences there with only their ids, type and times, so each takes the rest (title,
    /// guests, place…) from its series master: the one in the same delta, else the one last
    /// seen, else fetched.
    async fn delta_events(
        &self,
        calendar_id: &str,
        events: Vec<GraphEvent>,
        cats: &CategoryMap,
    ) -> Result<HashMap<String, Option<CalendarEvent>>> {
        let mut raw: HashMap<String, Option<GraphEvent>> = HashMap::new();
        for ev in events {
            let Some(id) = ev.id.clone() else {
                continue;
            };
            if ev.removed.is_some() || ev.is_cancelled == Some(true) {
                self.drop_master(&id);
                raw.insert(id, None);
            } else if ev.kind == Some(GraphEventType::SeriesMaster) {
                self.keep_master(id, ev);
            } else {
                raw.insert(id, Some(ev));
            }
        }
        let mut latest = HashMap::with_capacity(raw.len());
        let mut missing = HashSet::new();
        for (id, item) in raw {
            let Some(ev) = item else {
                latest.insert(id, None);
                continue;
            };
            let ev = match ev.series_master_id.as_deref() {
                Some(master_id) if ev.subject.is_none() && !missing.contains(master_id) => {
                    match self.master(calendar_id, master_id).await? {
                        Some(master) => inherit(ev, &master),
                        None => {
                            missing.insert(master_id.to_owned());
                            ev
                        }
                    }
                }
                _ => ev,
            };
            latest.insert(id, Some(event_from_graph(ev, &self.email, cats)));
        }
        Ok(latest)
    }

    fn keep_master(&self, id: String, master: GraphEvent) {
        let mut masters = self.masters.lock().unwrap_or_else(PoisonError::into_inner);
        if masters.len() >= MAX_MASTERS && !masters.contains_key(&id) {
            masters.clear();
        }
        masters.insert(id, master);
    }

    fn drop_master(&self, id: &str) {
        self.masters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    /// Series master `id`, from the cache or Graph; `None` when Graph no longer has it.
    async fn master(&self, calendar_id: &str, id: &str) -> Result<Option<GraphEvent>> {
        let cached = self
            .masters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned();
        if let Some(master) = cached {
            return Ok(Some(master));
        }
        let scope = Self::cal_scope(calendar_id);
        match self
            .send(ATTEMPTS, |client| {
                client.calendar().event(scope, id).timezone("UTC")
            })
            .await
        {
            Ok(master) => {
                self.keep_master(id.to_owned(), master.clone());
                Ok(Some(master))
            }
            Err(failure) if failure.is_not_found() => Ok(None),
            Err(failure) => Err(failure.into()),
        }
    }

    fn delta_commit(
        &self,
        calendar_id: &str,
        from: Timestamp,
        to: Timestamp,
        link: Option<Url>,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        let link = link?;
        let deltas = Arc::clone(&self.deltas);
        let key = (calendar_id.to_owned(), from, to);
        Some(Box::new(move || {
            let mut map = deltas.lock().unwrap_or_else(PoisonError::into_inner);
            if map.len() >= MAX_DELTAS && !map.contains_key(&key) {
                map.clear();
            }
            map.insert(key, link);
        }))
    }

    fn get_delta(&self, calendar_id: &str, from: Timestamp, to: Timestamp) -> Option<Url> {
        self.deltas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(calendar_id.to_owned(), from, to))
            .cloned()
    }

    fn drop_delta(&self, calendar_id: &str, from: Timestamp, to: Timestamp) {
        self.deltas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(calendar_id.to_owned(), from, to));
    }

    async fn send_rsvp(&self, calendar_id: &str, event_id: &str, status: RsvpStatus) -> Result<()> {
        let scope = Self::cal_scope(calendar_id);
        match status {
            RsvpStatus::Accepted => {
                let req = AcceptRequest {
                    comment: None,
                    send_response: Some(true),
                };
                self.send(ATTEMPTS, |client| {
                    client.calendar().accept(scope, event_id, &req)
                })
                .await?;
            }
            RsvpStatus::Tentative => {
                let req = RespondRequest {
                    comment: None,
                    send_response: Some(true),
                    proposed_new_time: None,
                };
                self.send(ATTEMPTS, |client| {
                    client.calendar().tentatively_accept(scope, event_id, &req)
                })
                .await?;
            }
            RsvpStatus::Declined => {
                let req = RespondRequest {
                    comment: None,
                    send_response: Some(true),
                    proposed_new_time: None,
                };
                self.send(ATTEMPTS, |client| {
                    client.calendar().decline(scope, event_id, &req)
                })
                .await?;
            }
            RsvpStatus::NeedsAction => {}
        }
        Ok(())
    }

    async fn calendar_view_busy(
        &self,
        calendar_id: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<FreeBusySlot>> {
        let events = self.list_view(calendar_id, from, to).await?;
        let mut slots = Vec::new();
        for ev in events {
            if ev.status == EventStatus::Cancelled || ev.show_as == ShowAs::Free {
                continue;
            }
            let (Some(start), Some(end)) = (
                ev.start.as_ref().and_then(|t| t.timestamp(&TimeZone::UTC)),
                ev.end.as_ref().and_then(|t| t.timestamp(&TimeZone::UTC)),
            ) else {
                continue;
            };
            if end > start {
                slots.push(FreeBusySlot {
                    start,
                    end,
                    status: ev.show_as,
                });
            }
        }
        Ok(slots)
    }

    async fn truncate_raw(
        &self,
        calendar_id: &str,
        series_id: &str,
        raw_master: &GraphEvent,
        from: Cut,
        notify: bool,
    ) -> Result<bool> {
        let cats = self.category_map().await;
        let master = event_from_graph(raw_master.clone(), &self.email, &cats);
        if master.start.as_ref().is_none_or(|first| !from.keeps(first)) {
            self.delete_event(calendar_id, series_id, notify, None)
                .await?;
            return Ok(true);
        }
        let Some(mut rec) = raw_master.recurrence.clone() else {
            self.delete_event(calendar_id, series_id, notify, None)
                .await?;
            return Ok(true);
        };
        let series_tz = master_timezone(raw_master, &master);
        let cut_day = match from {
            Cut::Day(day) => day,
            Cut::At(at) => at.to_zoned(series_tz).date(),
        };
        let end_date = cut_day.yesterday().unwrap_or(cut_day);
        if rec
            .range
            .as_ref()
            .and_then(|r| r.start_date)
            .is_some_and(|start_date| end_date < start_date)
        {
            self.delete_event(calendar_id, series_id, notify, None)
                .await?;
            return Ok(true);
        }
        let range = rec.range.get_or_insert_with(Default::default);
        range.kind = Some(RecurrenceRangeType::EndDate);
        range.end_date = Some(end_date);
        range.number_of_occurrences = None;

        let scope = Self::cal_scope(calendar_id);
        let patch = GraphEventPatch {
            recurrence: Some(Some(rec)),
            ..GraphEventPatch::default()
        };
        self.send(ATTEMPTS, |client| {
            client
                .calendar()
                .update_event(scope, series_id, &patch)
                .timezone("UTC")
        })
        .await?;
        self.drop_master(series_id);
        Ok(false)
    }
}

impl CalendarProvider for OutlookCalendar {
    fn kind(&self) -> CalendarProviderKind {
        CalendarProviderKind::Microsoft
    }

    fn email(&self) -> &str {
        &self.email
    }

    fn list_calendars(&self) -> BoxFuture<'_, Result<Vec<Calendar>>> {
        Box::pin(async move {
            self.refresh_categories().await;
            let page = self
                .send(ATTEMPTS, |client| client.calendar().calendars().all())
                .await?;
            Ok(page
                .value
                .into_iter()
                .filter(|cal| cal.id.is_some())
                .map(calendar_from_graph)
                .collect())
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
            let Some(link) = self.get_delta(calendar_id, from, to) else {
                return self.sync_delta_full(calendar_id, from, to).await;
            };
            let cats = self.category_map().await;
            match self
                .send(ATTEMPTS, |client| {
                    client
                        .follow::<GraphEvent>(&link)
                        .timezone("UTC")
                        .max_page_size(DELTA_PAGE_SIZE)
                        .all()
                })
                .await
            {
                Ok(page) => {
                    let on_commit = self.delta_commit(calendar_id, from, to, page.delta_link);
                    let latest = self.delta_events(calendar_id, page.value, &cats).await?;
                    let mut updated = Vec::new();
                    let mut removed = Vec::new();
                    for (id, item) in latest {
                        match item {
                            Some(ev) => updated.push(ev),
                            None => removed.push(id),
                        }
                    }
                    Ok(WindowSync {
                        events: WindowEvents::Delta { updated, removed },
                        on_commit,
                    })
                }
                Err(failure) if is_sync_state_expired(&failure) => {
                    self.drop_delta(calendar_id, from, to);
                    self.sync_delta_full(calendar_id, from, to).await
                }
                Err(failure) => Err(failure.into()),
            }
        })
    }

    fn list_events<'a>(
        &'a self,
        calendar_id: &'a str,
        from: Timestamp,
        to: Timestamp,
        _tz: &'a TimeZone,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>>> {
        Box::pin(self.list_view(calendar_id, from, to))
    }

    fn series_recurrence<'a>(
        &'a self,
        calendar_id: &'a str,
        series_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Recurrence>>> {
        Box::pin(async move {
            let raw = self.fetch_raw_event(calendar_id, series_id).await?;
            Ok(raw.recurrence.as_ref().and_then(Recurrence::from_graph))
        })
    }

    fn create_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event: &'a CalendarEvent,
        _notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>> {
        Box::pin(async move {
            let scope = Self::cal_scope(calendar_id);
            let cats = self.category_map().await;
            let wire = event_to_graph(event, &self.email);
            let created = self
                .send(ATTEMPTS, |client| {
                    client.calendar().create_event(scope, &wire).timezone("UTC")
                })
                .await?;
            Ok(event_from_graph(created, &self.email, &cats))
        })
    }

    fn patch_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event_id: &'a str,
        patch: &'a EventPatch,
        shift: Option<(SignedDuration, SignedDuration)>,
        _notify: bool,
    ) -> BoxFuture<'a, Result<CalendarEvent>> {
        Box::pin(async move {
            let scope = Self::cal_scope(calendar_id);
            let cats = self.category_map().await;
            let mut effective = patch.clone();
            let mut raw_recurrence_override = None;
            let needs_anchor =
                matches!(effective.recurrence, Some(Some(_))) && effective.start.is_none();
            let (existing_start, fallback_zone) = if shift.is_some() || needs_anchor {
                let raw_master = self.fetch_raw_event(calendar_id, event_id).await?;
                let master = event_from_graph(raw_master.clone(), &self.email, &cats);
                let zone_name = raw_master
                    .recurrence
                    .as_ref()
                    .and_then(|r| r.range.as_ref())
                    .and_then(|r| r.recurrence_time_zone.clone())
                    .or_else(|| {
                        master_timezone(&raw_master, &master)
                            .iana_name()
                            .map(str::to_owned)
                    });
                if let Some((start_delta, end_delta)) = shift {
                    let start_template = effective.start.clone();
                    let end_template = effective.end.clone();
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
                    if let (Some(target), Some(mut rec)) = (&new_start, raw_master.recurrence)
                        && effective.recurrence.is_none()
                        && let Some(new_date) = event_time_date(target)
                        && rec.range.as_ref().and_then(|r| r.start_date) != Some(new_date)
                    {
                        rec.range.get_or_insert_with(Default::default).start_date = Some(new_date);
                        raw_recurrence_override = Some(rec);
                    }
                    effective.start = new_start.or(start_template);
                    effective.end = new_end.or(end_template);
                }
                (master.start, zone_name)
            } else {
                (None, None)
            };
            let anchor_start = effective.start.as_ref().or(existing_start.as_ref());
            let mut wire = patch_to_graph(
                &effective,
                &self.email,
                anchor_start,
                fallback_zone.as_deref(),
            );
            if let Some(rec) = raw_recurrence_override {
                wire.recurrence = Some(Some(rec));
            }
            let updated = self
                .send(ATTEMPTS, |client| {
                    client
                        .calendar()
                        .update_event(scope, event_id, &wire)
                        .timezone("UTC")
                })
                .await?;
            self.drop_master(event_id);
            Ok(event_from_graph(updated, &self.email, &cats))
        })
    }

    fn delete_event<'a>(
        &'a self,
        calendar_id: &'a str,
        event_id: &'a str,
        _notify: bool,
        comment: Option<&'a str>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let scope = Self::cal_scope(calendar_id);
            let result = if let Some(note) = comment {
                let note = (!note.is_empty()).then_some(note);
                self.send(ATTEMPTS, |client| {
                    client.calendar().cancel(scope, event_id, note)
                })
                .await
            } else {
                self.send(ATTEMPTS, |client| {
                    client.calendar().delete_event(scope, event_id)
                })
                .await
            };
            match result {
                // Already deleted by an attempt whose response was lost or elsewhere.
                Err(failure) if is_not_found_or_gone(&failure) => {
                    self.drop_master(event_id);
                    Ok(())
                }
                Ok(()) => {
                    self.drop_master(event_id);
                    Ok(())
                }
                Err(failure) => Err(failure.into()),
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
            let id = if all {
                event.recurring_event_id.as_deref().or(event.id.as_deref())
            } else {
                event.id.as_deref()
            }
            .unwrap_or_default();
            self.send_rsvp(calendar_id, id, status).await?;
            Ok(None)
        })
    }

    fn respond_to_invite<'a>(
        &'a self,
        calendar_id: &'a str,
        invite: &'a CalendarEvent,
        status: RsvpStatus,
        _links: Option<&'a InviteLinks>,
    ) -> BoxFuture<'a, Result<Option<CalendarEvent>>> {
        Box::pin(async move {
            let Some(uid) = invite
                .ical_uid
                .as_deref()
                .map(str::trim)
                .filter(|uid| !uid.is_empty())
            else {
                return Err(Error::EventCancelled);
            };
            let scope = Self::cal_scope(calendar_id);
            let cats = self.category_map().await;
            let page = self
                .send(ATTEMPTS, |client| {
                    client
                        .calendar()
                        .events_by_ical_uid(scope, uid)
                        .timezone("UTC")
                        .all()
                })
                .await?;
            let had_any = !page.value.is_empty();
            let chosen = {
                let mut live = page
                    .value
                    .into_iter()
                    .filter(|ev| ev.is_cancelled != Some(true))
                    .collect::<Vec<_>>();
                live.iter()
                    .position(|ev| ev.kind == Some(GraphEventType::SeriesMaster))
                    .map(|idx| live.swap_remove(idx))
                    .or_else(|| (!live.is_empty()).then(|| live.swap_remove(0)))
            };
            match chosen {
                Some(found) => {
                    if status == RsvpStatus::NeedsAction {
                        return Ok(Some(event_from_graph(found, &self.email, &cats)));
                    }
                    let target_id = found
                        .series_master_id
                        .as_deref()
                        .or(found.id.as_deref())
                        .unwrap_or_default();
                    self.send_rsvp(calendar_id, target_id, status).await?;
                    Ok(None)
                }
                None if !had_any && status == RsvpStatus::NeedsAction => {
                    // "Add to Calendar" when Exchange has not yet placed a copy on the calendar.
                    let created = self.create_event(calendar_id, invite, false).await?;
                    Ok(Some(created))
                }
                None => Err(Error::EventCancelled),
            }
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
            let raw_master = self.fetch_raw_event(calendar_id, series_id).await?;
            self.truncate_raw(calendar_id, series_id, &raw_master, from, notify)
                .await
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
            let cats = self.category_map().await;
            let raw_master = self.fetch_raw_event(calendar_id, series_id).await?;
            let master = event_from_graph(raw_master.clone(), &self.email, &cats);
            let new_start = patch.start.clone().unwrap_or_else(|| start.clone());
            let new_end = patch.end.clone().unwrap_or_else(|| end.clone());
            if master
                .start
                .as_ref()
                .is_none_or(|first| !from.keeps(first) || first == &new_start)
            {
                let first_patch = EventPatch {
                    id: None,
                    start: Some(new_start.clone()),
                    end: Some(new_end),
                    ..patch.clone()
                };
                if first_patch.recurrence.is_none()
                    && let Some(mut rec) = raw_master.recurrence.clone()
                    && let Some(new_date) = event_time_date(&new_start)
                {
                    rec.range.get_or_insert_with(Default::default).start_date = Some(new_date);
                    let mut wire =
                        patch_to_graph(&first_patch, &self.email, Some(&new_start), None);
                    wire.recurrence = Some(Some(rec));
                    let scope = Self::cal_scope(calendar_id);
                    let updated = self
                        .send(ATTEMPTS, |client| {
                            client
                                .calendar()
                                .update_event(scope, series_id, &wire)
                                .timezone("UTC")
                        })
                        .await?;
                    return Ok(event_from_graph(updated, &self.email, &cats));
                }
                return self
                    .patch_event(calendar_id, series_id, &first_patch, None, notify)
                    .await;
            }

            // Build the replacement series with `transaction_id = patch.id` for retry idempotency.
            let tx_id = patch.id.clone().unwrap_or_else(fresh_event_id);
            let mut new_series = CalendarEvent {
                id: None,
                provider: CalendarProviderKind::Microsoft,
                summary: master.summary.clone(),
                description: master.description.clone(),
                location: master.location.clone(),
                attendees: master.attendees.clone(),
                conference: master.conference.clone(),
                categories: master.categories.clone(),
                show_as: master.show_as,
                visibility: master.visibility,
                reminders: master.reminders.clone(),
                guests_can_see_other_guests: master.guests_can_see_other_guests,
                transaction_id: Some(tx_id),
                ..CalendarEvent::default()
            };
            new_series.apply_patch(patch);
            new_series.start = Some(new_start.clone());
            new_series.end = Some(new_end);

            let mut wire_new = event_to_graph(&new_series, &self.email);
            if patch.description.is_none() && raw_master.body.is_some() {
                wire_new.body = raw_master.body.clone();
            }
            if patch.recurrence.is_none() {
                if let (Some(rule), Some(first)) = (
                    master.recurrence.as_ref(),
                    master
                        .start
                        .as_ref()
                        .and_then(|s| s.own_zoned(&master_timezone(&raw_master, &master))),
                ) {
                    let cut_day = match from {
                        Cut::Day(day) => day,
                        Cut::At(at) => at.to_zoned(first.time_zone().clone()).date(),
                    };
                    let remaining = rule.from_day(&first, cut_day);
                    let start_date = event_time_date(&new_start).unwrap_or(cut_day);
                    let zone_name = new_start
                        .time_zone()
                        .or_else(|| {
                            raw_master
                                .recurrence
                                .as_ref()
                                .and_then(|r| r.range.as_ref())
                                .and_then(|r| r.recurrence_time_zone.as_deref())
                        })
                        .unwrap_or("UTC");
                    wire_new.recurrence = Some(remaining.to_graph(start_date, zone_name));
                } else if let Some(mut rec) = raw_master.recurrence.clone() {
                    if let Some(start_date) = event_time_date(&new_start) {
                        rec.range.get_or_insert_with(Default::default).start_date =
                            Some(start_date);
                    }
                    wire_new.recurrence = Some(rec);
                }
            }

            // Create new series first (idempotent via `transactionId`) before truncating the old
            // master so retries after partial failure never copy the truncated `endDate`.
            let scope = Self::cal_scope(calendar_id);
            let created = self
                .send(ATTEMPTS, |client| {
                    client
                        .calendar()
                        .create_event(scope, &wire_new)
                        .timezone("UTC")
                })
                .await?;
            self.truncate_raw(calendar_id, series_id, &raw_master, from, notify)
                .await?;
            Ok(event_from_graph(created, &self.email, &cats))
        })
    }

    fn free_busy<'a>(
        &'a self,
        calendar_ids: &'a [String],
        from: Timestamp,
        to: Timestamp,
    ) -> BoxFuture<'a, Result<Vec<(String, Option<Vec<FreeBusySlot>>)>>> {
        Box::pin(async move {
            let mut results: HashMap<String, Option<Vec<FreeBusySlot>>> = HashMap::new();
            let mut emails = Vec::new();
            let mut opaque_ids = Vec::new();
            for id in calendar_ids {
                if id.contains('@') {
                    emails.push(id.clone());
                } else {
                    opaque_ids.push(id.clone());
                }
            }
            if !emails.is_empty() {
                let request = ScheduleRequest {
                    schedules: emails.clone(),
                    start_time: DateTimeTimeZone::utc(from),
                    end_time: DateTimeTimeZone::utc(to),
                    availability_view_interval: Some(30),
                };
                match self
                    .send(ATTEMPTS, |client| {
                        client.calendar().get_schedule(&request).timezone("UTC")
                    })
                    .await
                {
                    Ok(page) => {
                        for info in page.value {
                            let Some(id) = info.schedule_id else {
                                continue;
                            };
                            if info.error.is_some() {
                                results.insert(id, None);
                                continue;
                            }
                            let slots = info
                                .schedule_items
                                .into_iter()
                                .filter_map(|item| {
                                    let status = map_free_busy(item.status)?;
                                    let start = parse_dtz(item.start.as_ref()?)?;
                                    let end = parse_dtz(item.end.as_ref()?)?;
                                    Some(FreeBusySlot { start, end, status })
                                })
                                .collect();
                            results.insert(id, Some(slots));
                        }
                    }
                    Err(failure) if is_access_denied(&failure) => {
                        // Personal Outlook.com accounts return 403 on `getSchedule`:
                        // fall back to `calendarView` for our own address and `None` for guests.
                        for addr in &emails {
                            if addr.eq_ignore_ascii_case(&self.email) {
                                let slots = self.calendar_view_busy("", from, to).await?;
                                results.insert(addr.clone(), Some(slots));
                            } else {
                                results.insert(addr.clone(), None);
                            }
                        }
                    }
                    Err(failure) => return Err(failure.into()),
                }
            }
            for cal_id in opaque_ids {
                match self.calendar_view_busy(&cal_id, from, to).await {
                    Ok(slots) => {
                        results.insert(cal_id, Some(slots));
                    }
                    Err(err) if is_unreadable_calendar_error(&err) => {
                        results.insert(cal_id, None);
                    }
                    Err(err) => return Err(err),
                }
            }
            Ok(calendar_ids
                .iter()
                .map(|id| {
                    let found = results.remove(id).or_else(|| {
                        // Graph may normalize case in `scheduleId`.
                        let key = results
                            .keys()
                            .find(|k| k.eq_ignore_ascii_case(id))
                            .cloned()?;
                        results.remove(&key)
                    });
                    (id.clone(), found.flatten())
                })
                .collect())
        })
    }

    fn attendee_events<'a>(
        &'a self,
        guest_email: &'a str,
        from: Timestamp,
        to: Timestamp,
    ) -> BoxFuture<'a, Result<Option<Vec<CalendarEvent>>>> {
        Box::pin(async move {
            let request = ScheduleRequest {
                schedules: vec![guest_email.to_owned()],
                start_time: DateTimeTimeZone::utc(from),
                end_time: DateTimeTimeZone::utc(to),
                availability_view_interval: Some(30),
            };
            let page = match self
                .send(ATTEMPTS, |client| {
                    client.calendar().get_schedule(&request).timezone("UTC")
                })
                .await
            {
                Ok(page) => page,
                Err(failure) if is_access_denied(&failure) => return Ok(None),
                Err(failure) => return Err(failure.into()),
            };
            let Some(info) = page.value.into_iter().next() else {
                return Ok(None);
            };
            if info.error.is_some() {
                return Ok(None);
            }
            let events = info
                .schedule_items
                .into_iter()
                .filter_map(|item| {
                    let show_as = map_free_busy(item.status)?;
                    let start = parse_dtz(item.start.as_ref()?)?;
                    let end = parse_dtz(item.end.as_ref()?)?;
                    let private = item.is_private == Some(true);
                    let event_type = if show_as == ShowAs::OutOfOffice {
                        EventType::OutOfOffice
                    } else {
                        EventType::Default
                    };
                    Some(CalendarEvent {
                        provider: CalendarProviderKind::Microsoft,
                        summary: (!private)
                            .then_some(item.subject)
                            .flatten()
                            .filter(|s| !s.is_empty()),
                        location: (!private)
                            .then_some(item.location)
                            .flatten()
                            .filter(|s| !s.is_empty()),
                        start: Some(EventTime::timed(start, None)),
                        end: Some(EventTime::timed(end, None)),
                        visibility: if private {
                            Visibility::Private
                        } else {
                            Visibility::Default
                        },
                        show_as,
                        event_type,
                        ..CalendarEvent::default()
                    })
                })
                .collect();
            Ok(Some(events))
        })
    }
}

fn is_access_denied(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Permanent(ProviderError::Outlook(error))
            if error.status() == Some(403) || error.code() == Some("ErrorAccessDenied")
    )
}

fn is_not_found_or_gone(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Permanent(ProviderError::Outlook(error))
            if matches!(error.status(), Some(404 | 410))
                || error.code() == Some("ErrorItemNotFound")
    )
}

fn is_sync_state_expired(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Permanent(ProviderError::Outlook(error))
            if error.status() == Some(410)
                || matches!(
                    error.code(),
                    Some("SyncStateNotFound" | "ErrorInvalidSyncStateData" | "SyncStateInvalid")
                )
    )
}

fn map_free_busy(status: Option<FreeBusyStatus>) -> Option<ShowAs> {
    match status {
        Some(FreeBusyStatus::Free | FreeBusyStatus::Unknown) => None,
        Some(FreeBusyStatus::Tentative) => Some(ShowAs::Tentative),
        Some(FreeBusyStatus::Oof) => Some(ShowAs::OutOfOffice),
        Some(FreeBusyStatus::WorkingElsewhere) => Some(ShowAs::WorkingElsewhere),
        Some(FreeBusyStatus::Busy | FreeBusyStatus::UnknownFutureValue) | None => {
            Some(ShowAs::Busy)
        }
    }
}

fn parse_dtz(dtz: &DateTimeTimeZone) -> Option<Timestamp> {
    let tz = gmail::ics::resolve_timezone(&dtz.time_zone).unwrap_or(TimeZone::UTC);
    dtz.date_time.to_zoned(tz).ok().map(|z| z.timestamp())
}

fn event_time_date(time: &EventTime) -> Option<jiff::civil::Date> {
    match time {
        EventTime::AllDay(date) => Some(*date),
        EventTime::Timed { .. } => time.own_zoned(&TimeZone::UTC).map(|z| z.date()),
    }
}

fn master_timezone(raw: &GraphEvent, master: &CalendarEvent) -> TimeZone {
    raw.recurrence
        .as_ref()
        .and_then(|r| r.range.as_ref())
        .and_then(|r| r.recurrence_time_zone.as_deref())
        .and_then(gmail::ics::resolve_timezone)
        .or_else(|| {
            raw.original_start_time_zone
                .as_deref()
                .and_then(gmail::ics::resolve_timezone)
        })
        .or_else(|| {
            master
                .start
                .as_ref()
                .and_then(EventTime::time_zone)
                .and_then(gmail::ics::resolve_timezone)
        })
        .unwrap_or(TimeZone::UTC)
}

fn map_online_meeting_provider(kind: OnlineMeetingProviderType) -> Option<OnlineMeetingKind> {
    match kind {
        OnlineMeetingProviderType::TeamsForBusiness => Some(OnlineMeetingKind::TeamsForBusiness),
        OnlineMeetingProviderType::SkypeForConsumer => Some(OnlineMeetingKind::SkypeForConsumer),
        OnlineMeetingProviderType::Unknown
        | OnlineMeetingProviderType::SkypeForBusiness
        | OnlineMeetingProviderType::UnknownFutureValue => None,
    }
}

fn calendar_color_name(color: CalendarColor) -> Option<&'static str> {
    match color {
        CalendarColor::Auto => Some("auto"),
        CalendarColor::LightBlue => Some("lightBlue"),
        CalendarColor::LightGreen => Some("lightGreen"),
        CalendarColor::LightOrange => Some("lightOrange"),
        CalendarColor::LightGray => Some("lightGray"),
        CalendarColor::LightYellow => Some("lightYellow"),
        CalendarColor::LightTeal => Some("lightTeal"),
        CalendarColor::LightPink => Some("lightPink"),
        CalendarColor::LightBrown => Some("lightBrown"),
        CalendarColor::LightRed => Some("lightRed"),
        CalendarColor::MaxColor | CalendarColor::UnknownFutureValue => None,
    }
}

/// Converts a Microsoft Graph [`GraphCalendar`] into the provider-neutral [`Calendar`].
pub(crate) fn calendar_from_graph(cal: GraphCalendar) -> Calendar {
    let primary = cal.is_default_calendar == Some(true);
    let access_role =
        if primary || (cal.can_edit == Some(true) && cal.is_shared_with_me != Some(true)) {
            AccessRole::Owner
        } else if cal.can_edit == Some(true) {
            AccessRole::Writer
        } else {
            AccessRole::Reader
        };
    let can_view_private_items = cal
        .can_view_private_items
        .unwrap_or_else(|| access_role.is_writable());
    let allowed_online_meetings = cal
        .allowed_online_meeting_providers
        .into_iter()
        .filter_map(map_online_meeting_provider)
        .collect();
    let default_online_meeting = cal
        .default_online_meeting_provider
        .and_then(map_online_meeting_provider);
    Calendar {
        id: cal.id.unwrap_or_default(),
        summary: cal.name.unwrap_or_else(|| {
            if primary {
                "Calendar".to_owned()
            } else {
                String::new()
            }
        }),
        summary_override: None,
        primary,
        access_role,
        can_view_private_items,
        background_color: cal.hex_color.filter(|hex| !hex.trim().is_empty()),
        color_name: cal.color.and_then(calendar_color_name).map(str::to_owned),
        time_zone: None,
        hidden: false,
        selected: true,
        deleted: false,
        provider: CalendarProviderKind::Microsoft,
        allowed_online_meetings,
        default_online_meeting,
    }
}

fn resolve_iana_name(name: Option<&str>) -> Option<String> {
    let raw = name?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(iana) = gmail::ics::windows_to_iana(raw) {
        return Some(iana.to_owned());
    }
    gmail::ics::resolve_timezone(raw)
        .and_then(|tz| tz.iana_name().map(str::to_owned))
        .or_else(|| Some(raw.to_owned()))
}

fn time_from_graph(
    dtz: &DateTimeTimeZone,
    all_day: bool,
    original_zone: Option<&str>,
) -> Option<EventTime> {
    if all_day {
        if dtz.date_time.time() != jiff::civil::Time::midnight()
            && let Some(at) = parse_dtz(dtz)
            && let Some(tz) = original_zone.and_then(gmail::ics::resolve_timezone)
        {
            return Some(EventTime::AllDay(at.to_zoned(tz).date()));
        }
        return Some(EventTime::AllDay(dtz.date_time.date()));
    }
    let at = parse_dtz(dtz)?;
    let time_zone = resolve_iana_name(original_zone).or_else(|| {
        (!dtz.time_zone.eq_ignore_ascii_case("UTC"))
            .then(|| resolve_iana_name(Some(&dtz.time_zone)))
            .flatten()
    });
    Some(EventTime::Timed { at, time_zone })
}

fn time_to_graph(time: &EventTime) -> (DateTimeTimeZone, bool) {
    match time {
        EventTime::AllDay(date) => (DateTimeTimeZone::new(date.at(0, 0, 0, 0), "UTC"), true),
        EventTime::Timed { at, time_zone } => {
            if let Some(zone_name) = time_zone.as_deref()
                && let Some(tz) = gmail::ics::resolve_timezone(zone_name)
            {
                let iana = tz.iana_name().unwrap_or(zone_name);
                let graph_zone = gmail::ics::iana_to_windows(iana).unwrap_or(iana).to_owned();
                let local = at.to_zoned(tz).datetime();
                (DateTimeTimeZone::new(local, graph_zone), false)
            } else {
                (DateTimeTimeZone::utc(*at), false)
            }
        }
    }
}

fn response_from_graph(response: Option<ResponseType>) -> RsvpStatus {
    match response {
        Some(ResponseType::Accepted | ResponseType::Organizer) => RsvpStatus::Accepted,
        Some(ResponseType::TentativelyAccepted) => RsvpStatus::Tentative,
        Some(ResponseType::Declined) => RsvpStatus::Declined,
        _ => RsvpStatus::NeedsAction,
    }
}

fn conference_from_graph(event: &GraphEvent) -> Option<Conference> {
    let info = event.online_meeting.as_ref();
    let join_url = info
        .and_then(|m| m.join_url.clone())
        .or_else(|| event.online_meeting_url.clone())
        .filter(|url| !url.trim().is_empty());
    let dial_in = info.and_then(|m| {
        m.toll_number
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| m.toll_free_numbers.first().cloned())
            .or_else(|| m.phones.first().and_then(|p| p.number.clone()))
    });
    if join_url.is_none() && event.is_online_meeting != Some(true) {
        return None;
    }
    let kind = match event.online_meeting_provider {
        Some(OnlineMeetingProviderType::TeamsForBusiness) => ConferenceKind::Teams,
        Some(
            OnlineMeetingProviderType::SkypeForConsumer
            | OnlineMeetingProviderType::SkypeForBusiness,
        ) => ConferenceKind::Other("Skype".to_owned()),
        _ => match join_url.as_deref() {
            Some(url) if url.contains("teams.microsoft.com") || url.contains("/l/meetup-join/") => {
                ConferenceKind::Teams
            }
            Some(url) if url.contains("meet.google.com") => ConferenceKind::GoogleMeet,
            Some(url) if url.contains("zoom.us") => ConferenceKind::Zoom,
            _ if event.is_online_meeting == Some(true) => ConferenceKind::Teams,
            _ => ConferenceKind::Other(String::new()),
        },
    };
    let label = join_url.as_deref().map(|url| {
        if kind == ConferenceKind::Teams {
            "Microsoft Teams".to_owned()
        } else {
            url.trim_start_matches("https://").to_owned()
        }
    });
    Some(Conference {
        kind,
        pending: event.is_online_meeting == Some(true) && join_url.is_none(),
        join_url,
        label,
        dial_in,
    })
}

fn clean_graph_body(body: Option<ItemBody>, preview: Option<&str>) -> Option<String> {
    let content = body.and_then(|b| b.content)?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if preview.is_some_and(|p| p.trim().is_empty())
        && trimmed.eq_ignore_ascii_case("<html><head></head><body></body></html>")
    {
        return None;
    }
    Some(content)
}

/// `occurrence` with what it leaves out taken from its series `master`, except the series'
/// own recurrence and cancelled-occurrence list.
fn inherit(occurrence: GraphEvent, master: &GraphEvent) -> GraphEvent {
    let (Ok(serde_json::Value::Object(own)), Ok(serde_json::Value::Object(mut merged))) = (
        serde_json::to_value(&occurrence),
        serde_json::to_value(master),
    ) else {
        return occurrence;
    };
    merged.remove("recurrence");
    merged.remove("cancelledOccurrences");
    merged.extend(own);
    serde_json::from_value(serde_json::Value::Object(merged)).unwrap_or(occurrence)
}

/// Converts a Microsoft Graph [`GraphEvent`] into the provider-neutral [`CalendarEvent`].
pub(crate) fn event_from_graph(
    event: GraphEvent,
    me: &str,
    categories_map: &HashMap<String, Option<u8>>,
) -> CalendarEvent {
    let all_day = event.is_all_day == Some(true);
    let start = event
        .start
        .as_ref()
        .and_then(|dtz| time_from_graph(dtz, all_day, event.original_start_time_zone.as_deref()));
    let end = event
        .end
        .as_ref()
        .and_then(|dtz| time_from_graph(dtz, all_day, event.original_end_time_zone.as_deref()));
    let orig_start_iana = resolve_iana_name(event.original_start_time_zone.as_deref());
    let original_start = event.original_start.map(|ts| {
        if all_day {
            let tz = orig_start_iana
                .as_deref()
                .and_then(gmail::ics::resolve_timezone)
                .unwrap_or(TimeZone::UTC);
            EventTime::AllDay(ts.to_zoned(tz).date())
        } else {
            EventTime::Timed {
                at: ts,
                time_zone: orig_start_iana.clone(),
            }
        }
    });
    let is_organizer = event.is_organizer == Some(true);
    let organizer = event.organizer.as_ref().and_then(|rec| {
        let addr = rec.email_address.as_ref()?;
        let is_self = is_organizer
            || addr
                .address
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(me));
        Some(EventPerson {
            email: addr.address.clone(),
            display_name: addr.name.clone(),
            is_self,
        })
    });
    let organizer_email = organizer
        .as_ref()
        .and_then(|o| o.email.as_deref())
        .map(str::to_owned);
    let attendees = event
        .attendees
        .iter()
        .map(|att| {
            let email = att.email_address.as_ref().and_then(|e| e.address.clone());
            let display_name = att.email_address.as_ref().and_then(|e| e.name.clone());
            let is_org = organizer_email.as_deref().is_some_and(|org| {
                email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(org))
            });
            let is_self = email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(me));
            EventAttendee {
                id: None,
                email,
                display_name,
                organizer: is_org,
                is_self,
                resource: att.kind == Some(AttendeeType::Resource),
                optional: att.kind == Some(AttendeeType::Optional),
                response_status: response_from_graph(att.status.as_ref().and_then(|s| s.response)),
                comment: None,
                additional_guests: None,
            }
        })
        .collect();
    let self_response = match event.response_status.as_ref().and_then(|s| s.response) {
        Some(ResponseType::Organizer) => None,
        Some(ResponseType::Accepted) => Some(RsvpStatus::Accepted),
        Some(ResponseType::TentativelyAccepted) => Some(RsvpStatus::Tentative),
        Some(ResponseType::Declined) => Some(RsvpStatus::Declined),
        Some(ResponseType::None | ResponseType::NotResponded) if !is_organizer => {
            Some(RsvpStatus::NeedsAction)
        }
        _ => None,
    };
    let status = if event.is_cancelled == Some(true) {
        EventStatus::Cancelled
    } else if event.show_as == Some(FreeBusyStatus::Tentative) && self_response.is_none() {
        EventStatus::Tentative
    } else {
        EventStatus::Confirmed
    };
    let show_as = match event.show_as {
        Some(FreeBusyStatus::Free) => ShowAs::Free,
        Some(FreeBusyStatus::Tentative) => ShowAs::Tentative,
        Some(FreeBusyStatus::Oof) => ShowAs::OutOfOffice,
        Some(FreeBusyStatus::WorkingElsewhere) => ShowAs::WorkingElsewhere,
        _ => ShowAs::Busy,
    };
    let event_type = match show_as {
        ShowAs::OutOfOffice => EventType::OutOfOffice,
        ShowAs::WorkingElsewhere => EventType::WorkingLocation,
        _ => EventType::Default,
    };
    let visibility = match event.sensitivity {
        Some(Sensitivity::Private | Sensitivity::Personal) => Visibility::Private,
        Some(Sensitivity::Confidential) => Visibility::Confidential,
        _ => Visibility::Default,
    };
    let reminders = match event.is_reminder_on {
        Some(false) => EventReminders::None,
        Some(true) => match event.reminder_minutes_before_start {
            Some(minutes) => EventReminders::Custom(vec![minutes]),
            None => EventReminders::Default,
        },
        None => EventReminders::Default,
    };
    let categories = event
        .categories
        .iter()
        .map(|name| EventCategory {
            name: name.clone(),
            preset: categories_map.get(&name.to_lowercase()).copied().flatten(),
        })
        .collect();
    let conference = conference_from_graph(&event);
    let location = event
        .location
        .as_ref()
        .and_then(|loc| loc.display_name.clone())
        .filter(|name| !name.is_empty());
    let recurrence = event.recurrence.as_ref().and_then(Recurrence::from_graph);
    let attachments = event
        .attachments
        .iter()
        .map(|att| EventAttachment {
            file_url: None,
            title: att.name.clone(),
            mime_type: att.content_type.clone(),
            icon_link: None,
            file_id: att.id.clone(),
        })
        .collect();
    let description = clean_graph_body(event.body, event.body_preview.as_deref());
    CalendarEvent {
        id: event.id,
        provider: CalendarProviderKind::Microsoft,
        status,
        summary: event.subject.filter(|s| !s.is_empty()),
        description,
        location,
        start,
        end,
        creator: organizer.clone(),
        organizer,
        attendees,
        attendees_omitted: false,
        self_response,
        guests_can_modify: false,
        guests_can_invite_others: None,
        guests_can_see_other_guests: event.hide_attendees.map(|hide| !hide),
        recurring_event_id: event.series_master_id,
        original_start,
        recurrence,
        raw_recurrence: Vec::new(),
        ical_uid: event.ical_uid,
        html_link: event.web_link,
        conference,
        color_id: None,
        categories,
        visibility,
        show_as,
        event_type,
        attachments,
        reminders,
        transaction_id: event.transaction_id,
    }
}

fn attendees_to_graph(attendees: &[EventAttendee], me: &str) -> Vec<Attendee> {
    attendees
        .iter()
        .filter(|a| !a.organizer && !a.is_me(me))
        .filter_map(|a| {
            let email = a.email.as_deref()?;
            let kind = if a.resource {
                AttendeeType::Resource
            } else if a.optional {
                AttendeeType::Optional
            } else {
                AttendeeType::Required
            };
            Some(Attendee::new(email, a.display_name.as_deref(), kind))
        })
        .collect()
}

fn show_as_to_graph(show_as: ShowAs) -> FreeBusyStatus {
    match show_as {
        ShowAs::Free => FreeBusyStatus::Free,
        ShowAs::Tentative => FreeBusyStatus::Tentative,
        ShowAs::Busy => FreeBusyStatus::Busy,
        ShowAs::OutOfOffice => FreeBusyStatus::Oof,
        ShowAs::WorkingElsewhere => FreeBusyStatus::WorkingElsewhere,
    }
}

fn visibility_to_graph(visibility: Visibility) -> Sensitivity {
    match visibility {
        Visibility::Default | Visibility::Public => Sensitivity::Normal,
        Visibility::Private => Sensitivity::Private,
        Visibility::Confidential => Sensitivity::Confidential,
    }
}

fn is_unreadable_calendar_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Outlook(outlook::Error::Api(api))
            if matches!(api.status, 403 | 404 | 410)
                || (api.status == 400 && api.error.code == "ErrorInvalidIdMalformed")
    )
}

/// Whether `text` appears to contain HTML markup rather than plain text (ignoring email brackets
/// like `<alice@example.com>` and comparison operators like `a < b`).
fn looks_like_html(text: &str) -> bool {
    const TAGS: &[&str] = &[
        "a",
        "b",
        "blockquote",
        "body",
        "br",
        "code",
        "div",
        "em",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "hr",
        "html",
        "i",
        "li",
        "ol",
        "p",
        "pre",
        "span",
        "strong",
        "table",
        "td",
        "th",
        "tr",
        "u",
        "ul",
    ];
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(rel) = text[i..].find('<') {
        let start = i + rel + 1;
        let rest = &bytes[start..];
        let after_slash = rest.strip_prefix(b"/").unwrap_or(rest);
        let name_len = after_slash
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        if name_len > 0 {
            let name = &after_slash[..name_len];
            let next = after_slash.get(name_len).copied();
            let boundary = matches!(next, Some(b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n'));
            if boundary
                && TAGS
                    .iter()
                    .any(|tag| tag.as_bytes().eq_ignore_ascii_case(name))
                && after_slash[name_len..].contains(&b'>')
            {
                return true;
            }
        }
        i = start;
    }
    false
}

fn description_to_body(desc: &str) -> ItemBody {
    ItemBody {
        content_type: Some(if looks_like_html(desc) {
            BodyType::Html
        } else {
            BodyType::Text
        }),
        content: Some(desc.to_owned()),
    }
}

fn recurrence_to_graph(
    rule: &Recurrence,
    start: Option<&EventTime>,
    fallback_zone: Option<&str>,
) -> Option<PatternedRecurrence> {
    let start = start?;
    let start_date = event_time_date(start)?;
    let zone = start.time_zone().or(fallback_zone).unwrap_or("UTC");
    Some(rule.to_graph(start_date, zone))
}

fn event_to_graph(event: &CalendarEvent, me: &str) -> GraphEvent {
    let (start, start_all_day) = match &event.start {
        Some(t) => {
            let (dtz, all_day) = time_to_graph(t);
            (Some(dtz), Some(all_day))
        }
        None => (None, None),
    };
    let end = event.end.as_ref().map(|t| time_to_graph(t).0);
    let (is_online_meeting, online_meeting_provider) = match &event.conference {
        Some(conf) => match &conf.kind {
            ConferenceKind::Teams => (
                Some(true),
                Some(OnlineMeetingProviderType::TeamsForBusiness),
            ),
            ConferenceKind::Other(name) if name.eq_ignore_ascii_case("skype") => (
                Some(true),
                Some(OnlineMeetingProviderType::SkypeForConsumer),
            ),
            _ => (None, None),
        },
        None => (None, None),
    };
    let show_as = if event.event_type == EventType::OutOfOffice {
        Some(FreeBusyStatus::Oof)
    } else if event.event_type == EventType::WorkingLocation {
        Some(FreeBusyStatus::WorkingElsewhere)
    } else if event.show_as != ShowAs::Busy {
        Some(show_as_to_graph(event.show_as))
    } else {
        None
    };
    let sensitivity =
        (event.visibility != Visibility::Default).then(|| visibility_to_graph(event.visibility));
    let (is_reminder_on, reminder_minutes_before_start) = match &event.reminders {
        EventReminders::Default => (None, None),
        EventReminders::None => (Some(false), None),
        EventReminders::Custom(minutes) => (Some(true), minutes.first().copied()),
    };
    let recurrence = event
        .recurrence
        .as_ref()
        .and_then(|rule| recurrence_to_graph(rule, event.start.as_ref(), None));
    GraphEvent {
        subject: event.summary.clone(),
        body: event.description.as_deref().map(description_to_body),
        start,
        end,
        is_all_day: start_all_day,
        location: event.location.as_ref().map(Location::new),
        attendees: attendees_to_graph(&event.attendees, me),
        recurrence,
        show_as,
        sensitivity,
        categories: event.categories.iter().map(|c| c.name.clone()).collect(),
        is_reminder_on,
        reminder_minutes_before_start,
        hide_attendees: event.guests_can_see_other_guests.map(|see| !see),
        is_online_meeting,
        online_meeting_provider,
        // Use local optimistic `event.id` as Graph's idempotency key (`transactionId`).
        transaction_id: event.transaction_id.clone().or_else(|| event.id.clone()),
        ..GraphEvent::default()
    }
}

fn patch_to_graph(
    patch: &EventPatch,
    me: &str,
    anchor_start: Option<&EventTime>,
    fallback_zone: Option<&str>,
) -> GraphEventPatch {
    let (start, start_all_day) = match &patch.start {
        Some(t) => {
            let (dtz, all_day) = time_to_graph(t);
            (Some(dtz), Some(all_day))
        }
        None => (None, None),
    };
    let end = patch.end.as_ref().map(|t| time_to_graph(t).0);
    let (is_online_meeting, online_meeting_provider) = match &patch.conference {
        Some(ConferencePatch::Create(OnlineMeetingKind::TeamsForBusiness)) => (
            Some(true),
            Some(OnlineMeetingProviderType::TeamsForBusiness),
        ),
        Some(ConferencePatch::Create(OnlineMeetingKind::SkypeForConsumer)) => (
            Some(true),
            Some(OnlineMeetingProviderType::SkypeForConsumer),
        ),
        // Graph ignores turning `isOnlineMeeting` off post-create, so omit on `Remove`.
        _ => (None, None),
    };
    let recurrence = match &patch.recurrence {
        None => None,
        Some(None) => Some(None),
        Some(Some(rule)) => {
            recurrence_to_graph(rule, anchor_start.or(patch.start.as_ref()), fallback_zone)
                .map(Some)
        }
    };
    let (is_reminder_on, reminder_minutes_before_start) = match &patch.reminders {
        None | Some(EventReminders::Default) => (None, None),
        Some(EventReminders::None) => (Some(false), None),
        Some(EventReminders::Custom(minutes)) => (Some(true), minutes.first().copied()),
    };
    GraphEventPatch {
        subject: patch.summary.clone(),
        body: patch.description.as_deref().map(description_to_body),
        start,
        end,
        is_all_day: start_all_day,
        location: patch.location.as_ref().map(Location::new),
        attendees: patch
            .attendees
            .as_ref()
            .map(|list| attendees_to_graph(list, me)),
        recurrence,
        show_as: patch.show_as.map(show_as_to_graph),
        sensitivity: patch.visibility.map(visibility_to_graph),
        categories: patch
            .categories
            .as_ref()
            .map(|list| list.iter().map(|c| c.name.clone()).collect()),
        is_reminder_on,
        reminder_minutes_before_start,
        hide_attendees: patch.guests_can_see_other_guests.map(|see| !see),
        is_online_meeting,
        online_meeting_provider,
        ..GraphEventPatch::default()
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    #[test]
    fn all_day_events_in_utc_plus_and_minus_zones_preserve_local_date() {
        // Asia/Tokyo (UTC+9): 2026-11-17 00:00 JST arrives under Prefer UTC as 2026-11-16T15:00:00.
        let tokyo_dtz = DateTimeTimeZone::new(date(2026, 11, 16).at(15, 0, 0, 0), "UTC");
        assert_eq!(
            time_from_graph(&tokyo_dtz, true, Some("Tokyo Standard Time")),
            Some(EventTime::AllDay(date(2026, 11, 17)))
        );

        // America/New_York (UTC-5): 2026-11-17 00:00 EST arrives under Prefer UTC as 2026-11-17T05:00:00.
        let ny_dtz = DateTimeTimeZone::new(date(2026, 11, 17).at(5, 0, 0, 0), "UTC");
        assert_eq!(
            time_from_graph(&ny_dtz, true, Some("Eastern Standard Time")),
            Some(EventTime::AllDay(date(2026, 11, 17)))
        );

        // Floating midnight UTC keeps its date directly.
        let utc_dtz = DateTimeTimeZone::new(date(2026, 11, 17).at(0, 0, 0, 0), "UTC");
        assert_eq!(
            time_from_graph(&utc_dtz, true, Some("Tokyo Standard Time")),
            Some(EventTime::AllDay(date(2026, 11, 17)))
        );
    }

    #[test]
    fn description_to_body_uses_text_for_plain_and_html_only_for_markup() {
        let plain = description_to_body("Agenda with <alice@example.com> & Bob\nCheck a < b");
        assert_eq!(plain.content_type, Some(BodyType::Text));
        assert_eq!(
            plain.content.as_deref(),
            Some("Agenda with <alice@example.com> & Bob\nCheck a < b")
        );

        let html = description_to_body("<p>Agenda &amp; notes</p>");
        assert_eq!(html.content_type, Some(BodyType::Html));

        let br = description_to_body("Line 1<br>Line 2");
        assert_eq!(br.content_type, Some(BodyType::Html));
    }

    #[test]
    fn bare_delta_occurrences_inherit_their_series_details() {
        let master: GraphEvent = serde_json::from_value(serde_json::json!({
            "id": "master",
            "type": "seriesMaster",
            "subject": "Standup",
            "location": { "displayName": "Room 1" },
            "recurrence": { "pattern": { "type": "daily", "interval": 1 } },
            "start": { "dateTime": "2026-11-02T09:00:00", "timeZone": "UTC" },
        }))
        .expect("valid master");
        let occurrence: GraphEvent = serde_json::from_value(serde_json::json!({
            "id": "occurrence",
            "type": "occurrence",
            "seriesMasterId": "master",
            "start": { "dateTime": "2026-11-26T09:00:00", "timeZone": "UTC" },
        }))
        .expect("valid occurrence");
        let filled = inherit(occurrence.clone(), &master);
        assert_eq!(filled.subject.as_deref(), Some("Standup"));
        assert!(filled.location.is_some());
        assert_eq!(filled.id, occurrence.id);
        assert_eq!(filled.kind, occurrence.kind);
        assert_eq!(filled.start, occurrence.start);
        assert!(filled.recurrence.is_none());
    }

    #[tokio::test]
    async fn delta_events_hydrates_bare_occurrences_and_respects_page_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let fetched_remote = Arc::new(AtomicUsize::new(0));
        let fetched_gone = Arc::new(AtomicUsize::new(0));
        let remote_hits = Arc::clone(&fetched_remote);
        let gone_hits = Arc::clone(&fetched_gone);

        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                let (status, body) = if path.contains("/events/master-remote") {
                    remote_hits.fetch_add(1, Ordering::Relaxed);
                    (
                        "200 OK",
                        serde_json::json!({
                            "id": "master-remote",
                            "type": "seriesMaster",
                            "subject": "Fetched weekly",
                            "location": { "displayName": "Teams" },
                            "start": { "dateTime": "2026-11-01T14:00:00", "timeZone": "UTC" },
                            "end": { "dateTime": "2026-11-01T15:00:00", "timeZone": "UTC" },
                        })
                        .to_string(),
                    )
                } else if path.contains("/events/master-gone") {
                    gone_hits.fetch_add(1, Ordering::Relaxed);
                    (
                        "404 Not Found",
                        serde_json::json!({
                            "error": { "code": "ErrorItemNotFound", "message": "not found" }
                        })
                        .to_string(),
                    )
                } else {
                    ("404 Not Found", "{}".to_owned())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });

        let base = Url::parse(&format!("http://{addr}")).expect("url");
        let client = Client::new(
            outlook::Endpoints::mock(&base),
            outlook::StaticToken("tok".into()),
        );
        let dir = std::env::temp_dir().join(format!("exo-cal-delta-test-{}", std::process::id()));
        let cal = OutlookCalendar::new(
            "microsoft test@example.com".into(),
            "test@example.com".into(),
            client,
            Duration::from_millis(1),
            Debounce::default(),
            &dir,
            None,
        );

        let events: Vec<GraphEvent> = serde_json::from_value(serde_json::json!([
            // Bare occurrence appearing BEFORE its seriesMaster in the same delta.
            {
                "id": "occ-inline",
                "type": "occurrence",
                "seriesMasterId": "master-inline",
                "start": { "dateTime": "2026-11-26T09:00:00", "timeZone": "UTC" },
                "end": { "dateTime": "2026-11-26T09:30:00", "timeZone": "UTC" },
            },
            {
                "id": "master-inline",
                "type": "seriesMaster",
                "subject": "Daily Standup",
                "location": { "displayName": "Room A" },
                "recurrence": { "pattern": { "type": "daily", "interval": 1 } },
                "start": { "dateTime": "2026-11-01T09:00:00", "timeZone": "UTC" },
                "end": { "dateTime": "2026-11-01T09:30:00", "timeZone": "UTC" },
            },
            // Occurrence present early, then removed later in the same delta -> must stay None.
            {
                "id": "occ-deleted",
                "type": "occurrence",
                "seriesMasterId": "master-inline",
                "start": { "dateTime": "2026-11-27T09:00:00", "timeZone": "UTC" },
            },
            {
                "id": "occ-deleted",
                "@removed": { "reason": "deleted" },
            },
            // Two occurrences sharing a master not in the delta -> fetched once, cached once.
            {
                "id": "occ-remote-1",
                "type": "occurrence",
                "seriesMasterId": "master-remote",
                "start": { "dateTime": "2026-11-26T14:00:00", "timeZone": "UTC" },
                "end": { "dateTime": "2026-11-26T15:00:00", "timeZone": "UTC" },
            },
            {
                "id": "occ-remote-2",
                "type": "occurrence",
                "seriesMasterId": "master-remote",
                "start": { "dateTime": "2026-12-03T14:00:00", "timeZone": "UTC" },
                "end": { "dateTime": "2026-12-03T15:00:00", "timeZone": "UTC" },
            },
            // Two occurrences sharing a 404 master -> only 1 Graph request, no sync error.
            {
                "id": "occ-gone-1",
                "type": "occurrence",
                "seriesMasterId": "master-gone",
                "start": { "dateTime": "2026-11-26T16:00:00", "timeZone": "UTC" },
            },
            {
                "id": "occ-gone-2",
                "type": "occurrence",
                "seriesMasterId": "master-gone",
                "start": { "dateTime": "2026-11-27T16:00:00", "timeZone": "UTC" },
            },
        ]))
        .expect("valid delta events");

        let cats = HashMap::new();
        let out = cal
            .delta_events("primary", events, &cats)
            .await
            .expect("delta_events succeeds");

        let inline = out["occ-inline"].as_ref().expect("inline present");
        assert_eq!(inline.summary.as_deref(), Some("Daily Standup"));
        assert_eq!(inline.location.as_deref(), Some("Room A"));
        assert!(inline.recurrence.is_none());

        assert!(
            out["occ-deleted"].is_none(),
            "later @removed must not be resurrected by earlier occurrence"
        );
        assert_eq!(
            out["occ-remote-1"]
                .as_ref()
                .and_then(|e| e.summary.as_deref()),
            Some("Fetched weekly")
        );
        assert_eq!(
            out["occ-remote-2"]
                .as_ref()
                .and_then(|e| e.summary.as_deref()),
            Some("Fetched weekly")
        );
        assert_eq!(fetched_remote.load(Ordering::Relaxed), 1);
        assert_eq!(fetched_gone.load(Ordering::Relaxed), 1);
        assert!(out["occ-gone-1"].is_some());

        // Removing the series master evicts it from the cache.
        let remove_master: Vec<GraphEvent> = serde_json::from_value(serde_json::json!([
            { "id": "master-inline", "@removed": { "reason": "deleted" } },
        ]))
        .expect("valid removal");
        cal.delta_events("primary", remove_master, &cats)
            .await
            .expect("removal succeeds");
        assert!(
            !cal.masters
                .lock()
                .expect("lock")
                .contains_key("master-inline")
        );

        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
