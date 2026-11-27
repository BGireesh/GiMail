//! Sync: snapshot the history cursor before anything else, refresh labels and identity, apply
//! Gmail history incrementally, and backfill lists page by page with the coverage rule. Expired
//! history triggers a soft bankruptcy that keeps the outbox.

mod arrivals;
mod backfill;
mod bodies;
mod history;

pub use arrivals::Arrival;
pub(crate) use backfill::LIST_CAP;
pub use backfill::RECENT_THREADS;
#[cfg(test)]
pub(crate) use backfill::WARMED;

use std::sync::atomic::Ordering;

use tokio::sync::MutexGuard;

use crate::db::{
    self, ALIASES, AttachmentRow, BodyRow, EVICTED_BEFORE, IDENTITY, KvRow, LabelRow, ListCursor,
    ListEntry, MessageRow, ParticipantEntry, TextDue, ThreadRow,
};
use crate::error::Result;
use crate::ids::ListId;
use crate::model::{Alias, LabelKind};
use crate::provider::{Account, Cursors, Labels, Markers};
use crate::store::{Inner, Store, reindex_many};
use crate::view::{Identity, Router};

/// Tries per sync call before giving up.
pub(crate) const ATTEMPTS: u32 = 5;
/// The headers a message's row reads (for lists, splits, the search index, the contact pane and
/// replies), which are all that fetching a row asks for. `Content-Type` stands in for the
/// top-level MIME type if Gmail leaves `mimeType` out, and `Content-Class` marks Exchange's
/// invites.
pub(crate) const ROW_HEADERS: [&str; 16] = [
    "From",
    "To",
    "Cc",
    "Bcc",
    "Reply-To",
    "Subject",
    "Message-ID",
    "In-Reply-To",
    "References",
    "List-Id",
    "List-Unsubscribe",
    "List-Unsubscribe-Post",
    "Delivered-To",
    "Content-Type",
    "Content-Class",
    // What Gmail found of the sender's DMARC, which a brand's logo needs.
    "Authentication-Results",
];
/// How far Gmail's history is applied this session, as the footer shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncProgress {
    /// Whether a round of history ended this session, however it ended.
    pub caught_up: bool,
    /// While a round applies history, how many changes to messages it has, and how many of them
    /// it has applied.
    pub changes: Option<(usize, usize)>,
}

/// What one [`Store::sync`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// How many threads Gmail history changed.
    pub changed: usize,
    /// Whether the background backfill isn't finished, so `sync` should be called again soon (on
    /// an idle tick).
    pub backfilling: bool,
    /// Whether history had expired, so the cache was rebuilt from scratch. Queued mutations were
    /// kept.
    pub rebuilt: bool,
    /// The messages Gmail history brought that the store didn't have, oldest first, which is the
    /// new mail. A rebuild or a backfill brings none.
    pub arrived: Vec<Arrival>,
}

impl Store {
    /// One round of sync: first-run setup, Gmail history since the last round, then one page
    /// of each background backfill (All Mail for 31 days; Inbox, Important and Other up to
    /// 1250 threads each). The sync lock is let go between pages, so a list opened meanwhile
    /// ([`backfill`](Self::backfill)) waits for one page at most.
    ///
    /// # Errors
    /// [`Error::Offline`](crate::Error::Offline), [`Error::Throttled`](crate::Error::Throttled)
    /// and the like when Gmail can't be reached or refuses. Local data stays as it was.
    pub async fn sync(&self) -> Result<SyncReport> {
        let inner = &self.inner;
        let mut report = SyncReport::default();
        {
            let _sync = inner.lock_sync().await?;
            match inner.sync_history().await? {
                Some(applied) => {
                    report.changed = applied.changed;
                    report.arrived = inner.arrivals(&applied.arrived).await?;
                }
                None => {
                    inner.bootstrapped.store(false, Ordering::Release);
                    inner.bankrupt().await?;
                    inner.bootstrap().await?;
                    inner.bootstrapped.store(true, Ordering::Release);
                    report.rebuilt = true;
                }
            }
        }
        report.backfilling = inner.backfill_round().await?;
        // Threads with notes are kept whole, so their notes are found with their mail. One the
        // store has none of (after Clear Cache, or notes brought back from their backup) is
        // fetched, a few a round. A failure here is the next round's to try again.
        match self.fetch_noted_threads().await {
            Ok(asked) => report.backfilling |= asked > 0,
            Err(error) => log::warn!("fetching the threads with notes: {error}"),
        }
        Ok(report)
    }

    /// How far Gmail's history is applied, as it changes.
    #[must_use]
    pub fn sync_progress(&self) -> tokio::sync::watch::Receiver<SyncProgress> {
        self.inner.progress.subscribe()
    }

    /// Says which list is on screen. When it's one of the background passes (Inbox, Important,
    /// Other or a split), each round backfills it first, from a screenful up to the app's first
    /// 100 threads, so after a rebuild the list in front of the user fills before the others.
    pub fn show(&self, list: &ListId) {
        *self
            .inner
            .on_screen
            .write()
            .expect("on-screen lock is never poisoned") = Some(list.clone());
    }

    /// Says how many rows of the list on screen the app has loaded. Sync fetches the bodies of
    /// all those rows, and eviction keeps them.
    pub fn loaded(&self, rows: usize) {
        self.inner.loaded_rows.store(
            u32::try_from(rows).unwrap_or(u32::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Backfills `list` further back by at least `threads` threads (when Gmail has them), for
    /// opening a list or scrolling past its end. Returns whether Gmail has more.
    ///
    /// # Errors
    /// As [`sync`](Self::sync).
    pub async fn backfill(&self, list: &ListId, threads: usize) -> Result<bool> {
        let inner = &self.inner;
        let mut listed = 0;
        loop {
            let _sync = inner.lock_sync().await?;
            let page = inner
                .backfill_page(list, backfill::Limit::None, u32::MAX)
                .await?;
            listed += page.listed;
            if !page.more || listed >= threads {
                return Ok(page.more);
            }
        }
    }
}

impl Inner {
    /// Takes the sync lock, bootstrapping first if this store hasn't yet.
    pub(crate) async fn lock_sync(&self) -> Result<MutexGuard<'_, ()>> {
        let sync = self.sync.lock().await;
        if !self.bootstrapped.load(Ordering::Acquire) {
            self.bootstrap().await?;
            self.bootstrapped.store(true, Ordering::Release);
        }
        Ok(sync)
    }

    /// Waits until this store has bootstrapped (labels and identity, which saving a thread
    /// needs), without waiting for a sync that's running.
    pub(crate) async fn bootstrapped(&self) -> Result<()> {
        if !self.bootstrapped.load(Ordering::Acquire) {
            drop(self.lock_sync().await?);
        }
        Ok(())
    }

    /// Snapshots the cursors (once per database), then refreshes labels and identity.
    async fn bootstrap(&self) -> Result<()> {
        let mut db = self.db.clone();
        if Cursors::load(&mut db).await?.is_empty() {
            let cursors = self.mail.start().await?;
            let mut write = self.begin(&mut db).await?;
            cursors.save(write.exec()).await?;
            // In a new store, threads another store snoozed may be waiting with no reminder here.
            db::kv_set(write.exec(), db::ORPHANED_SNOOZES, &true).await?;
            write.commit().await?;
        }
        let markers = self.refresh_labels().await?;
        // The splits' label names resolve against the labels just listed.
        self.reload_splits(false).await?;
        self.refresh_identity(markers).await
    }

    /// Re-lists labels, the provider making Exo's markers first if they're missing. Returns the
    /// markers' keys.
    async fn refresh_labels(&self) -> Result<Markers> {
        let Labels { labels, markers } = self.mail.labels().await?;
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        LabelRow::all().delete().exec(write.exec()).await?;
        let mut rows = LabelRow::create_many();
        for label in &labels {
            rows = rows.with_item(|row| {
                row.id(&label.id)
                    .name(&label.name)
                    .system(label.kind == LabelKind::System)
                    .hidden(label.hidden)
                    .text_color(label.text_color.clone())
                    .background_color(label.background_color.clone())
            });
        }
        rows.exec(write.exec()).await?;
        write.publish().await?;
        Ok(markers)
    }

    /// Learns which addresses are "me" and the keys of Exo's markers. A change re-routes every
    /// stored thread.
    async fn refresh_identity(&self, markers: Markers) -> Result<()> {
        let Account {
            addresses,
            name,
            aliases,
        } = self.mail.account().await?;
        self.save_aliases(aliases).await?;
        let identity = Identity {
            addresses,
            name,
            muted_label: markers.muted,
            snoozed_label: markers.snoozed,
            reminder_unread_label: markers.reminder_unread,
        };
        if identity == self.me() {
            return Ok(());
        }
        let mut db = self.db.clone();
        let threads: Vec<String> = ThreadRow::all()
            .select(ThreadRow::fields().id())
            .exec(&mut db)
            .await?;
        let mut write = self.begin(&mut db).await?;
        db::kv_set(write.exec(), IDENTITY, &identity).await?;
        let router = Router {
            me: identity.clone(),
            ..self.router()
        };
        reindex_many(write.exec(), &threads, &router).await?;
        // Only once it's stored, so a failure above leaves the old identity and the next
        // bootstrap tries again.
        write.commit().await?;
        *self
            .identity
            .write()
            .expect("identity lock is never poisoned") = identity;
        self.notify();
        Ok(())
    }

    /// Keeps the account's send-as aliases and their signatures for compose.
    async fn save_aliases(&self, aliases: Vec<Alias>) -> Result<()> {
        let mut db = self.db.clone();
        if db::kv_get::<Vec<Alias>>(&mut db, ALIASES).await?.as_ref() == Some(&aliases) {
            return Ok(());
        }
        let mut write = self.begin(&mut db).await?;
        db::kv_set(write.exec(), ALIASES, &aliases).await?;
        write.publish().await
    }

    /// Soft bankruptcy, which forgets everything fetched from Gmail and keeps the outbox. The
    /// next bootstrap snapshots a fresh cursor and backfill starts over.
    async fn bankrupt(&self) -> Result<()> {
        log::warn!("Gmail history expired; rebuilding the local cache (queued mutations are kept)");
        let mut db = self.db.clone();
        let mut write = self.begin(&mut db).await?;
        ThreadRow::all().delete().exec(write.exec()).await?;
        MessageRow::all().delete().exec(write.exec()).await?;
        AttachmentRow::all().delete().exec(write.exec()).await?;
        BodyRow::all().delete().exec(write.exec()).await?;
        ListEntry::all().delete().exec(write.exec()).await?;
        ParticipantEntry::all().delete().exec(write.exec()).await?;
        TextDue::all().delete().exec(write.exec()).await?;
        toasty::sql::statement("DELETE FROM search_fts")
            .exec(write.exec())
            .await?;
        ListCursor::all().delete().exec(write.exec()).await?;
        LabelRow::all().delete().exec(write.exec()).await?;
        Cursors::default().save(write.exec()).await?;
        KvRow::filter(KvRow::fields().key().eq(EVICTED_BEFORE))
            .delete()
            .exec(write.exec())
            .await?;
        write.publish().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bankrupt_clears_search_fts_text_due_and_evicted_before() {
        let dir = std::env::temp_dir().join(format!("exo-store-bankrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            crate::Remote::gmail(
                gmail::Endpoints::google(),
                gmail::StaticToken("unused".into()),
            ),
        )
        .await
        .expect("opens");
        let mut db = store.inner.db.clone();
        let msg = gmail::Message {
            id: Some("m1".into()),
            thread_id: Some("t1".into()),
            internal_date: Some(1_794_900_000_000),
            label_ids: vec!["INBOX".into()],
            payload: Some(gmail::MessagePart {
                mime_type: Some("text/plain".into()),
                headers: vec![gmail::MessagePartHeader {
                    name: "Subject".into(),
                    value: "BankruptSubject".into(),
                }],
                body: Some(gmail::MessagePartBody {
                    data: Some(b"uniquebankruptword".to_vec()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            let parsed = crate::provider::gmail::parse::parse(&msg, true).expect("parses");
            crate::truth::save_message(write.exec(), parsed)
                .await
                .expect("saves");
            crate::store::reindex(write.exec(), "t1", &store.inner.router())
                .await
                .expect("indexes");
            crate::provider::gmail::cursors_at(42)
                .save(write.exec())
                .await
                .expect("cursors");
            db::kv_set(write.exec(), EVICTED_BEFORE, &1_700_000_000_000_i64)
                .await
                .expect("kv");
            write.commit().await.expect("commits");
        }

        store.inner.bankrupt().await.expect("bankrupts");
        assert!(
            TextDue::all().exec(&mut db).await.expect("due").is_empty(),
            "text_due cleared"
        );
        assert_eq!(
            db::kv_get::<i64>(&mut db, EVICTED_BEFORE)
                .await
                .expect("kv"),
            None,
            "EVICTED_BEFORE cleared"
        );
        assert!(
            Cursors::load(&mut db).await.expect("cursors").is_empty(),
            "the history cursor cleared"
        );
        let mut s = crate::Search::new("uniquebankruptword");
        assert!(
            store
                .search(&mut s, 10, false, 10)
                .await
                .expect("search")
                .results
                .is_empty(),
            "search_fts cleared"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
