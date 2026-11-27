//! What changed, from Gmail's history (`history.list`). Every page is read first, and the changes
//! are grouped per thread.

use std::collections::{BTreeMap, BTreeSet};

use gmail::{History, HistoryQuery};

use super::{Gmail, cursors_at, history_id};
use crate::error::Result;
use crate::ids::labels::CHAT;
use crate::provider::{Changes, Cursors, ThreadChanges};
use crate::quota::Lane;
use crate::sync::ATTEMPTS;

const PAGE_SIZE: u32 = 500;
/// More than this many pages or changes means the cache is too far behind to patch, so sync
/// starts over.
const MAX_PAGES: usize = 100;
const MAX_CHANGES: usize = 10_000;

impl Gmail {
    /// Gmail's history since `since`, or `None` when it can't be used (expired, or too long to
    /// patch the cache with).
    pub(super) async fn changes(&self, since: &Cursors) -> Result<Option<Changes>> {
        let Some(start) = history_id(since) else {
            return Ok(None);
        };
        let mut records: Vec<History> = Vec::new();
        let mut response_history_id: Option<u64> = None;
        let (mut page_token, mut pages, mut changes) = (None, 0, 0);
        loop {
            let query = HistoryQuery {
                start_history_id: start,
                max_results: Some(PAGE_SIZE),
                page_token: page_token.take(),
                ..HistoryQuery::default()
            };
            let page = match self
                .api
                .call(Lane::Sync, ATTEMPTS, |gmail| gmail.history(&query))
                .await
            {
                Ok(page) => page,
                Err(failure) if failure.is_not_found() => {
                    // The cursor expired, unless it already is the mailbox's current id.
                    let profile = self
                        .api
                        .call(Lane::Sync, ATTEMPTS, |gmail| gmail.profile())
                        .await?;
                    if profile.history_id == Some(start) {
                        log::info!("history.list 404 at the current history id {start}");
                        return Ok(Some(unchanged(since)));
                    }
                    return Ok(None);
                }
                Err(failure) => return Err(failure.into()),
            };
            response_history_id = response_history_id.max(page.history_id);
            pages += 1;
            changes += page
                .history
                .iter()
                .map(|record| {
                    record.messages_added.len()
                        + record.messages_deleted.len()
                        + record.labels_added.len()
                        + record.labels_removed.len()
                })
                .sum::<usize>();
            records.extend(page.history);
            if pages > MAX_PAGES || changes > MAX_CHANGES {
                log::warn!("{pages} pages / {changes} changes of history: rebuilding instead");
                return Ok(None);
            }
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        Ok(Some(collect(since, start, records, response_history_id)))
    }
}

/// No changes, and the cursors where they are.
fn unchanged(since: &Cursors) -> Changes {
    Changes {
        next: since.clone(),
        ..Changes::default()
    }
}

/// The changes in `records`, read from `start` (the history id `since` holds), with the mailbox
/// at `response_history_id` when Gmail said so. The cursors move to the newest of the two, and
/// never back.
fn collect(
    since: &Cursors,
    start: u64,
    records: Vec<History>,
    response_history_id: Option<u64>,
) -> Changes {
    let Some(record_max) = records.iter().filter_map(|record| record.id).max() else {
        return match response_history_id.filter(|&id| id > start) {
            Some(latest) => Changes {
                next: cursors_at(latest),
                ..Changes::default()
            },
            None => unchanged(since),
        };
    };
    let next = cursors_at(response_history_id.map_or(record_max, |id| id.max(record_max)));
    let labels: BTreeSet<String> = records
        .iter()
        .flat_map(|record| {
            let added = record.messages_added.iter().map(|added| &added.message);
            let changed = record.labels_added.iter().map(|change| &change.message);
            let removed = record.labels_removed.iter().map(|change| &change.message);
            added.chain(changed).chain(removed)
        })
        .flat_map(|message| message.label_ids.iter().cloned())
        .collect();
    Changes {
        threads: group(records),
        labels,
        next,
        ..Changes::default()
    }
}

/// Groups history records by thread. Chat messages are skipped. Drafts are kept, since Exo
/// shows Gmail drafts.
fn group(records: Vec<History>) -> BTreeMap<String, ThreadChanges> {
    let mut threads: BTreeMap<String, ThreadChanges> = BTreeMap::new();
    for record in records {
        let id = record.id.unwrap_or_default();
        for added in &record.messages_added {
            if added.message.label_ids.iter().any(|label| label == CHAT) {
                continue;
            }
            if let Some((thread, message)) = touch(&mut threads, id, &added.message) {
                thread.added.insert(message);
            }
        }
        for deleted in &record.messages_deleted {
            if let Some((thread, message)) = touch(&mut threads, id, &deleted.message) {
                thread.deleted.insert(message);
            }
        }
        let changed = record
            .labels_added
            .iter()
            .map(|change| &change.message)
            .chain(record.labels_removed.iter().map(|change| &change.message));
        for message in changed {
            let labels = message.label_ids.clone();
            if let Some((thread, message)) = touch(&mut threads, id, message) {
                let entry = thread.labels.entry(message).or_insert((0, Vec::new()));
                if id >= entry.0 {
                    *entry = (id, labels);
                }
            }
        }
    }
    threads
}

/// The thread a record touches (bumping its newest record id), and the message id.
fn touch<'a>(
    threads: &'a mut BTreeMap<String, ThreadChanges>,
    record: u64,
    message: &gmail::Message,
) -> Option<(&'a mut ThreadChanges, String)> {
    let thread = threads.entry(message.thread_id.clone()?).or_default();
    thread.latest = thread.latest.max(record);
    thread.changes += 1;
    Some((thread, message.id.clone()?))
}

#[cfg(test)]
mod tests {
    use gmail::{HistoryLabelAdded, HistoryMessageAdded, HistoryMessageDeleted, Message};

    use super::*;

    fn message(id: &str, thread: &str, labels: &[&str]) -> Message {
        Message {
            id: Some(id.into()),
            thread_id: Some(thread.into()),
            label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
            ..Message::default()
        }
    }

    #[test]
    fn no_records_move_the_cursor_only_forward() {
        let since = cursors_at(500);
        assert_eq!(collect(&since, 500, Vec::new(), Some(450)).next, since);
        assert_eq!(collect(&since, 500, Vec::new(), Some(500)).next, since);
        assert_eq!(collect(&since, 500, Vec::new(), None).next, since);
        let ahead = collect(&since, 500, Vec::new(), Some(950));
        assert_eq!(ahead.next, cursors_at(950));
        assert!(ahead.threads.is_empty() && ahead.labels.is_empty());
    }

    #[test]
    fn records_group_by_thread_and_the_cursor_takes_the_newest_id() {
        let records = vec![
            History {
                id: Some(601),
                messages_added: vec![
                    HistoryMessageAdded {
                        message: message("m1", "t1", &["INBOX", "UNREAD"]),
                    },
                    HistoryMessageAdded {
                        message: message("c1", "t2", &["CHAT"]),
                    },
                ],
                ..History::default()
            },
            History {
                id: Some(602),
                labels_added: vec![HistoryLabelAdded {
                    message: message("m1", "t1", &["INBOX", "UNREAD", "Label_7"]),
                    label_ids: vec!["Label_7".into()],
                }],
                messages_deleted: vec![HistoryMessageDeleted {
                    message: message("m0", "t3", &[]),
                }],
                ..History::default()
            },
        ];
        let changes = collect(&cursors_at(500), 500, records, Some(600));
        assert_eq!(changes.next, cursors_at(602));
        assert_eq!(
            changes
                .labels
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["CHAT", "INBOX", "Label_7", "UNREAD"],
            "every label the records name, a chat's too"
        );
        assert_eq!(
            changes
                .threads
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["t1", "t3"],
            "chats added are skipped"
        );
        let t1 = &changes.threads["t1"];
        assert_eq!(t1.added.iter().collect::<Vec<_>>(), ["m1"]);
        assert_eq!(
            t1.labels["m1"],
            (602, vec!["INBOX".into(), "UNREAD".into(), "Label_7".into()])
        );
        assert_eq!((t1.latest, t1.changes), (602, 2));
        assert_eq!(
            changes.threads["t3"].deleted.iter().collect::<Vec<_>>(),
            ["m0"]
        );
    }
}
