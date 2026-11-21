//! Bodies. Backfill, history, search and the contact pane fetch threads for their rows
//! ([`fetch_rows`](super::fetch_rows)), and a message's body, attachments and every header come
//! after. The threads the app shows first (the first [`ON_SCREEN_AHEAD`] of the list on screen
//! and of every inbox list) get them in the background, a page each sync round
//! ([`Lane::Sync`]). The thread the list's cursor rests on gets them before it's opened
//! ([`Store::prefetch`], [`Lane::Show`]). The rest that eviction would keep get them a hundred
//! at a time ([`Store::prerender`], [`Lane::Sync`]). Any other thread gets them when it's
//! opened ([`Store::load_bodies`], [`Lane::Act`]).

use std::collections::HashSet;

use futures::{StreamExt, stream};
use toasty::Executor;
use toasty::stmt::Value;

use super::backfill::ON_SCREEN_AHEAD;
use super::{ATTEMPTS, fetch_message, fetch_thread};
use crate::api::{Api, Failure};
use crate::db::{self, BodyRow, DraftRow, EVICTED_BEFORE, MessageRow, query_i64};
use crate::error::{Error, Result};
use crate::evict::Eviction;
use crate::ids::ThreadId;
use crate::ids::labels::{INBOX, SH_IMPORTANT, SH_OTHER};
use crate::provider;
use crate::quota::Lane;
use crate::store::{Inner, Store, reindex};

/// Threads a sync round fetches the bodies of.
const PAGE: usize = 10;
/// Threads fetched at once.
const CONCURRENCY: usize = 5;
/// Tries for the bodies of an opened thread and of the cursor's thread, because someone is
/// waiting for them.
const OPEN_ATTEMPTS: u32 = 2;
/// The conversations with a message stored without its body ([`bodiless`]) whose newest
/// message is newer than `?1` (epoch milliseconds), newest first, `?2` of them.
const UNRENDERED: &str = "SELECT thread_id FROM messages WHERE thread_id IN (SELECT m.thread_id FROM messages m WHERE NOT EXISTS (SELECT 1 FROM bodies b WHERE b.message_id = m.id) AND NOT EXISTS (SELECT 1 FROM drafts d WHERE d.message_id = m.id AND d.failure IS NULL AND (d.send_at IS NULL OR json_extract(d.record, '$.send_later') IS NULL))) GROUP BY thread_id HAVING max(internal_date) > ?1 ORDER BY max(internal_date) DESC LIMIT ?2";
/// The bytes of HTML and text the store's bodies take, as eviction counts them.
const BODY_BYTES: &str = "SELECT coalesce(sum(coalesce(octet_length(html), 0) + coalesce(octet_length(text), 0)), 0) FROM bodies";
/// Of the newest `?2` threads of list `?1`, those with a message stored without its body
/// ([`bodiless`]), newest first.
const WANTING: &str = "SELECT top.thread_id FROM (SELECT thread_id, sort FROM list_ids WHERE list_id = ?1 ORDER BY sort DESC LIMIT ?2) AS top WHERE EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = top.thread_id AND NOT EXISTS (SELECT 1 FROM bodies b WHERE b.message_id = m.id) AND NOT EXISTS (SELECT 1 FROM drafts d WHERE d.message_id = m.id AND d.failure IS NULL AND (d.send_at IS NULL OR json_extract(d.record, '$.send_later') IS NULL))) ORDER BY top.sort DESC";

impl Inner {
    /// Fetches the bodies of [`PAGE`] of the threads the app shows first that lack some,
    /// starting with the list on screen. Returns whether more remain. The caller holds the sync
    /// lock.
    pub(super) async fn body_page(&self) -> Result<bool> {
        let mut db = self.db.clone();
        let skipped = self
            .skipped_bodies
            .lock()
            .expect("skipped-bodies lock is never poisoned")
            .clone();
        let mut wanted: Vec<String> = Vec::new();
        for (list, depth) in self.prefetched_lists() {
            let rows = toasty::sql::query(WANTING)
                .bind(list)
                .bind(i64::from(depth))
                .exec(&mut db)
                .await?;
            for row in &rows {
                if let Value::Record(row) = row
                    && let Some(Value::String(thread)) = row.fields.first()
                    && !skipped.contains(thread)
                    && !wanted.contains(thread)
                {
                    wanted.push(thread.clone());
                }
            }
        }
        let more = wanted.len() > PAGE;
        wanted.truncate(PAGE);
        let mut missing = Vec::new();
        for thread in wanted {
            let messages = bodiless(&mut db, &thread).await?;
            missing.push((thread, messages));
        }
        let api = &self.api;
        let results = stream::iter(missing)
            .map(|(thread, messages)| async move {
                let fetched = fetch_bodies(api, Lane::Sync, ATTEMPTS, &thread, &messages).await;
                (thread, fetched)
            })
            .buffer_unordered(CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let router = self.router();
        let mut write = self.begin(&mut db).await?;
        let mut tried = Vec::new();
        let mut halted = None;
        for (thread, fetched) in results {
            match fetched {
                Ok(messages) => {
                    provider::save_bodies(write.exec(), &messages).await?;
                    reindex(write.exec(), &thread, &router).await?;
                }
                // Retrying soon won't fix this, so the thread waits to be opened.
                Err(failure @ (Failure::Transient(_) | Failure::Permanent(_))) => {
                    log::warn!(
                        "fetching the bodies of thread {thread}: {}",
                        failure.message()
                    );
                }
                // Offline, throttled or signed out. The rest of the round would be too.
                Err(failure) => {
                    halted = Some(failure);
                    continue;
                }
            }
            tried.push(thread);
        }
        // A thread still missing some bodies (because a message is gone since, say) waits to be
        // opened rather than being fetched again every round.
        let mut stuck = Vec::new();
        for thread in tried {
            if !bodiless(write.exec(), &thread).await?.is_empty() {
                stuck.push(thread);
            }
        }
        write.publish().await?;
        self.skipped_bodies
            .lock()
            .expect("skipped-bodies lock is never poisoned")
            .extend(stuck);
        match halted {
            Some(failure) => Err(failure.into()),
            None => Ok(more),
        }
    }

    /// The lists whose first threads get their bodies ahead of being opened, and how many: the
    /// one on screen, as far as it has loaded rows, then the first [`ON_SCREEN_AHEAD`] of the
    /// inbox and its tabs (Important, Other and every split).
    pub(crate) fn prefetched_lists(&self) -> Vec<(String, u32)> {
        let on_screen = self
            .on_screen
            .read()
            .expect("on-screen lock is never poisoned")
            .clone();
        let loaded = self
            .loaded_rows
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(ON_SCREEN_AHEAD);
        let splits = self.router().splits;
        let mut lists: Vec<(String, u32)> = on_screen
            .iter()
            .map(|list| (list.key().to_owned(), loaded))
            .collect();
        for list in [INBOX, SH_IMPORTANT, SH_OTHER]
            .into_iter()
            .chain(splits.routed_ids())
        {
            if !lists.iter().any(|(known, _)| known == list) {
                lists.push((list.to_owned(), ON_SCREEN_AHEAD));
            }
        }
        lists
    }
}

impl Store {
    /// Whether every message of `thread` has its body stored, as the thread view shows it.
    ///
    /// # Errors
    /// On database failures.
    pub async fn bodies_stored(&self, thread: &ThreadId) -> Result<bool> {
        Ok(self.bodiless(thread).await?.is_empty())
    }

    /// The messages of `thread` stored without their body (for their rows alone), which the
    /// thread view shows placeholders for until they're in.
    ///
    /// # Errors
    /// On database failures.
    pub async fn bodiless(&self, thread: &ThreadId) -> Result<Vec<String>> {
        let mut db = self.inner.db.clone();
        bodiless(&mut db, thread.as_str()).await
    }

    /// Fetches the bodies `thread` lacks because it was stored for its rows alone, so it can be
    /// opened. The calls go ahead of everything but other user actions. Does nothing when the
    /// bodies are stored.
    ///
    /// # Errors
    /// [`Error::Offline`](crate::Error::Offline) and the like when Gmail can't be reached.
    pub async fn load_bodies(&self, thread: &ThreadId) -> Result<()> {
        self.inner
            .fetch_missing(thread.as_str(), Lane::Act, true)
            .await
    }

    /// [`load_bodies`](Self::load_bodies) for the thread the list's cursor rests on, ahead of
    /// its opening. Its calls go after the user's own, along with what's on screen.
    ///
    /// # Errors
    /// As [`load_bodies`](Self::load_bodies).
    pub async fn prefetch(&self, thread: &ThreadId) -> Result<()> {
        self.inner
            .fetch_missing(thread.as_str(), Lane::Show, true)
            .await
    }

    /// Fetches the bodies of up to `limit` conversations stored for their rows alone, one at a
    /// time, newest first, and only those `eviction` would keep. It skips conversations quieter
    /// than the eviction's age or than the conversations it let go of, and stops once the bodies
    /// take nine tenths of its budget, where eviction stops. Returns how many it fetched.
    ///
    /// # Errors
    /// [`Error::Offline`](crate::Error::Offline) and the like when Gmail can't be reached, and on
    /// database failures.
    pub async fn prerender(&self, eviction: &Eviction, limit: usize) -> Result<usize> {
        let inner = &self.inner;
        inner.bootstrapped().await?;
        let mut db = inner.db.clone();
        let target = i64::try_from(eviction.budget / 10 * 9).unwrap_or(i64::MAX);
        let since = (jiff::Timestamp::now() - eviction.age).as_millisecond();
        let since = since.max(
            db::kv_get::<i64>(&mut db, EVICTED_BEFORE)
                .await?
                .unwrap_or(i64::MIN),
        );
        let rows = toasty::sql::query(UNRENDERED)
            .bind(since)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .exec(&mut db)
            .await?;
        let skipped = inner
            .skipped_bodies
            .lock()
            .expect("skipped-bodies lock is never poisoned")
            .clone();
        let threads: Vec<String> = rows
            .into_iter()
            .filter_map(|row| match row {
                Value::Record(row) => match row.fields.into_iter().next() {
                    Some(Value::String(thread)) => Some(thread),
                    _ => None,
                },
                _ => None,
            })
            .filter(|thread| !skipped.contains(thread))
            .collect();
        let mut fetched = 0;
        for thread in threads {
            if query_i64(&mut db, BODY_BYTES).await? >= target {
                break;
            }
            let _sync = inner.sync.lock().await;
            // Nothing on screen changes, except for the open conversation.
            let open = inner.reading.lock().await.as_deref() == Some(thread.as_str());
            match inner.fetch_missing(&thread, Lane::Sync, open).await {
                Ok(()) => fetched += 1,
                // Gmail wouldn't give it, so it waits to be opened.
                Err(error @ Error::Gmail(_)) => {
                    log::warn!("pre-rendering thread {thread}: {error}");
                    inner
                        .skipped_bodies
                        .lock()
                        .expect("skipped-bodies lock is never poisoned")
                        .insert(thread);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(fetched)
    }
}

impl Inner {
    /// Fetches the bodies `thread` lacks in `lane`, and tells the store's subscribers when
    /// `notify` is set. Either the store must have bootstrapped, or the caller must not hold the
    /// sync lock.
    async fn fetch_missing(&self, thread: &str, lane: Lane, notify: bool) -> Result<()> {
        self.bootstrapped().await?;
        let mut db = self.db.clone();
        let missing = bodiless(&mut db, thread).await?;
        if missing.is_empty() {
            return Ok(());
        }
        let attempts = if lane == Lane::Sync {
            ATTEMPTS
        } else {
            OPEN_ATTEMPTS
        };
        let messages = fetch_bodies(&self.api, lane, attempts, thread, &missing).await?;
        let mut write = self.begin(&mut db).await?;
        provider::save_bodies(write.exec(), &messages).await?;
        reindex(write.exec(), thread, &self.router()).await?;
        if notify {
            write.publish().await
        } else {
            write.commit().await
        }
    }
}

/// The messages of `thread` stored without their body, except for Exo's own drafts. Compose
/// shows those from their record, so saving a draft stores only its message's row. A failed or
/// scheduled draft's message counts, because the thread shows it as a message.
async fn bodiless(exec: &mut dyn Executor, thread: &str) -> Result<Vec<String>> {
    let ids: Vec<String> = MessageRow::filter_by_thread_id(thread)
        .select(MessageRow::fields().id())
        .exec(exec)
        .await?;
    let stored: HashSet<String> =
        BodyRow::filter(BodyRow::fields().message_id().in_list(ids.as_slice()))
            .select(BodyRow::fields().message_id())
            .exec(exec)
            .await?
            .into_iter()
            .collect();
    let drafts: HashSet<String> = DraftRow::all()
        .exec(exec)
        .await?
        .into_iter()
        .filter(|draft| draft.failure.is_none() && crate::drafts::scheduled(draft).is_none())
        .filter_map(|draft| draft.message_id)
        .collect();
    Ok(ids
        .into_iter()
        .filter(|id| !stored.contains(id) && !drafts.contains(id))
        .collect())
}

/// Fetches the `missing` bodies of `thread` the cheapest way: the one message alone
/// (`messages.get`, half a thread's units), or else the whole thread. Returns nothing when the
/// thread is gone.
async fn fetch_bodies(
    api: &Api,
    lane: Lane,
    attempts: u32,
    thread: &str,
    missing: &[String],
) -> Result<Vec<gmail::Message>, Failure> {
    Ok(match missing {
        [message] => fetch_message(api, lane, attempts, message)
            .await?
            .into_iter()
            .collect(),
        _ => fetch_thread(api, lane, attempts, thread)
            .await?
            .map(|thread| thread.messages)
            .unwrap_or_default(),
    })
}
