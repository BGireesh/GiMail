//! Triage beyond single threads, done against the provider directly: bulk actions on every
//! conversation a select-all reaches, loaded or not, bulk relabels of every thread a search finds
//! (blocks, moving a sender to Important or Other), and the sender rules that carry those choices
//! to future mail (Gmail filters or Outlook Inbox rules and Focused Inbox overrides).
//!
//! A relabel goes straight to the provider rather than through the outbox, since most of the
//! threads it touches aren't stored. Syncs along the way and afterwards bring the stored ones up
//! to date. Taking labels off stored threads is done on the messages that carry them, a thousand
//! at a time (Gmail's `messages.batchModify`, 50 quota units), as for a single thread.
//! Everything else is one change per thread (`threads.modify`, 10 units).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::db::ListEntry;
use crate::error::{Error, Result};
use crate::ids::labels::{STARRED, UNREAD};
use crate::ids::{Filter, ListId, ThreadId};
use crate::mutation::Mutation;
use crate::provider::{Dated, Find};
use crate::store::{Store, read_views};

/// Provider calls a relabel runs at once.
pub(crate) const CONCURRENCY: usize = 8;
/// Retries per call.
pub(crate) const ATTEMPTS: u32 = 3;
/// A long relabel syncs after every this many threads, so the stored ones leave their lists as
/// it goes.
const SYNC_EVERY: usize = 200;
/// Threads read from the database at once.
const CHUNK: usize = 500;
/// Most message ids `messages.batchModify` takes.
const BATCH_IDS: usize = 1000;

/// A provider-neutral rule that routes a sender's or domain's future mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderRule {
    /// Send future mail from the sender or domain straight to Trash / Deleted Items.
    Block,
    /// Route future mail from the sender to Important / Focused.
    Important,
    /// Route future mail from the sender to Other.
    Other,
}

/// What [`Store::set_sender_rule`] created or replaced, so [`Store::undo_sender_rule`] can
/// reverse it cleanly on either provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum SenderRuleUndo {
    /// The rule was already present, so undo does nothing.
    #[default]
    None,
    /// Gmail filter created (if any) and opposite category filter replaced (if any).
    Gmail {
        /// Newly created filter id to delete on undo.
        created: Option<String>,
        /// Opposite category filter deleted and recreated on undo.
        replaced: Option<Box<gmail::Filter>>,
    },
    /// Outlook Inbox message rule created to block a sender or domain.
    OutlookRule {
        /// Rule id to delete on undo.
        id: String,
    },
    /// Outlook Focused Inbox override created or updated for a sender.
    OutlookOverride {
        /// Override id.
        id: String,
        /// Previous classification if an existing override was updated, or `None` if a new
        /// override was created and must be deleted on undo.
        previous: Option<outlook::InferenceClassificationType>,
    },
}

/// The threads a bulk action is for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkTarget {
    /// A select-all: every thread of a list at or before a date, loaded or not.
    pub list: Option<ListCutoff>,
    /// Threads it's for whatever their date (the loaded rows selected).
    pub include: Vec<ThreadId>,
    /// Threads it leaves alone (the loaded rows not selected).
    pub exclude: Vec<ThreadId>,
}

/// Every thread of a list or search (narrowed by `filter`) dated at or before `before`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCutoff {
    /// The list or search.
    pub scope: Scope,
    /// The filter narrowing it: Unread or Starred, the ones Gmail can search for.
    pub filter: Option<Filter>,
    /// Unread or Starred. Threads these filters pass are left alone (Get Me To Zero's
    /// `Keep unread emails` and `Keep starred emails`).
    pub keep: Vec<Filter>,
    /// The date of the row the select-all started from.
    pub before: Timestamp,
}

/// What a select-all is of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// A list.
    List(ListId),
    /// A search's results, by the query as typed. Its Gmail form is what the search itself asks
    /// Gmail.
    Search(String),
}

/// How far a bulk relabel (or its undo) has got, shared with whoever shows it, and a way to
/// stop it between rounds of Gmail calls.
#[derive(Debug, Default)]
pub struct Progress {
    total: AtomicUsize,
    done: AtomicUsize,
    stopped: AtomicBool,
    /// The bulk action's entry in the log ([`Store::bulk_log`]), which keeps what it has changed
    /// as it goes.
    log: Option<u64>,
}

impl Progress {
    /// The progress of running logged bulk action `log` (not of undoing it), whose entry keeps
    /// what it changes as it goes, so a quit midway loses no more than the last
    /// [`SYNC_EVERY`] threads of its undo.
    pub fn logged(log: u64) -> Self {
        Self {
            log: Some(log),
            ..Self::default()
        }
    }

    /// The progress of one that's over, having changed (or put back) `threads`.
    pub fn ended(threads: usize) -> Self {
        Self {
            total: AtomicUsize::new(threads),
            done: AtomicUsize::new(threads),
            ..Self::default()
        }
    }

    /// How many threads it's changing. It's 0 until they're all found.
    pub fn total(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    /// How many it has changed.
    pub fn done(&self) -> usize {
        self.done.load(Ordering::Relaxed)
    }

    /// Stops it after the calls in flight.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    fn start(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }
}

/// What a bulk relabel changed, which undoing it reverses: threads with the labels each one
/// gained (only those it didn't have) and lost (only those it had).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relabelled {
    groups: Vec<Relabel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Relabel {
    changed: Changed,
    added: Vec<String>,
    removed: Vec<String>,
}

/// The threads a relabel changed.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Changed {
    /// Changed whole (`threads.modify`).
    threads: Vec<ThreadId>,
    /// Changed on these messages only (`messages.batchModify`).
    messages: Vec<(ThreadId, Vec<String>)>,
}

impl Changed {
    fn len(&self) -> usize {
        self.threads.len() + self.messages.len()
    }

    fn into_threads(self) -> Vec<ThreadId> {
        self.threads
            .into_iter()
            .chain(self.messages.into_iter().map(|(thread, _)| thread))
            .collect()
    }
}

impl Relabelled {
    /// How many threads changed.
    pub fn len(&self) -> usize {
        self.groups.iter().map(|group| group.changed.len()).sum()
    }

    /// Nothing changed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A relabel of `added` and `removed` about to start, with nothing changed yet.
    fn starting(added: &[String], removed: &[String]) -> Self {
        let mut relabelled = Self::default();
        relabelled.start(added, removed);
        relabelled
    }

    /// Starts recording a group of threads that gain `added` and lose `removed`.
    fn start(&mut self, added: &[String], removed: &[String]) {
        self.groups.push(Relabel {
            changed: Changed::default(),
            added: added.to_vec(),
            removed: removed.to_vec(),
        });
    }

    /// The group being recorded.
    fn current(&mut self) -> &mut Changed {
        &mut self
            .groups
            .last_mut()
            .expect("a group is started before its threads change")
            .changed
    }
}

/// The label carried by the threads that a filter passes. Only Unread's and Starred's are
/// labels.
pub(crate) fn label_of(filter: Filter) -> Result<&'static str> {
    match filter {
        Filter::Unread => Ok(UNREAD),
        Filter::Starred => Ok(STARRED),
        Filter::Important => Err(Error::Unsearchable("Important")),
        Filter::NoReply => Err(Error::Unsearchable("No reply")),
    }
}

impl Store {
    /// Every thread the provider finds for `query`, newest first.
    ///
    /// # Errors
    /// When the provider can't be reached or refuses the query.
    pub async fn find_threads(&self, query: &str) -> Result<Vec<ThreadId>> {
        let labels = crate::search::label_index(&mut self.inner.db.clone()).await?;
        let router = self.inner.router();
        self.inner.mail.find_threads(query, &labels, &router).await
    }

    /// The threads of `cutoff`'s list that the provider finds `dated` so.
    async fn find(&self, cutoff: &ListCutoff, dated: Dated<'_>) -> Result<Vec<ThreadId>> {
        let labels = crate::search::label_index(&mut self.inner.db.clone()).await?;
        self.inner
            .mail
            .find(Find {
                cutoff,
                dated,
                router: &self.inner.router(),
                labels: &labels,
            })
            .await
    }

    /// Adds `add` and removes `remove` on every message of `threads`, then syncs so stored
    /// threads show it. Returns the threads Gmail changed. A thread that's gone is skipped.
    ///
    /// # Errors
    /// When Gmail can't be reached. Threads already changed stay changed.
    pub async fn relabel(
        &self,
        threads: &[ThreadId],
        add: &[String],
        remove: &[String],
    ) -> Result<Vec<ThreadId>> {
        let mut changed = Relabelled::starting(add, remove);
        self.modify_threads(threads, add, remove, &Progress::default(), &mut changed)
            .await?;
        self.sync().await?;
        Ok(changed
            .groups
            .into_iter()
            .flat_map(|group| group.changed.into_threads())
            .collect())
    }

    /// Applies `mutation` to every thread of `target`, to whole threads, recording in `done`
    /// what changed as it goes, so even a failed or stopped bulk action can be undone
    /// ([`Self::undo_bulk`]). A thread is changed only where it differs (a label added where it's
    /// missing, removed where it's there), which is what makes the undo exact. Stored threads are
    /// read as the user sees them, and for the others Gmail is asked which carry each label. A
    /// stored thread that the list's query finds is only taken when the list on screen has it at
    /// or before the cutoff, because the query only approximates the list.
    ///
    /// # Errors
    /// When Gmail can't be reached or refuses a query, when the list's filter can't be searched
    /// for ([`Error::Unsearchable`]), or for muting before the first sync.
    pub async fn bulk(
        &self,
        target: &BulkTarget,
        mutation: &Mutation,
        progress: &Progress,
        done: &mut Relabelled,
    ) -> Result<()> {
        let muted = match mutation {
            Mutation::Mute | Mutation::Unmute => self
                .inner
                .router()
                .me
                .muted_label
                .ok_or(Error::NotSynced("the mute label"))?,
            _ => String::new(),
        };
        let Some((add, remove)) = mutation.label_change(&muted) else {
            return Ok(());
        };
        let keep = target
            .list
            .iter()
            .flat_map(|cutoff| &cutoff.keep)
            .map(|filter| label_of(*filter).map(str::to_owned))
            .collect::<Result<Vec<String>>>()?;
        let labels: Vec<String> = add.iter().chain(&remove).chain(&keep).cloned().collect();
        let (threads, carried) = self.bulk_threads(target, &labels).await?;
        let has = |thread: &ThreadId, label: &str| {
            carried
                .get(thread.as_str())
                .is_some_and(|labels| labels.contains(label))
        };
        let mut groups: Vec<(Vec<ThreadId>, Vec<String>, Vec<String>)> = Vec::new();
        for thread in threads
            .into_iter()
            .filter(|thread| !keep.iter().any(|label| has(thread, label)))
        {
            let added: Vec<String> = add
                .iter()
                .filter(|label| !has(&thread, label))
                .cloned()
                .collect();
            let removed: Vec<String> = remove
                .iter()
                .filter(|label| has(&thread, label))
                .cloned()
                .collect();
            if added.is_empty() && removed.is_empty() {
                continue;
            }
            match groups.iter_mut().find(|(_, group_added, group_removed)| {
                *group_added == added && *group_removed == removed
            }) {
                Some((threads, _, _)) => threads.push(thread),
                None => groups.push((vec![thread], added, removed)),
            }
        }
        progress.start(groups.iter().map(|(threads, _, _)| threads.len()).sum());
        for (threads, added, removed) in groups {
            done.start(&added, &removed);
            self.modify_threads(&threads, &added, &removed, progress, done)
                .await?;
        }
        self.sync().await?;
        Ok(())
    }

    /// Undoes [`Self::bulk`]: every thread gets back the labels it lost and loses the ones it
    /// gained.
    ///
    /// # Errors
    /// When Gmail can't be reached. What was put back by then stays put back.
    pub async fn undo_bulk(&self, done: &Relabelled, progress: &Progress) -> Result<()> {
        progress.start(done.len());
        for group in done.groups.iter().rev() {
            let mut changed = Relabelled::starting(&group.removed, &group.added);
            let whole = self
                .batch_modify(
                    &group.changed.messages,
                    &group.removed,
                    &group.added,
                    progress,
                    &mut changed,
                )
                .await?;
            let threads: Vec<ThreadId> =
                group.changed.threads.iter().cloned().chain(whole).collect();
            self.modify_whole(
                &threads,
                &group.removed,
                &group.added,
                progress,
                &mut changed,
            )
            .await?;
        }
        self.sync().await?;
        Ok(())
    }

    /// The threads `target` names, the included first, and which of `labels` (the ones the
    /// action changes) each carries, as the user sees it.
    async fn bulk_threads(
        &self,
        target: &BulkTarget,
        labels: &[String],
    ) -> Result<(Vec<ThreadId>, HashMap<String, HashSet<String>>)> {
        let excluded: HashSet<&ThreadId> = target.exclude.iter().collect();
        let mut seen: HashSet<ThreadId> = HashSet::new();
        let mut threads: Vec<ThreadId> = target
            .include
            .iter()
            .filter(|thread| !excluded.contains(thread) && seen.insert((*thread).clone()))
            .cloned()
            .collect();
        let found: Vec<ThreadId> = match &target.list {
            Some(cutoff) => {
                // `before:` finds a thread by any message before the cutoff. One with mail since
                // is dated after it, above the select-all, whatever the store has of it.
                let newer: HashSet<ThreadId> =
                    self.find(cutoff, Dated::After).await?.into_iter().collect();
                self.find(cutoff, Dated::AtOrBefore { label: None })
                    .await?
                    .into_iter()
                    .filter(|thread| {
                        !excluded.contains(thread)
                            && !seen.contains(thread)
                            && !newer.contains(thread)
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        let candidates: Vec<String> = threads
            .iter()
            .chain(&found)
            .map(|thread| thread.as_str().to_owned())
            .collect();
        let mut db = self.inner.db.clone();
        let mut carried: HashMap<String, HashSet<String>> = HashMap::new();
        for chunk in candidates.chunks(CHUNK) {
            for (id, view) in read_views(&mut db, chunk).await? {
                let has: HashSet<String> = labels
                    .iter()
                    .filter(|label| view.has(label))
                    .cloned()
                    .collect();
                carried.insert(id, has);
            }
        }
        if let Some(cutoff) = &target.list {
            let (stored, unstored): (Vec<ThreadId>, Vec<ThreadId>) = found
                .into_iter()
                .partition(|thread| carried.contains_key(thread.as_str()));
            let stored: Vec<String> = stored
                .iter()
                .map(|thread| thread.as_str().to_owned())
                .collect();
            let listed = self.listed_by(cutoff, &stored, &mut db).await?;
            if !unstored.is_empty() {
                let unstored: HashSet<&ThreadId> = unstored.iter().collect();
                for label in labels {
                    for thread in self
                        .find(cutoff, Dated::AtOrBefore { label: Some(label) })
                        .await?
                    {
                        if unstored.contains(&thread) {
                            carried
                                .entry(thread.as_str().to_owned())
                                .or_default()
                                .insert(label.clone());
                        }
                    }
                }
            }
            threads.extend(
                stored
                    .into_iter()
                    .filter(|thread| listed.contains(thread))
                    .map(ThreadId::new),
            );
            threads.extend(unstored);
        }
        Ok((threads, carried))
    }

    /// Of the stored `threads`, those that `cutoff`'s list or search has at or before the
    /// cutoff's date and that its filter passes.
    async fn listed_by(
        &self,
        cutoff: &ListCutoff,
        threads: &[String],
        db: &mut toasty::Db,
    ) -> Result<HashSet<String>> {
        let fields = ListEntry::fields();
        let mut listed = HashSet::new();
        let before = cutoff.before.as_millisecond();
        for chunk in threads.chunks(CHUNK) {
            let mut passing: HashSet<String> = match &cutoff.scope {
                Scope::List(list) => ListEntry::filter(
                    fields
                        .thread_id()
                        .in_list(chunk)
                        .and(fields.list_id().in_list(list.members()))
                        .and(fields.sort().le(before)),
                )
                .exec(db)
                .await?
                .into_iter()
                .map(|entry| entry.thread_id)
                .collect(),
                Scope::Search(_) => read_views(db, chunk)
                    .await?
                    .into_iter()
                    .filter(|(_, view)| view.gmail_sort() <= before)
                    .map(|(id, _)| id)
                    .collect(),
            };
            if let Some(filter) = cutoff.filter {
                let filtered: HashSet<String> = ListEntry::filter(
                    fields
                        .thread_id()
                        .in_list(chunk)
                        .and(fields.list_id().eq(filter.key())),
                )
                .exec(db)
                .await?
                .into_iter()
                .map(|entry| entry.thread_id)
                .collect();
                passing.retain(|thread| filtered.contains(thread));
            }
            listed.extend(passing);
        }
        Ok(listed)
    }

    /// Adds `add` and removes `remove` on every message of `threads` until done or `progress`
    /// is stopped, counting them in `progress` and recording in `changed`'s current group what
    /// Gmail changed. A relabel that only removes takes the labels off the stored threads'
    /// messages that carry them in batches. The rest are changed whole.
    async fn modify_threads(
        &self,
        threads: &[ThreadId],
        add: &[String],
        remove: &[String],
        progress: &Progress,
        changed: &mut Relabelled,
    ) -> Result<()> {
        let (carrying, mut whole) = if add.is_empty() {
            self.carrying(threads, remove).await?
        } else {
            (Vec::new(), threads.to_vec())
        };
        whole.extend(
            self.batch_modify(&carrying, add, remove, progress, changed)
                .await?,
        );
        self.modify_whole(&whole, add, remove, progress, changed)
            .await
    }

    /// Of `threads`, the stored ones with the messages carrying any of `labels` as the user
    /// sees them, and the others.
    async fn carrying(
        &self,
        threads: &[ThreadId],
        labels: &[String],
    ) -> Result<(Vec<(ThreadId, Vec<String>)>, Vec<ThreadId>)> {
        let mut db = self.inner.db.clone();
        let (mut carrying, mut others) = (Vec::new(), Vec::new());
        for chunk in threads.chunks(CHUNK) {
            let ids: Vec<String> = chunk
                .iter()
                .map(|thread| thread.as_str().to_owned())
                .collect();
            let views = read_views(&mut db, &ids).await?;
            for thread in chunk {
                match views.get(thread.as_str()) {
                    Some(view) => carrying.push((thread.clone(), view.carriers(labels))),
                    None => others.push(thread.clone()),
                }
            }
        }
        Ok((carrying, others))
    }

    /// `messages.batchModify` on the messages of `threads`, up to [`BATCH_IDS`] a call, until
    /// done or `progress` is stopped. Returns the threads of a batch Gmail refused (a message
    /// deleted since it was stored), to be changed whole instead.
    async fn batch_modify(
        &self,
        threads: &[(ThreadId, Vec<String>)],
        add: &[String],
        remove: &[String],
        progress: &Progress,
        changed: &mut Relabelled,
    ) -> Result<Vec<ThreadId>> {
        let mut refused = Vec::new();
        let mut rest = threads;
        while !rest.is_empty() && !progress.is_stopped() {
            // Whole threads to a batch, as many as fit (a thread with more than fits goes alone).
            let mut ids = 0;
            let taken = rest
                .iter()
                .take_while(|(_, messages)| {
                    ids += messages.len();
                    ids <= BATCH_IDS
                })
                .count()
                .max(1);
            let (batch, after) = rest.split_at(taken);
            rest = after;
            let ids: Vec<String> = batch
                .iter()
                .flat_map(|(_, messages)| messages.iter().cloned())
                .collect();
            let sent = match ids.len() {
                0 => Ok(()),
                1..=BATCH_IDS => self.inner.mail.relabel_messages(&ids, add, remove).await,
                _ => {
                    refused.push(batch[0].0.clone());
                    continue;
                }
            };
            match sent {
                Ok(()) => changed.current().messages.extend_from_slice(batch),
                Err(failure) if failure.is_permanent() => {
                    log::info!(
                        "Gmail refused a batch of {} messages ({}); changing their threads whole",
                        ids.len(),
                        failure.message()
                    );
                    refused.extend(batch.iter().map(|(thread, _)| thread.clone()));
                    continue;
                }
                Err(failure) => return Err(failure.into()),
            }
            self.progressed(progress, batch.len(), changed).await?;
        }
        Ok(refused)
    }

    /// `threads.modify` on `threads`, a round of [`CONCURRENCY`] at a time until done or
    /// `progress` is stopped. A thread that's gone is skipped.
    async fn modify_whole(
        &self,
        threads: &[ThreadId],
        add: &[String],
        remove: &[String],
        progress: &Progress,
        changed: &mut Relabelled,
    ) -> Result<()> {
        // In rounds rather than a stream, because a stream's closure makes the future unsendable
        // (rust-lang/rust#100013), and tasks spawn this.
        for round in threads.chunks(CONCURRENCY) {
            if progress.is_stopped() {
                break;
            }
            for (thread, result) in futures::future::join_all(
                round
                    .iter()
                    .map(|thread| self.modify_thread(thread, add, remove)),
            )
            .await
            {
                match result {
                    Ok(()) => changed.current().threads.push(thread),
                    Err(failure) if failure.is_not_found() => {}
                    Err(failure) => return Err(failure.into()),
                }
            }
            self.progressed(progress, round.len(), changed).await?;
        }
        Ok(())
    }

    /// Counts `threads` more done, syncing every [`SYNC_EVERY`] threads, so the stored ones
    /// leave their lists as the relabel goes. A logged relabel's entry then gets what it has
    /// `changed` so far.
    async fn progressed(
        &self,
        progress: &Progress,
        threads: usize,
        changed: &Relabelled,
    ) -> Result<()> {
        let done = progress.done.fetch_add(threads, Ordering::Relaxed) + threads;
        if done / SYNC_EVERY > (done - threads) / SYNC_EVERY {
            self.sync().await?;
            if let Some(log) = progress.log {
                self.checkpoint_bulk(log, changed).await?;
            }
        }
        Ok(())
    }

    /// `add` and `remove` on every message of `thread`, paired with it.
    async fn modify_thread(
        &self,
        thread: &ThreadId,
        add: &[String],
        remove: &[String],
    ) -> (ThreadId, Result<(), crate::api::Failure>) {
        let result = self
            .inner
            .mail
            .relabel_thread(thread.as_str(), add, remove)
            .await;
        (thread.clone(), result)
    }

    /// Creates or updates the provider's rule for `from`:
    /// - On Gmail: a filter adding `TRASH` (block), `CATEGORY_PERSONAL` (Important), or
    ///   `CATEGORY_SOCIAL` (Other), replacing any opposite category filter.
    /// - On Outlook: an Inbox message rule moving mail from the sender or domain to Deleted Items
    ///   for [`SenderRule::Block`], or a Focused Inbox override (`focused` / `other`) for
    ///   [`SenderRule::Important`] and [`SenderRule::Other`] (addresses only).
    ///
    /// Returns a [`SenderRuleUndo`] token for [`Self::undo_sender_rule`].
    ///
    /// # Errors
    /// When the provider can't be reached, refuses the rule, or does not support the rule shape
    /// (such as Focused Inbox overrides for bare domains on Outlook).
    pub async fn set_sender_rule(&self, from: &str, rule: SenderRule) -> Result<SenderRuleUndo> {
        self.inner.mail.set_sender_rule(from, rule).await
    }

    /// Reverses a previous [`Self::set_sender_rule`].
    ///
    /// # Errors
    /// When the provider can't be reached or refuses the change.
    pub async fn undo_sender_rule(&self, undo: &SenderRuleUndo) -> Result<()> {
        self.inner.mail.undo_sender_rule(undo).await
    }

    /// Unblocks `from`, an address or domain, on either Gmail or Outlook. Deletes every provider
    /// rule shaped as a block for `from` (Gmail filters adding Trash alone, or Outlook Inbox rules
    /// deleting / moving mail from `from` to Deleted Items alone), leaving already-trashed mail
    /// where it is. Returns how many rules were deleted.
    ///
    /// # Errors
    /// When the provider can't be reached.
    pub async fn delete_block_filters(&self, from: &str) -> Result<usize> {
        self.inner.mail.delete_block_rules(from).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_listed_by_filters_out_stored_threads_newer_than_cutoff() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-bulk-listed-by-{}", std::process::id()));
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
        let insert = |id: &str, thread_id: &str, ms: i64| gmail::Message {
            id: Some(id.into()),
            thread_id: Some(thread_id.into()),
            internal_date: Some(ms),
            label_ids: vec!["INBOX".into()],
            payload: Some(gmail::MessagePart {
                mime_type: Some("text/plain".into()),
                headers: vec![gmail::MessagePartHeader {
                    name: "From".into(),
                    value: "Tom <tom@brightmoor.co>".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            for (m, t, ms) in [
                ("m1", "t_before", 1_794_319_200_250),
                ("m2", "t_subsec_after", 1_794_319_200_750),
            ] {
                let parsed =
                    crate::provider::gmail::parse::parse(&insert(m, t, ms), false).expect("parses");
                crate::truth::save_message(write.exec(), parsed)
                    .await
                    .expect("saves");
                crate::store::reindex(write.exec(), t, &store.inner.router())
                    .await
                    .expect("indexes");
            }
            write.commit().await.expect("commits");
        }

        let cutoff = ListCutoff {
            scope: Scope::Search("from:tom@brightmoor.co".into()),
            filter: None,
            keep: Vec::new(),
            before: Timestamp::from_millisecond(1_794_319_200_250).unwrap(),
        };
        let listed = store
            .listed_by(
                &cutoff,
                &["t_before".to_owned(), "t_subsec_after".to_owned()],
                &mut db,
            )
            .await
            .expect("listed");
        assert_eq!(listed, HashSet::from(["t_before".to_owned()]));

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
