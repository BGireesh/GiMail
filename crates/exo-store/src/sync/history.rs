//! Incremental sync from the provider's changes. The provider reads every change since the
//! stored cursors first, grouped per thread. They're applied idempotently, and the cursors move
//! only once everything is stored.

use std::collections::{BTreeSet, HashSet};

use futures::{StreamExt, stream};

use super::ATTEMPTS;
use crate::db::{LabelRow, MessageRow};
use crate::error::Result;
use crate::ids::labels::DRAFT;
use crate::provider::{Changes, Cursors, ThreadChanges};
use crate::quota::Lane;
use crate::store::{Inner, reindex};
use crate::truth;

/// Threads applied at once, and messages fetched at once per thread.
const THREAD_CONCURRENCY: usize = 3;
const MESSAGE_CONCURRENCY: usize = 5;

/// What a run of history did: how many threads it changed, and the messages it added that the
/// store didn't have (thread and message ids).
#[derive(Debug, Default)]
pub(super) struct Applied {
    pub changed: usize,
    pub arrived: Vec<(String, String)>,
}

impl Inner {
    /// Applies Gmail history since the stored cursor, or returns `None` when history can't be
    /// used and the cache must be rebuilt.
    pub(super) async fn sync_history(&self) -> Result<Option<Applied>> {
        // However it ends, a failure included, the round is over.
        let _caught_up = CaughtUp(&self.progress);
        let mut db = self.db.clone();
        let since = Cursors::load(&mut db).await?;
        if since.is_empty() {
            return Ok(Some(Applied::default()));
        }
        let Some(changes) = self.mail.changes(&since).await? else {
            return Ok(None);
        };
        self.apply_changes(&mut db, &since, changes).await.map(Some)
    }

    /// Applies `changes` read from `since`, then stores their cursors.
    async fn apply_changes(
        &self,
        db: &mut toasty::Db,
        since: &Cursors,
        changes: Changes,
    ) -> Result<Applied> {
        let Changes {
            mut threads,
            labels,
            relabelled,
            removed,
            next,
        } = changes;
        // Messages gone without a thread belong to the threads the store has them in.
        for chunk in removed
            .into_iter()
            .collect::<Vec<_>>()
            .chunks(crate::db::IN_LIST_CHUNK)
        {
            for row in MessageRow::filter(MessageRow::fields().id().in_list(chunk))
                .exec(&mut *db)
                .await?
            {
                let thread = threads.entry(row.thread_id).or_default();
                thread.deleted.insert(row.id);
                thread.changes += 1;
            }
        }
        if threads.is_empty() && labels.is_empty() && !relabelled {
            if next != *since {
                let mut write = self.begin(db).await?;
                next.save(write.exec()).await?;
                write.commit().await?;
            }
            return Ok(Applied::default());
        }

        // When the changes name a label created elsewhere, pick up its name and color first, so
        // that if this fails the next sync reads the same changes and tries again.
        let known: HashSet<String> = LabelRow::all()
            .select(LabelRow::fields().id())
            .exec(&mut *db)
            .await?
            .into_iter()
            .collect();
        if relabelled || labels.iter().any(|label| !known.contains(label)) {
            self.refresh_labels().await?;
        }

        let changed = threads.len();
        let total: usize = threads.values().map(|changes| changes.changes).sum();
        let progress = &self.progress;
        progress.send_modify(|progress| progress.changes = Some((total, 0)));
        let touched: HashSet<String> = threads.keys().cloned().collect();
        // Messages that are no longer drafts, because they were deleted or (should Gmail keep
        // the id) sent.
        let undrafted: HashSet<String> = threads
            .values()
            .flat_map(|changes| {
                let relabelled = changes
                    .labels
                    .iter()
                    .filter(|(_, (_, labels))| !labels.iter().any(|label| label == DRAFT));
                changes.deleted.iter().chain(relabelled.map(|(id, _)| id))
            })
            .cloned()
            .collect();
        let arrived = stream::iter(threads)
            .map(|(thread, changes)| async move {
                let count = changes.changes;
                let applied = self.apply_history(thread, changes).await;
                progress.send_modify(|progress| {
                    if let Some((_, done)) = &mut progress.changes {
                        *done += count;
                    }
                });
                applied
            })
            .buffer_unordered(THREAD_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<Vec<(String, String)>>>>()?
            .concat();
        self.forget_drafts_gone(&undrafted).await?;
        // A reply that came in has called off an `if no reply` send.
        self.unschedule_replied(&touched).await?;
        let mut write = self.begin(db).await?;
        next.save(write.exec()).await?;
        write.commit().await?;
        Ok(Applied { changed, arrived })
    }

    /// Applies one thread's changes. Returns the messages history added that the store didn't
    /// have and now does (thread and message ids).
    async fn apply_history(
        &self,
        thread: String,
        changes: ThreadChanges,
    ) -> Result<Vec<(String, String)>> {
        let mut db = self.db.clone();
        let stored: HashSet<String> = MessageRow::filter_by_thread_id(&thread)
            .select(MessageRow::fields().id())
            .exec(&mut db)
            .await?
            .into_iter()
            .collect();
        if stored.is_empty() {
            // For a thread we don't have, fetch its rows if it gained mail or labels that are
            // still there (it may now belong in a list), and ignore deletions. A message added
            // and deleted since (an earlier version of a draft, say) leaves nothing to fetch.
            if changes
                .added
                .iter()
                .chain(changes.labels.keys())
                .all(|id| changes.deleted.contains(id))
            {
                return Ok(Vec::new());
            }
            let Some(fetched) = self
                .mail
                .rows(&thread, &stored, Lane::Sync, ATTEMPTS)
                .await?
            else {
                return Ok(Vec::new());
            };
            let arrived = fetched
                .messages
                .iter()
                .map(|parsed| parsed.message.id.clone())
                .filter(|id| changes.added.contains(id))
                .map(|id| (thread.clone(), id))
                .collect();
            let mut write = self.begin(&mut db).await?;
            if let Some(id) = truth::save_rows(write.exec(), fetched).await? {
                reindex(write.exec(), &id, &self.router()).await?;
            }
            write.publish().await?;
            return Ok(arrived);
        }

        let missing: BTreeSet<String> = changes
            .added
            .iter()
            .chain(changes.labels.keys())
            .filter(|id| !stored.contains(*id) && !changes.deleted.contains(*id))
            .cloned()
            .collect();
        let mail = &self.mail;
        let fetched: Vec<_> = stream::iter(missing)
            .map(|id| async move { mail.message(&id, true, Lane::Sync, ATTEMPTS).await })
            .buffer_unordered(MESSAGE_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()?;

        let arrived = fetched
            .iter()
            .flatten()
            .map(|parsed| parsed.message.id.clone())
            .filter(|id| changes.added.contains(id))
            .map(|id| (thread.clone(), id))
            .collect();
        let mut write = self.begin(&mut db).await?;
        for parsed in fetched.into_iter().flatten() {
            truth::save_message(write.exec(), parsed).await?;
        }
        for (id, (record, labels)) in &changes.labels {
            truth::set_labels(write.exec(), id, labels, *record).await?;
        }
        let deleted: Vec<String> = changes.deleted.into_iter().collect();
        truth::delete_messages(write.exec(), &deleted).await?;
        let emptied = MessageRow::filter_by_thread_id(&thread)
            .first()
            .exec(write.exec())
            .await?
            .is_none();
        if emptied {
            truth::delete_thread(write.exec(), &thread).await?;
        } else {
            truth::bump_thread(write.exec(), &thread, changes.latest).await?;
            reindex(write.exec(), &thread, &self.router()).await?;
        }
        write.publish().await?;
        Ok(arrived)
    }
}

/// Marks a round of history over when dropped, meaning none is running and the session has
/// caught up.
struct CaughtUp<'a>(&'a tokio::sync::watch::Sender<crate::SyncProgress>);

impl Drop for CaughtUp<'_> {
    fn drop(&mut self) {
        self.0.send_replace(crate::SyncProgress {
            caught_up: true,
            changes: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::provider::gmail::{cursors_at, history_id};

    #[tokio::test]
    async fn changes_with_nothing_in_them_store_only_a_cursor_that_moved() {
        let dir =
            std::env::temp_dir().join(format!("exo-empty-history-cursor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            crate::Remote::gmail(gmail::Endpoints::google(), gmail::StaticToken("tok".into())),
        )
        .await
        .expect("opens");
        let mut db = store.inner.db.clone();
        let since = cursors_at(500);
        since.save(&mut db).await.expect("sets start");
        let stored = |mut db: toasty::Db| async move {
            history_id(&Cursors::load(&mut db).await.expect("reads"))
        };
        let nothing = |next: Cursors| Changes {
            next,
            ..Changes::default()
        };

        let applied = store
            .inner
            .apply_changes(&mut db, &since, nothing(since.clone()))
            .await
            .expect("applies");
        assert_eq!(applied.changed, 0);
        assert_eq!(stored(db.clone()).await, Some(500));

        let applied = store
            .inner
            .apply_changes(&mut db, &since, nothing(cursors_at(950)))
            .await
            .expect("applies");
        assert_eq!(applied.changed, 0);
        assert_eq!(stored(db.clone()).await, Some(950));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
