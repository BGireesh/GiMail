//! The calendar cache: the user's calendar list and the events of the days the calendar has
//! shown, fetched from Google Calendar or Microsoft Graph a window at a time.
//!
//! Each account's calendar backend sits behind [`provider::CalendarProvider`]. Changes to events
//! wait in their own persistent queue until the provider has them, and every read overlays them
//! ([`outbox`]).
//!
//! Additional Google or Microsoft accounts can be signed in for their calendars alone
//! ([`Store::add_calendar_account`]), keyed by canonical account key (`email` for Google,
//! `"microsoft {email}"` for Microsoft). Their lists sit beside the mail account's, and each
//! calendar is read and changed through one account: the one whose primary calendar it is, else
//! the one with the most access to it ([`Store::calendars`]). Events are cached by calendar,
//! whichever account read them.
//!
//! ```no_run
//! # async fn demo(store: &exo_store::Store) -> exo_store::Result<()> {
//! use jiff::{civil::date, tz::TimeZone};
//!
//! let tz = TimeZone::get("America/New_York").expect("zone");
//! let calendars = store.sync_calendars("maya@brightmoor.co").await?;
//! if let Some(primary) = calendars.iter().find(|calendar| calendar.primary) {
//!     let days = date(2026, 11, 15)..date(2026, 11, 22);
//!     store.sync_events(&primary.id, days.clone(), &tz).await?;
//!     let week = store.events(&[primary.id.clone()], days, &tz).await?;
//! }
//! # Ok(()) }
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use toasty::{Executor, Json};

use crate::db::{CalendarRow, EventRow, GuestCacheRow, IN_LIST_CHUNK};
use crate::error::{Error, Result};
use crate::store::Store;

pub(crate) mod google;
pub(crate) mod model;
pub(crate) mod outbox;
pub(crate) mod outlook;
pub(crate) mod provider;
pub(crate) mod recurrence;
pub(crate) mod serde_compat;

use self::google::GoogleCalendar;
use self::model::{
    AccountCalendar, Calendar, CalendarEvent, CalendarProviderKind, EventStatus, FreeBusySlot,
    StoredEvent, Visibility,
};
use self::outlook::OutlookCalendar;
use self::provider::{CalendarProvider, CalendarRemote, WindowEvents};
use self::recurrence::Recurrence;
use self::serde_compat::Compat;

/// A colleague's calendar as Exo last fetched it, for showing their day beside the user's own.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedGuest {
    /// When it was fetched.
    pub fetched_at: Timestamp,
    /// The window `events` cover.
    pub from: Timestamp,
    pub to: Timestamp,
    /// Their events in the window, as their calendar listed them.
    pub events: Vec<CalendarEvent>,
}

impl Store {
    /// What Exo last fetched of `email`'s calendar, if anything. The address is matched without
    /// regard to case.
    ///
    /// # Errors
    /// On database failures.
    pub async fn cached_guest(&self, email: &str) -> Result<Option<CachedGuest>> {
        let mut db = self.inner.db.clone();
        let Some(row) = GuestCacheRow::filter_by_person(email.to_lowercase())
            .first()
            .exec(&mut db)
            .await?
        else {
            return Ok(None);
        };
        // A time out of range can't be one Exo wrote, so the row counts as not there.
        let time = |ms| Timestamp::from_millisecond(ms).ok();
        Ok((|| {
            Some(CachedGuest {
                fetched_at: time(row.fetched_at)?,
                from: time(row.start_ms)?,
                to: time(row.to_ms)?,
                events: row.events.0.into_iter().map(|event| event.0).collect(),
            })
        })())
    }

    /// Keeps `cached` as `email`'s calendar, replacing what was there.
    ///
    /// # Errors
    /// On database failures.
    pub async fn cache_guest(&self, email: &str, cached: &CachedGuest) -> Result<()> {
        let person = email.to_lowercase();
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        GuestCacheRow::filter_by_person(&person)
            .delete()
            .exec(write.exec())
            .await?;
        GuestCacheRow::create()
            .person(person)
            .fetched_at(cached.fetched_at.as_millisecond())
            .start_ms(cached.from.as_millisecond())
            .to_ms(cached.to.as_millisecond())
            .events(Json(
                cached
                    .events
                    .iter()
                    .cloned()
                    .map(Compat)
                    .collect::<Vec<_>>(),
            ))
            .exec(write.exec())
            .await?;
        // The account's views don't show the cache, so this wakes none of them.
        write.commit().await
    }

    /// Forgets every cached calendar but those of `keep` (lower-case addresses). An empty `keep`
    /// forgets them all.
    ///
    /// # Errors
    /// On database failures.
    pub async fn forget_cached_guests(&self, keep: &[String]) -> Result<()> {
        let mut db = self.inner.db.clone();
        // Only the addresses: the events can be many, and this runs every minute.
        let gone: Vec<String> = GuestCacheRow::all()
            .select(GuestCacheRow::fields().person())
            .exec(&mut db)
            .await?
            .into_iter()
            .filter(|person| !keep.iter().any(|kept| kept.to_lowercase() == *person))
            .collect();
        if gone.is_empty() {
            return Ok(());
        }
        let mut write = self.inner.begin(&mut db).await?;
        for people in gone.chunks(500) {
            GuestCacheRow::filter(GuestCacheRow::fields().person().in_list(people))
                .delete()
                .exec(write.exec())
                .await?;
        }
        write.commit().await
    }

    /// The calendars on the lists of the account and of its signed-in calendar accounts, as last
    /// synced. Each shows once, from the account it's read through: the one whose primary
    /// calendar it is, else the one with the most access to it (owner, writer, reader), else the
    /// mail account, else the first by key. The mail account's calendars come first, then the
    /// other accounts' by key, each in the provider's order.
    ///
    /// # Errors
    /// On database failures.
    pub async fn calendars(&self) -> Result<Vec<AccountCalendar>> {
        let mut db = self.inner.db.clone();
        let rows = CalendarRow::all().exec(&mut db).await?;
        Ok(held(rows, &self.inner.account, |account| {
            self.reads(account)
        }))
    }

    /// Lists the calendars of `account` (canonical key: the mail account or a signed-in calendar
    /// account) from its provider and stores them in place of its last list.
    ///
    /// # Errors
    /// When the provider can't be reached or refuses, or on database failures.
    pub async fn sync_calendars(&self, account: &str) -> Result<Vec<Calendar>> {
        let provider = self.provider_for_account(account)?;
        let entries = provider.list_calendars().await?;
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        CalendarRow::filter(CalendarRow::fields().account().eq(account))
            .delete()
            .exec(write.exec())
            .await?;
        if !entries.is_empty() {
            let mut rows = CalendarRow::create_many();
            for (position, entry) in entries.iter().enumerate() {
                rows = rows.with_item(|row| {
                    row.account(account)
                        .id(&entry.id)
                        .position(u32::try_from(position).unwrap_or(u32::MAX))
                        .entry(Json(Compat(entry.clone())))
                });
            }
            rows.exec(write.exec()).await?;
        }
        write.publish().await?;
        Ok(entries)
    }

    /// Reads the calendars of secondary calendar account `account` (canonical key: `email` for
    /// Google, `"microsoft {email}"` for Microsoft) through `remote` from now on. Its list joins
    /// [`Store::calendars`], and calls for calendars read through `account` use `remote` without
    /// consuming primary mail quota or affecting primary mail outage state.
    pub fn add_calendar_account(&self, account: &str, remote: CalendarRemote) {
        let provider: Arc<dyn CalendarProvider> = match remote {
            CalendarRemote::Gmail {
                email,
                endpoints,
                tokens,
            } => Arc::new(GoogleCalendar::new(
                email,
                gmail::Client::new(endpoints, tokens),
                self.inner.backoff,
                self.inner.debounce,
                &self.inner.dir,
                None,
            )),
            CalendarRemote::Outlook {
                email,
                endpoints,
                tokens,
            } => Arc::new(OutlookCalendar::new(
                account.to_owned(),
                email,
                ::outlook::Client::new(endpoints, tokens),
                self.inner.backoff,
                self.inner.debounce,
                &self.inner.dir,
                None,
            )),
        };
        self.inner
            .calendar_accounts
            .write()
            .expect("calendar accounts are never poisoned")
            .insert(account.to_owned(), provider);
    }

    /// Stops reading calendar account `account`, as when it signs out. Its calendars leave
    /// [`Store::calendars`] until it's added again. With `forget`, for an account that was
    /// removed, its list goes too, along with the events of calendars that no other account lists.
    ///
    /// # Errors
    /// On database failures.
    pub async fn remove_calendar_account(&self, account: &str, forget: bool) -> Result<()> {
        self.inner
            .calendar_accounts
            .write()
            .expect("calendar accounts are never poisoned")
            .remove(account);
        if !forget {
            return Ok(());
        }
        let mut db = self.inner.db.clone();
        let rows = CalendarRow::all().exec(&mut db).await?;
        let (theirs, others): (Vec<CalendarRow>, Vec<CalendarRow>) =
            rows.into_iter().partition(|row| row.account == account);
        let orphans: Vec<String> = theirs
            .into_iter()
            .map(|row| row.id)
            .filter(|id| !others.iter().any(|row| &row.id == id))
            .collect();
        let mut write = self.inner.begin(&mut db).await?;
        CalendarRow::filter(CalendarRow::fields().account().eq(account))
            .delete()
            .exec(write.exec())
            .await?;
        for ids in orphans.chunks(IN_LIST_CHUNK) {
            EventRow::filter(EventRow::fields().calendar_id().in_list(ids))
                .delete()
                .exec(write.exec())
                .await?;
        }
        write.publish().await
    }

    /// The canonical account keys whose calendars are currently read: the mail account first,
    /// then signed-in secondary calendar accounts sorted by key.
    #[must_use]
    pub fn calendar_accounts(&self) -> Vec<String> {
        let accounts = self
            .inner
            .calendar_accounts
            .read()
            .expect("calendar accounts are never poisoned");
        let mut others: Vec<String> = accounts
            .keys()
            .filter(|key| *key != &self.inner.account)
            .cloned()
            .collect();
        others.sort();
        if accounts.contains_key(&self.inner.account) {
            std::iter::once(self.inner.account.clone())
                .chain(others)
                .collect()
        } else {
            others
        }
    }

    /// Which provider backs calendar account `account`, if registered.
    #[must_use]
    pub fn calendar_provider(&self, account: &str) -> Option<CalendarProviderKind> {
        self.inner
            .calendar_accounts
            .read()
            .expect("calendar accounts are never poisoned")
            .get(account)
            .map(|provider| provider.kind())
    }

    /// Whether `account`'s calendars are read now (registered in `calendar_accounts`).
    pub(crate) fn reads(&self, account: &str) -> bool {
        self.inner
            .calendar_accounts
            .read()
            .expect("calendar accounts are never poisoned")
            .contains_key(account)
    }

    /// The provider registered for `account`. Never silently falls back to the primary account
    /// when a different `account` is unsigned-in/unregistered.
    pub(crate) fn provider_for_account(&self, account: &str) -> Result<Arc<dyn CalendarProvider>> {
        let accounts = self
            .inner
            .calendar_accounts
            .read()
            .expect("calendar accounts are never poisoned");
        if let Some(provider) = accounts.get(account) {
            return Ok(provider.clone());
        }
        if account == self.inner.account {
            Err(Error::Unsupported("Calendar"))
        } else {
            Err(Error::Unauthorized(
                gmail::Error::Token("the calendar's account is signed out".into()).into(),
            ))
        }
    }

    /// When `calendars` has been cleared (for example right after Clear Cache and Restart, before
    /// secondary calendar accounts are registered and re-listed), consults persisted
    /// `settings.calendar_accounts` for a secondary owner of `calendar`. Returns `(account_key,
    /// email)` if `calendar` belongs to a secondary account rather than `self.inner.account`.
    async fn settings_secondary_owner(&self, calendar: &str) -> Result<Option<(String, String)>> {
        if calendar == "primary" {
            return Ok(None);
        }
        let settings: crate::settings::Settings = self.settings().await?;
        if settings
            .calendar_accounts
            .get(&self.inner.account)
            .is_some_and(|entry| entry.calendars.contains_key(calendar))
        {
            return Ok(None);
        }
        Ok(settings
            .calendar_accounts
            .iter()
            .filter(|(account, entry)| {
                account.as_str() != self.inner.account
                    && (entry.calendars.contains_key(calendar) || account.as_str() == calendar)
            })
            .min_by_key(|(account, _)| (!self.reads(account), account.as_str()))
            .map(|(account, entry)| (account.clone(), entry.email_for(account).to_owned())))
    }

    /// The provider that `calendar` is read through, by the rule in [`Store::calendars`].
    ///
    /// When `calendars` has been cleared (for example right after Clear Cache and Restart, before
    /// secondary calendar accounts are registered and re-listed), consults persisted
    /// `settings.calendar_accounts` before falling back to the primary account so queued edits for
    /// a secondary calendar wait with [`Error::Unauthorized`] instead of misrouting to primary.
    ///
    /// # Errors
    /// [`Error::Unauthorized`] for a calendar listed only by signed-out accounts (or accounts not
    /// added yet this session), so queued changes wait for them.
    pub(crate) async fn provider_for_calendar(
        &self,
        calendar: &str,
    ) -> Result<Arc<dyn CalendarProvider>> {
        let mut db = self.inner.db.clone();
        let rows = CalendarRow::filter(CalendarRow::fields().id().eq(calendar))
            .exec(&mut db)
            .await?;
        let listed = !rows.is_empty();
        match held(rows, &self.inner.account, |account| self.reads(account))
            .into_iter()
            .next()
        {
            Some(cal) => self.provider_for_account(&cal.account),
            None if listed => Err(Error::Unauthorized(
                gmail::Error::Token("the calendar's account is signed out".into()).into(),
            )),
            None => match self.settings_secondary_owner(calendar).await? {
                Some((owner, _)) => self.provider_for_account(&owner),
                None => self.provider_for_account(&self.inner.account),
            },
        }
    }

    /// Resolves `account`'s primary calendar ID from the local cache, if synced.
    pub(crate) async fn primary_calendar_id(&self, account: &str) -> Result<Option<String>> {
        let mut db = self.inner.db.clone();
        let rows = CalendarRow::filter(CalendarRow::fields().account().eq(account))
            .exec(&mut db)
            .await?;
        Ok(rows
            .into_iter()
            .find(|row| row.entry.0.0.primary)
            .map(|row| row.id))
    }

    /// Maps each calendar ID to the email address of the account that reads it.
    pub(crate) async fn calendar_emails(&self) -> Result<HashMap<String, String>> {
        let mut db = self.inner.db.clone();
        let rows = CalendarRow::all().exec(&mut db).await?;
        let active = held(rows, &self.inner.account, |account| self.reads(account));
        Ok(active
            .into_iter()
            .map(|cal| (cal.calendar.id, cal.email))
            .collect())
    }

    /// The display email address of the account reading `calendar`, falling back to persisted
    /// `settings.calendar_accounts` and finally the primary account's email.
    pub(crate) async fn email_for_calendar(&self, calendar: &str) -> Result<String> {
        let mut db = self.inner.db.clone();
        let rows = CalendarRow::filter(CalendarRow::fields().id().eq(calendar))
            .exec(&mut db)
            .await?;
        if let Some(cal) = held(rows, &self.inner.account, |account| self.reads(account))
            .into_iter()
            .next()
        {
            return Ok(cal.email);
        }
        if let Some((_, email)) = self.settings_secondary_owner(calendar).await? {
            return Ok(email);
        }
        Ok(CalendarProviderKind::parse_account_key(&self.inner.account)
            .1
            .to_owned())
    }

    /// Fetches `calendar`'s events on `days` (in `tz`) and updates the cache (replacing the window
    /// for full snapshots, or applying incremental changes for Outlook `calendarView/delta`).
    ///
    /// # Errors
    /// When the provider can't be reached or refuses, or on database failures.
    pub async fn sync_events(
        &self,
        calendar: &str,
        days: Range<Date>,
        tz: &TimeZone,
    ) -> Result<()> {
        let provider = self.provider_for_calendar(calendar).await?;
        let fetched = provider
            .sync_window(
                calendar,
                midnight(days.start, tz),
                midnight(days.end, tz),
                tz,
            )
            .await?;

        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        match fetched.events {
            WindowEvents::Replace(events) => {
                let rows: Vec<(String, (i64, i64, bool), CalendarEvent)> = events
                    .into_iter()
                    .filter(|event| event.status != EventStatus::Cancelled)
                    .filter_map(|event| Some((event.id.clone()?, span(&event)?, event)))
                    .collect();
                let mut stale: HashSet<String> =
                    overlapping(write.exec(), &[calendar.to_owned()], &days, tz)
                        .await?
                        .into_iter()
                        .map(|row| row.id)
                        .collect();
                stale.extend(rows.iter().map(|(id, ..)| id.clone()));
                let stale: Vec<String> = stale.into_iter().collect();
                for ids in stale.chunks(IN_LIST_CHUNK) {
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
                if !rows.is_empty() {
                    let mut create = EventRow::create_many();
                    for (id, (start, end, all_day), event) in rows {
                        let series_id = event.recurring_event_id.clone();
                        create = create.with_item(|row| {
                            row.calendar_id(calendar)
                                .id(id)
                                .start(start)
                                .end(end)
                                .all_day(all_day)
                                .series_id(series_id)
                                .event(Json(Compat(event)))
                        });
                    }
                    create.exec(write.exec()).await?;
                }
            }
            WindowEvents::Delta { updated, removed } => {
                for id in &removed {
                    delete_cached_event_in_window(write.exec(), calendar, id, &days, tz).await?;
                }
                for event in &updated {
                    if event.status == EventStatus::Cancelled {
                        if let Some(id) = event.id.as_deref() {
                            delete_cached_event_in_window(write.exec(), calendar, id, &days, tz)
                                .await?;
                        }
                    } else {
                        put(write.exec(), calendar, event).await?;
                    }
                }
            }
        }
        write.publish().await?;
        if let Some(commit) = fetched.on_commit {
            commit();
        }
        Ok(())
    }

    /// `calendar`'s events overlapping `from` to `to`, as the provider lists them now, without
    /// touching the cache. Passing `"primary"` resolves the primary account's default calendar on
    /// both Google and Outlook.
    ///
    /// # Errors
    /// When the provider can't be reached or refuses.
    pub async fn upcoming_events(
        &self,
        calendar: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<CalendarEvent>> {
        if calendar == "primary" {
            let resolved = self
                .primary_calendar_id(&self.inner.account)
                .await?
                .unwrap_or_else(|| "primary".to_owned());
            let provider = self.provider_for_account(&self.inner.account)?;
            return provider
                .list_events(&resolved, from, to, &TimeZone::UTC)
                .await;
        }
        let provider = self.provider_for_calendar(calendar).await?;
        provider
            .list_events(calendar, from, to, &TimeZone::UTC)
            .await
    }

    /// Busy slots for each calendar or address in `calendars` between `from` and `to`. Each gets
    /// its busy slots, or `None` when the provider won't say.
    ///
    /// # Errors
    /// When the provider can't be reached or refuses.
    pub async fn free_busy(
        &self,
        calendars: &[String],
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<(String, Option<Vec<FreeBusySlot>>)>> {
        let mut db = self.inner.db.clone();
        let rows = CalendarRow::all().exec(&mut db).await?;
        let holders: BTreeMap<String, String> =
            held(rows, &self.inner.account, |account| self.reads(account))
                .into_iter()
                .map(|cal| (cal.calendar.id, cal.account))
                .collect();
        let mut asked: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for id in calendars {
            asked
                .entry(
                    holders
                        .get(id)
                        .map_or(self.inner.account.as_str(), String::as_str),
                )
                .or_default()
                .push(id.clone());
        }
        let mut busy: HashMap<String, Option<Vec<FreeBusySlot>>> = HashMap::new();
        for (account, ids) in asked {
            let provider = self.provider_for_account(account)?;
            for (id, slots) in provider.free_busy(&ids, from, to).await? {
                busy.insert(id, slots);
            }
        }
        Ok(calendars
            .iter()
            .map(|id| (id.clone(), busy.remove(id).flatten()))
            .collect())
    }

    /// Person `email`'s events from `from` to `to`, asked through the calendar account that read
    /// their calendar last, else through every active calendar account at once, preferring the
    /// first answer with detailed/public events. Returns `None` when no account has visibility.
    ///
    /// # Errors
    /// When a provider can't be reached or refuses with an unexpected error.
    pub async fn attendee_events(
        &self,
        email: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Option<Vec<CalendarEvent>>> {
        let accounts = self.calendar_accounts();
        if accounts.is_empty() {
            return Err(Error::Unsupported("Calendar"));
        }
        let query = |account: String| async move {
            let provider = match self.provider_for_account(&account) {
                Ok(provider) => provider,
                Err(error) => return (account, Err(error)),
            };
            let result = provider.attendee_events(email, from, to).await;
            (account, result)
        };
        let previous = self
            .inner
            .attendee_accounts
            .read()
            .expect("attendee accounts are never poisoned")
            .get(email)
            .cloned();
        let mut answers = Vec::new();
        if let Some(account) = previous.filter(|account| accounts.contains(account)) {
            answers.push(query(account).await);
        }
        if !answers.iter().any(|(_, res)| matches!(res, Ok(Some(_)))) {
            answers = futures::future::join_all(accounts.into_iter().map(query)).await;
        }
        let detailed = |events: &Vec<CalendarEvent>| {
            events
                .iter()
                .any(|event| event.visibility == Visibility::Public || event.summary.is_some())
        };
        let found = answers
            .iter()
            .position(|(_, res)| matches!(res, Ok(Some(events)) if detailed(events)))
            .or_else(|| {
                answers
                    .iter()
                    .position(|(_, res)| matches!(res, Ok(Some(_))))
            });
        let found = found.map(|index| answers.swap_remove(index));
        {
            let mut remembered = self
                .inner
                .attendee_accounts
                .write()
                .expect("attendee accounts are never poisoned");
            match &found {
                Some((account, _)) => remembered.insert(email.to_owned(), account.clone()),
                None => remembered.remove(email),
            };
        }
        if let Some((_, Ok(events))) = found {
            return Ok(events);
        }
        if let Some((_, Err(error))) = answers.into_iter().find(|(_, res)| res.is_err()) {
            return Err(error);
        }
        Ok(None)
    }

    /// The recurrence rule of series master `id` on `calendar`, when representable.
    ///
    /// # Errors
    /// When the provider can't be reached or refuses.
    pub async fn series_recurrence(&self, calendar: &str, id: &str) -> Result<Option<Recurrence>> {
        let provider = self.provider_for_calendar(calendar).await?;
        provider.series_recurrence(calendar, id).await
    }

    /// The stored events of `calendars` on `days` (in `tz`), with queued local changes overlaid,
    /// ordered by start.
    ///
    /// # Errors
    /// On database failures.
    pub async fn events(
        &self,
        calendars: &[String],
        days: Range<Date>,
        tz: &TimeZone,
    ) -> Result<Vec<StoredEvent>> {
        if calendars.is_empty() {
            return Ok(Vec::new());
        }
        self.inner.read_calendar(&days, tz);
        let mut db = self.inner.db.clone();
        let rows = overlapping(&mut db, calendars, &days, tz).await?;
        let mut events: Vec<StoredEvent> = rows
            .into_iter()
            .map(|row| StoredEvent {
                calendar_id: row.calendar_id,
                event: row.event.0.0,
                unsynced: false,
            })
            .collect();
        let emails = self.calendar_emails().await?;
        let primary_email = CalendarProviderKind::parse_account_key(&self.inner.account).1;
        let me_for = |calendar: &str| emails.get(calendar).map_or(primary_email, String::as_str);
        let shown = |calendar: &str, event: &CalendarEvent| {
            calendars.iter().any(|id| id == calendar)
                && span(event).is_some_and(|span| overlaps(span, &days, tz))
        };
        let window = (midnight(days.start, tz), midnight(days.end, tz));
        for change in outbox::queued(&mut db).await? {
            change.apply(&mut events, &shown, Some(window), &me_for);
        }
        events.sort_by_cached_key(|stored| {
            (
                span(&stored.event).map_or(i64::MAX, |(start, ..)| start),
                stored.calendar_id.clone(),
                stored.event.id.clone(),
            )
        });
        Ok(events)
    }
}

impl crate::store::Inner {
    /// Remembers that the calendar read `days` (in `tz`) this session, so eviction keeps the
    /// events of those days.
    fn read_calendar(&self, days: &Range<Date>, tz: &TimeZone) {
        let span = (
            midnight(days.start, tz)
                .as_millisecond()
                .min(utc_midnight(days.start)),
            midnight(days.end, tz)
                .as_millisecond()
                .max(utc_midnight(days.end)),
        );
        let mut read = self
            .calendar_read
            .lock()
            .expect("calendar-read lock is never poisoned");
        if !read
            .iter()
            .any(|(from, to)| *from <= span.0 && span.1 <= *to)
        {
            read.push(span);
        }
    }
}

/// `rows`, the calendar accounts' lists, with each calendar once by the rule in
/// [`Store::calendars`], among the accounts read now (`reads`). The mail account's (`mail`) come
/// first, then the others' by key, each in its list's order.
fn held(rows: Vec<CalendarRow>, mail: &str, reads: impl Fn(&str) -> bool) -> Vec<AccountCalendar> {
    let access = |cal: &Calendar| {
        if cal.primary {
            0
        } else {
            cal.access_role.rank()
        }
    };
    let mut rows: Vec<CalendarRow> = rows.into_iter().filter(|row| reads(&row.account)).collect();
    rows.sort_by(|a, b| {
        (access(&a.entry.0.0), a.account != mail, &a.account).cmp(&(
            access(&b.entry.0.0),
            b.account != mail,
            &b.account,
        ))
    });
    let mut seen = HashSet::new();
    let mut kept: Vec<CalendarRow> = rows
        .into_iter()
        .filter(|row| seen.insert(row.id.clone()))
        .collect();
    kept.sort_by(|a, b| {
        (a.account != mail, &a.account, a.position).cmp(&(
            b.account != mail,
            &b.account,
            b.position,
        ))
    });
    kept.into_iter()
        .map(|row| {
            let (provider, email) = CalendarProviderKind::parse_account_key(&row.account);
            AccountCalendar {
                email: email.to_owned(),
                provider,
                account: row.account,
                calendar: row.entry.0.0,
            }
        })
        .collect()
}

/// Writes `event` (which has an id) over the cache's copy on `calendar`.
pub(crate) async fn put(
    exec: &mut dyn Executor,
    calendar: &str,
    event: &CalendarEvent,
) -> Result<()> {
    let (Some(id), Some((start, end, all_day))) = (event.id.clone(), span(event)) else {
        return Ok(());
    };
    EventRow::filter(
        EventRow::fields()
            .calendar_id()
            .eq(calendar)
            .and(EventRow::fields().id().eq(&id)),
    )
    .delete()
    .exec(&mut *exec)
    .await?;
    EventRow::create()
        .calendar_id(calendar)
        .id(id)
        .start(start)
        .end(end)
        .all_day(all_day)
        .series_id(event.recurring_event_id.clone())
        .event(Json(Compat(event.clone())))
        .exec(exec)
        .await?;
    Ok(())
}

/// Deletes event `id` and any cached recurring instances whose `series_id == id` on `calendar`,
/// using the primary key and `(calendar_id, series_id)` index without scanning the full calendar.
pub(crate) async fn delete_cached_event(
    exec: &mut dyn Executor,
    calendar: &str,
    id: &str,
) -> Result<()> {
    let fields = EventRow::fields();
    EventRow::filter(fields.calendar_id().eq(calendar).and(fields.id().eq(id)))
        .delete()
        .exec(&mut *exec)
        .await?;
    EventRow::filter(
        fields
            .calendar_id()
            .eq(calendar)
            .and(fields.series_id().eq(id)),
    )
    .delete()
    .exec(exec)
    .await?;
    Ok(())
}

/// Deletes event `id` and any cached recurring instances whose `series_id == id` on `calendar`,
/// scoped to rows overlapping `days` in `tz`. Used by `WindowEvents::Delta` so `@removed` items
/// emitted for events that moved outside the synced window never wipe events cached in other
/// windows.
async fn delete_cached_event_in_window(
    exec: &mut dyn Executor,
    calendar: &str,
    id: &str,
    days: &Range<Date>,
    tz: &TimeZone,
) -> Result<()> {
    let fields = EventRow::fields();
    let (from, to) = (
        midnight(days.start, tz).as_millisecond(),
        midnight(days.end, tz).as_millisecond(),
    );
    let (first, last) = (utc_midnight(days.start), utc_midnight(days.end));
    let timed = || {
        fields
            .all_day()
            .eq(false)
            .and(fields.start().lt(to))
            .and(fields.end().gt(from))
    };
    let all_day = || {
        fields
            .all_day()
            .eq(true)
            .and(fields.start().lt(last))
            .and(fields.end().gt(first))
    };
    EventRow::filter(
        fields
            .calendar_id()
            .eq(calendar)
            .and(fields.id().eq(id))
            .and(timed().or(all_day())),
    )
    .delete()
    .exec(&mut *exec)
    .await?;
    EventRow::filter(
        fields
            .calendar_id()
            .eq(calendar)
            .and(fields.series_id().eq(id))
            .and(timed().or(all_day())),
    )
    .delete()
    .exec(exec)
    .await?;
    Ok(())
}

/// Rows of `calendars` overlapping `days`: timed events by instant in `tz`, all-day ones by
/// date.
async fn overlapping(
    exec: &mut dyn Executor,
    calendars: &[String],
    days: &Range<Date>,
    tz: &TimeZone,
) -> Result<Vec<EventRow>> {
    let fields = EventRow::fields();
    let (from, to) = (
        midnight(days.start, tz).as_millisecond(),
        midnight(days.end, tz).as_millisecond(),
    );
    let (first, last) = (utc_midnight(days.start), utc_midnight(days.end));
    let timed = fields
        .all_day()
        .eq(false)
        .and(fields.start().lt(to))
        .and(fields.end().gt(from));
    let all_day = fields
        .all_day()
        .eq(true)
        .and(fields.start().lt(last))
        .and(fields.end().gt(first));
    Ok(EventRow::filter(
        fields
            .calendar_id()
            .in_list(calendars)
            .and(timed.or(all_day)),
    )
    .exec(exec)
    .await?)
}

/// Whether an event spanning `span` ([`span`]) falls on `days` in `tz`, as [`overlapping`]
/// finds rows.
fn overlaps((start, end, all_day): (i64, i64, bool), days: &Range<Date>, tz: &TimeZone) -> bool {
    let (from, to) = if all_day {
        (utc_midnight(days.start), utc_midnight(days.end))
    } else {
        (
            midnight(days.start, tz).as_millisecond(),
            midnight(days.end, tz).as_millisecond(),
        )
    };
    start < to && end > from
}

/// An event's stored span in epoch milliseconds, and whether it's all-day.
pub(crate) fn span(event: &CalendarEvent) -> Option<(i64, i64, bool)> {
    let (start, end) = (event.start.as_ref()?, event.end.as_ref()?);
    let (start_ms, start_all_day) = start.stored_millis();
    let (end_ms, end_all_day) = end.stored_millis();
    Some((start_ms, end_ms, start_all_day && end_all_day))
}

/// The start of `day` in `tz`.
fn midnight(day: Date, tz: &TimeZone) -> Timestamp {
    day.to_zoned(tz.clone()).map_or_else(
        |_| {
            day.to_zoned(TimeZone::UTC)
                .map(|zoned| zoned.timestamp())
                .unwrap_or_default()
        },
        |zoned| zoned.timestamp(),
    )
}

/// `day`'s UTC midnight in milliseconds, which is how an all-day date is stored.
fn utc_midnight(day: Date) -> i64 {
    day.to_zoned(TimeZone::UTC)
        .map(|zoned| zoned.timestamp().as_millisecond())
        .unwrap_or_default()
}
