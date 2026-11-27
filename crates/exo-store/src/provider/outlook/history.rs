//! What changed, from Graph's deltas: one for the folder tree, and one per folder for its
//! messages, each kept as a cursor of its own.
//!
//! A message that moves leaves one folder's delta (`@removed`) and arrives in another's. One that
//! left a folder and turned up in no other folder this round is asked for by id: it's gone, or it
//! moved while the round read the folders, and it's read where it is now. A folder whose delta
//! Graph no longer knows (410) is listed again, alone.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use outlook::{MailFolder, Message, OpenTypeExtension, Page};
use url::Url;

use super::Outlook;
use super::mailbox::{MARKERS, Mailbox};
use super::parse::{LABELS, expand};
use crate::api::Failure;
use crate::error::{ProviderError, Result};
use crate::provider::{Changes, Cursors, ThreadChanges};
use crate::quota::Lane;
use crate::sync::ATTEMPTS;

/// The cursor scope of the folder tree's delta.
const FOLDERS: &str = "folders";
/// The cursor scope holding the mailbox's newest `receivedDateTime` read (epoch milliseconds).
/// Mail an incremental delta brings that wasn't moved from another folder and was received since
/// is new; a message moved into a folder or returned by a full re-list isn't.
const ARRIVALS: &str = "arrivals";
/// Messages a delta page carries.
const PAGE_SIZE: u32 = 50;
/// More changes than this in a round means the cache is too far behind to patch, so sync starts
/// over.
const MAX_CHANGES: usize = 10_000;

/// Where a folder's round of its delta starts.
enum Start {
    /// From now: the round reads (almost) nothing, and ends with a link to what changes next.
    Now,
    /// Every message in the folder.
    Everything,
    /// From the link the last round ended with.
    Link(Url),
}

/// The latest state a single folder's delta pages reported for one message ID (page order wins:
/// a later `@removed` supersedes an earlier snapshot in the same folder, and a later snapshot
/// supersedes an earlier `@removed`).
enum FolderItem {
    Present(Box<Message>),
    Removed,
}

/// A folder's round of its delta, as read.
#[derive(Default)]
struct Round {
    items: HashMap<String, FolderItem>,
    /// The link the next round starts from.
    next: Option<Url>,
    /// Graph no longer knows the round's link.
    expired: bool,
    /// This round listed the folder from scratch (new folder, role change, or 410 expiry), so its
    /// messages aren't treated as fresh arrivals.
    relisted: bool,
}

impl Outlook {
    /// The cursors at the mailbox now: the folder tree's, each folder's from now, and the newest
    /// mail's date.
    pub(super) async fn start(&self) -> Result<Cursors> {
        let (mailbox, tree) = self.load().await?;
        let newest = self
            .graph
            .call(Lane::Show, ATTEMPTS, |graph| {
                graph
                    .mail()
                    .messages()
                    .select("receivedDateTime")
                    .order_by("receivedDateTime desc")
                    .top(1)
            })
            .await?
            .value
            .first()
            .and_then(|message| message.received_date_time)
            .map_or(0, jiff::Timestamp::as_millisecond);
        let mut folders: Vec<String> = mailbox.read().into_iter().map(str::to_owned).collect();
        folders.sort();
        let starts = folders.iter().map(|_| Start::Now).collect();
        let rounds = self.rounds(&folders, starts, Lane::Show).await?;
        let mut cursors: Vec<(String, String)> = folders
            .iter()
            .zip(rounds)
            .filter_map(|(folder, round)| Some((folder.clone(), round.next?.to_string())))
            .collect();
        cursors.extend(tree.map(|link| (FOLDERS.to_owned(), link.to_string())));
        cursors.push((ARRIVALS.to_owned(), newest.to_string()));
        Ok(cursors.into_iter().collect())
    }

    /// What changed since `since`, or `None` when it can't be patched in (cursors that aren't
    /// Graph's, or too many changes).
    pub(super) async fn changes(&self, since: &Cursors) -> Result<Option<Changes>> {
        let (Some(tree), Some(since_watermark)) = (
            since.get(FOLDERS).and_then(|link| Url::parse(link).ok()),
            since.get(ARRIVALS).and_then(parse_watermark),
        ) else {
            return Ok(None);
        };
        let before = self.mailbox().await?;
        let (mailbox, next_tree, relabelled) = self.tree_changes(tree.clone()).await?;
        let mut labels = BTreeSet::new();
        let tree = next_tree.unwrap_or(tree);

        let mut folders: Vec<String> = mailbox.read().into_iter().map(str::to_owned).collect();
        folders.sort();
        let starts = folders
            .iter()
            .map(|folder| {
                let link = since.get(folder).and_then(|link| Url::parse(link).ok());
                match link {
                    // A folder moved into or out of Deleted Items or Junk Email changes the
                    // labels of all its mail, which is read again.
                    Some(_) if before.folder_labels(folder) != mailbox.folder_labels(folder) => {
                        Start::Everything
                    }
                    Some(link) => Start::Link(link),
                    // New since the last round: everything in it came from elsewhere, or is new.
                    None => Start::Everything,
                }
            })
            .collect();
        let mut rounds = self.rounds(&folders, starts, Lane::Sync).await?;
        let expired: Vec<usize> = (0..rounds.len()).filter(|&at| rounds[at].expired).collect();
        if !expired.is_empty() {
            let names: Vec<String> = expired.iter().map(|&at| folders[at].clone()).collect();
            log::info!(
                "{} folders' deltas expired; listing them again",
                names.len()
            );
            let again = self
                .rounds(
                    &names,
                    names.iter().map(|_| Start::Everything).collect(),
                    Lane::Sync,
                )
                .await?;
            for (at, round) in expired.into_iter().zip(again) {
                rounds[at] = round;
            }
        }

        // Each message by id with its newest observed state, its folder, and whether it came from
        // a move or full folder re-list (not a fresh arrival). If two folders report the same
        // message with different `lastModifiedDateTime`, the newer state wins; if they tie on the
        // same second in different folders, `where_now` resolves its current state.
        let mut read: HashMap<String, (Message, String, bool)> = HashMap::new();
        let mut left = BTreeSet::new();
        let mut ambiguous = BTreeSet::new();
        for (folder, round) in folders.iter().zip(&mut rounds) {
            let relisted = round.relisted;
            for (id, item) in round.items.drain() {
                let message = match item {
                    FolderItem::Removed => {
                        left.insert(id);
                        continue;
                    }
                    FolderItem::Present(message) => *message,
                };
                let here = message.parent_folder_id.clone().unwrap_or(folder.clone());
                match read.get_mut(&id) {
                    Some((existing, existing_folder, moved_or_relisted)) => {
                        let new_mod = message.last_modified_date_time;
                        let old_mod = existing.last_modified_date_time;
                        let cross_folder = *existing_folder != here;
                        if cross_folder {
                            *moved_or_relisted = true;
                        } else {
                            *moved_or_relisted |= relisted;
                        }
                        match new_mod.cmp(&old_mod) {
                            std::cmp::Ordering::Greater => {
                                *existing = message;
                                *existing_folder = here;
                                ambiguous.remove(&id);
                            }
                            std::cmp::Ordering::Less => {
                                ambiguous.remove(&id);
                            }
                            std::cmp::Ordering::Equal => {
                                if cross_folder {
                                    ambiguous.insert(id);
                                }
                            }
                        }
                    }
                    None => {
                        read.insert(id, (message, here, relisted));
                    }
                }
            }
        }
        if read.len() + left.len() > MAX_CHANGES {
            log::warn!(
                "{} changed messages: rebuilding instead",
                read.len() + left.len()
            );
            return Ok(None);
        }
        let mut threads: BTreeMap<String, ThreadChanges> = BTreeMap::new();
        let mut removed = BTreeSet::new();
        let unresolved: Vec<String> = left
            .iter()
            .filter(|id| !read.contains_key(*id))
            .chain(ambiguous.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for (id, found) in unresolved.iter().zip(self.where_now(&unresolved).await?) {
            match found {
                Some(message) => {
                    let folder = message.parent_folder_id.clone().unwrap_or_default();
                    if mailbox.is_read(&folder) {
                        read.insert(id.clone(), (message, folder, true));
                    } else {
                        read.remove(id);
                        if let Some(thread) = message.conversation_id.clone() {
                            let thread = threads.entry(thread).or_default();
                            thread.deleted.insert(id.clone());
                            thread.changes += 1;
                        }
                    }
                }
                None => {
                    read.remove(id);
                    removed.insert(id.clone());
                }
            }
        }
        // Refresh the master category list after `where_now` populates moved messages too.
        let mailbox = self
            .mailbox_for(read.values().map(|(message, _, _)| message))
            .await?;

        let mut watermark = since_watermark;
        for (id, (message, folder, moved_or_relisted)) in read {
            let Some(thread) = message.conversation_id.clone() else {
                continue;
            };
            let version = self.versions.read(message.last_modified_date_time);
            let keys = mailbox.labels(&message, &folder);
            labels.extend(keys.iter().cloned());
            let received = message
                .received_date_time
                .map_or(0, jiff::Timestamp::as_millisecond);
            watermark = watermark.max(received);
            let thread = threads.entry(thread).or_default();
            // Graph's dates are to the second, so incremental mail received in the second of the
            // newest read counts too; moved messages, re-listed folders, and mail already stored
            // are never fresh arrivals.
            let moved = moved_or_relisted || left.contains(&id);
            if !moved && received >= since_watermark {
                thread.added.insert(id.clone());
            }
            thread.labels.insert(id, (version, keys));
            thread.latest = thread.latest.max(version);
            thread.changes += 1;
        }

        let mut next: Vec<(String, String)> = folders
            .into_iter()
            .zip(rounds)
            .filter_map(|(folder, round)| Some((folder, round.next?.to_string())))
            .collect();
        next.push((FOLDERS.to_owned(), tree.to_string()));
        next.push((ARRIVALS.to_owned(), watermark.to_string()));
        Ok(Some(Changes {
            threads,
            relabelled,
            labels,
            removed,
            next: next.into_iter().collect(),
        }))
    }

    /// The folder tree's changes since `link`, applied to the mailbox. Returns the mailbox now,
    /// where the tree's next round starts, and whether a folder was made, renamed, moved or
    /// deleted, which changes the labels.
    async fn tree_changes(
        &self,
        link: Url,
    ) -> Result<(std::sync::Arc<Mailbox>, Option<Url>, bool)> {
        let page = self
            .graph
            .call(Lane::Sync, ATTEMPTS, |graph| {
                graph.follow::<MailFolder>(&link).max_page_size(PAGE_SIZE)
            })
            .await;
        let mut page = match page {
            Ok(page) => page,
            Err(failure) if expired(&failure) => {
                // The tree is read again, and any folder may have changed.
                let (mailbox, tree) = self.load().await?;
                return Ok((mailbox, tree, true));
            }
            Err(failure) => return Err(failure.into()),
        };
        let mut folders = std::mem::take(&mut page.value);
        while let Some(next) = page.next_link.take() {
            page = self
                .graph
                .call(Lane::Sync, ATTEMPTS, |graph| {
                    graph.follow::<MailFolder>(&next).max_page_size(PAGE_SIZE)
                })
                .await?;
            folders.append(&mut page.value);
        }
        let mut mailbox = (*self.mailbox().await?).clone();
        let mut relabelled = false;
        for folder in &folders {
            let Some(id) = folder.id.clone() else {
                continue;
            };
            // The tree's delta also reports folders whose counts alone changed.
            let moved = if folder.removed.is_some() {
                mailbox.remove(&id)
            } else {
                mailbox.put(folder)
            };
            relabelled |= moved;
        }
        Ok((self.keep(mailbox), page.delta_link, relabelled))
    }

    /// A round of each of `folders`' deltas, starting where `starts` says, in batches.
    async fn rounds(
        &self,
        folders: &[String],
        starts: Vec<Start>,
        lane: Lane,
    ) -> Result<Vec<Round>> {
        let mut rounds: Vec<Round> = starts
            .iter()
            .map(|start| Round {
                relisted: matches!(start, Start::Everything),
                ..Round::default()
            })
            .collect();
        // Each folder's next page, until its round ends, and whether its messages are news. A
        // round from now reads what's there already, which isn't.
        let mut pending: Vec<(usize, Start, bool)> = starts
            .into_iter()
            .enumerate()
            .map(|(folder, start)| {
                let news = !matches!(start, Start::Now);
                (folder, start, news)
            })
            .collect();
        while !pending.is_empty() {
            let pages = self
                .graph
                .each(lane, ATTEMPTS, pending.len(), |graph, at| {
                    let (folder, start, _) = &pending[at];
                    let fresh = || {
                        graph
                            .mail()
                            .messages_delta(&folders[*folder])
                            .select(LABELS)
                    };
                    match start {
                        Start::Now => fresh().top(1),
                        Start::Everything => fresh(),
                        Start::Link(link) => graph.follow::<Message>(link),
                    }
                    .max_page_size(PAGE_SIZE)
                })
                .await?;
            let mut next = Vec::new();
            for ((folder, _, news), page) in pending.into_iter().zip(pages) {
                let round = &mut rounds[folder];
                let page: Page<Message> = match page {
                    Ok(page) => page,
                    Err(error) if is_expired(&error) => {
                        round.expired = true;
                        continue;
                    }
                    // The folder went since the tree was read, and so does its cursor.
                    Err(error) if error.is_not_found() => continue,
                    Err(error) => return Err(ProviderError::from(error).into()),
                };
                for message in page.value {
                    let Some(id) = message.id.clone() else {
                        continue;
                    };
                    if message.removed.is_some() {
                        round.items.insert(id, FolderItem::Removed);
                    } else if news {
                        round
                            .items
                            .insert(id, FolderItem::Present(Box::new(message)));
                    }
                }
                match page.next_link {
                    Some(link) => next.push((folder, Start::Link(link), news)),
                    None => round.next = page.delta_link,
                }
            }
            pending = next;
        }
        self.mark(&mut rounds, lane).await?;
        Ok(rounds)
    }

    /// Puts Exo's markers on `rounds`' messages. Some mailboxes refuse to expand extensions on a
    /// message delta (400 `Parsing OData Select and Expand failed`), so each marker's messages
    /// are listed instead.
    async fn mark(&self, rounds: &mut [Round], lane: Lane) -> Result<(), Failure> {
        let has_present = rounds.iter().any(|round| {
            round
                .items
                .values()
                .any(|item| matches!(item, FolderItem::Present(_)))
        });
        if !has_present {
            return Ok(());
        }
        for name in MARKERS {
            let marked: HashSet<String> = self
                .graph
                .call(lane, ATTEMPTS, |graph| {
                    graph
                        .mail()
                        .messages()
                        .filter(&OpenTypeExtension::filter(name))
                        .select("id")
                        .max_page_size(PAGE_SIZE)
                        .all()
                })
                .await?
                .value
                .into_iter()
                .filter_map(|message| message.id)
                .collect();
            if marked.is_empty() {
                continue;
            }
            for item in rounds.iter_mut().flat_map(|round| round.items.values_mut()) {
                if let FolderItem::Present(message) = item
                    && message.id.as_ref().is_some_and(|id| marked.contains(id))
                {
                    message
                        .extensions
                        .push(OpenTypeExtension::new(name, serde_json::Map::new()));
                }
            }
        }
        Ok(())
    }

    /// Where each of `ids` is now, by id across folders, or `None` for one that's gone.
    async fn where_now(&self, ids: &[String]) -> Result<Vec<Option<Message>>, Failure> {
        let markers = expand(false);
        let found = self
            .graph
            .each(Lane::Sync, ATTEMPTS, ids.len(), |graph, at| {
                graph
                    .mail()
                    .message(&ids[at])
                    .select(LABELS)
                    .expand(&markers)
            })
            .await?;
        found
            .into_iter()
            .map(|found| match found {
                Ok(message) => Ok(Some(message)),
                Err(error)
                    if error.is_not_found() || error.code() == Some("ErrorInvalidIdMalformed") =>
                {
                    Ok(None)
                }
                Err(error) => Err(super::graph::failed(error)),
            })
            .collect()
    }
}

/// Parses the stored `ARRIVALS` watermark: either the current integer format or the legacy
/// per-folder JSON map.
fn parse_watermark(raw: &str) -> Option<i64> {
    raw.parse::<i64>().ok().or_else(|| {
        serde_json::from_str::<BTreeMap<String, i64>>(raw)
            .ok()
            .map(|map| map.values().copied().max().unwrap_or_default())
    })
}

/// Whether Graph no longer knows a delta's link.
fn is_expired(error: &outlook::Error) -> bool {
    error.status() == Some(410) || error.code() == Some("SyncStateNotFound")
}

fn expired(failure: &Failure) -> bool {
    matches!(failure, Failure::Permanent(ProviderError::Outlook(error)) if is_expired(error))
}
