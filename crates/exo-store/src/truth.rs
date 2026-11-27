//! Writes of provider truth, meaning what the provider says threads and messages are. The outbox
//! overlay is never written here. It's applied on read and when list membership is recomputed.
//!
//! Label writes only move forward in version (Gmail's `historyId`), so history, backfill and the
//! outbox can store what they fetched in any order and the newest copy wins.

use std::collections::BTreeSet;

use toasty::Executor;

use crate::db::{
    AttachmentRow, BodyRow, ListEntry, MessageRow, ParticipantEntry, ReminderRow, TextDue,
    ThreadRow,
};
use crate::error::Result;
use crate::mutation::Step;
use crate::provider::{Fetched, Parsed};
use crate::view::ThreadView;

/// Stores a thread fetched for its rows, unless the local copy is newer. The messages it holds
/// in full are stored whole, and the rest as rows alone. Messages the provider no longer lists
/// are deleted. Returns the thread id when something was written.
pub(crate) async fn save_rows(exec: &mut dyn Executor, thread: Fetched) -> Result<Option<String>> {
    let Fetched {
        id,
        version,
        messages,
    } = thread;
    if let Some(local) = ThreadRow::filter_by_id(&id).first().exec(exec).await?
        && local.version > version
    {
        return Ok(None);
    }
    let existing = MessageRow::filter_by_thread_id(&id).exec(exec).await?;
    let gone: Vec<String> = existing
        .iter()
        .filter(|row| !messages.iter().any(|parsed| parsed.message.id == row.id))
        .map(|row| row.id.clone())
        .collect();
    delete_messages(exec, &gone).await?;
    for parsed in messages {
        let stored = existing.iter().any(|row| row.id == parsed.message.id);
        upsert_message(exec, parsed, stored).await?;
    }
    ThreadRow::upsert_by_id(&id)
        .version(version)
        .exec(exec)
        .await?;
    Ok(Some(id))
}

/// Adds what a row alone lacks (body, attachments, every header) to the stored messages among
/// `messages` (fetched in full) that lack it. Their labels move forward as usual. Nothing else
/// changes, so a copy older than what history has stored since can't undo history's writes.
pub(crate) async fn save_bodies(exec: &mut dyn Executor, messages: Vec<Parsed>) -> Result<()> {
    for parsed in messages {
        if MessageRow::filter_by_id(&parsed.message.id)
            .first()
            .exec(exec)
            .await?
            .is_some()
        {
            upsert_message(exec, parsed, true).await?;
        }
    }
    Ok(())
}

/// Stores one message fetched in full (a history addition).
pub(crate) async fn save_message(exec: &mut dyn Executor, parsed: Parsed) -> Result<()> {
    let (thread, version) = (parsed.message.thread_id.clone(), parsed.message.version);
    let stored = MessageRow::filter_by_id(&parsed.message.id)
        .first()
        .exec(exec)
        .await?
        .is_some();
    upsert_message(exec, parsed, stored).await?;
    bump_thread(exec, &thread, version).await
}

/// Raises a thread's stored version to `version`.
pub(crate) async fn bump_thread(exec: &mut dyn Executor, id: &str, version: u64) -> Result<()> {
    let current = ThreadRow::filter_by_id(id).first().exec(exec).await?;
    if current.is_none_or(|thread| thread.version < version) {
        ThreadRow::upsert_by_id(id)
            .version(version)
            .exec(exec)
            .await?;
    }
    Ok(())
}

/// Replaces a message's labels with what the provider reported at `version`, unless the stored
/// copy is at least that new.
pub(crate) async fn set_labels(
    exec: &mut dyn Executor,
    id: &str,
    labels: &[String],
    version: u64,
) -> Result<()> {
    let fields = MessageRow::fields();
    MessageRow::filter(fields.id().eq(id).and(fields.version().lt(version)))
        .update()
        .label_ids(labels.to_vec())
        .version(version)
        .exec(exec)
        .await?;
    Ok(())
}

/// Applies a step Gmail just acknowledged to the stored labels, so the overlay can drop it
/// without the thread flickering back until the next history sync.
pub(crate) async fn apply_step(exec: &mut dyn Executor, thread: &str, step: &Step) -> Result<()> {
    let rows = MessageRow::filter_by_thread_id(thread).exec(exec).await?;
    // Which drafts are Exo's doesn't matter here, because every message's labels are written
    // back.
    let mut view = ThreadView::new(thread.to_owned(), rows, &[], &BTreeSet::new());
    view.apply(step);
    for message in view.all_messages() {
        let labels: Vec<String> = message.labels.iter().cloned().collect();
        let mut stored = message.row.label_ids.clone();
        stored.sort();
        if stored != labels {
            MessageRow::update_by_id(&message.row.id)
                .label_ids(labels)
                .exec(exec)
                .await?;
        }
    }
    Ok(())
}

/// Inserts a message, or, when it's `stored` already, refreshes its labels (the only part of a
/// Gmail message that changes) and adds what a row alone lacks when `parsed` has it.
async fn upsert_message(exec: &mut dyn Executor, parsed: Parsed, stored: bool) -> Result<()> {
    let Parsed {
        message,
        attachments,
        body,
    } = parsed;
    if stored {
        if let Some(body) = body
            && BodyRow::filter_by_message_id(&message.id)
                .first()
                .exec(exec)
                .await?
                .is_none()
        {
            // Write every header, not just the row's. The rest of the row is the same.
            MessageRow::update_by_id(&message.id)
                .headers(message.headers)
                .has_attachments(message.has_attachments)
                .exec(exec)
                .await?;
            // A message whose body was evicted kept its attachment rows. They're replaced,
            // because the new rows have Gmail's ids now.
            AttachmentRow::filter(AttachmentRow::fields().message_id().eq(&message.id))
                .delete()
                .exec(exec)
                .await?;
            insert_parts(exec, &message.thread_id, attachments, body).await?;
        }
        return set_labels(exec, &message.id, &message.label_ids, message.version).await;
    }
    let thread = message.thread_id.clone();
    MessageRow::create()
        .id(message.id)
        .thread_id(message.thread_id)
        .version(message.version)
        .internal_date(message.internal_date)
        .label_ids(message.label_ids)
        .subject(message.subject)
        .snippet(message.snippet)
        .sender(message.sender)
        .sender_raw(message.sender_raw)
        .to(message.to)
        .cc(message.cc)
        .bcc(message.bcc)
        .reply_to(message.reply_to)
        .rfc822_id(message.rfc822_id)
        .in_reply_to(message.in_reply_to)
        .references(message.references)
        .list_id(message.list_id)
        .list_unsubscribe(message.list_unsubscribe)
        .list_unsubscribe_post(message.list_unsubscribe_post)
        .headers(message.headers)
        .size(message.size)
        .has_attachments(message.has_attachments)
        .exec(exec)
        .await?;
    match body {
        Some(body) => insert_parts(exec, &thread, attachments, body).await,
        None => Ok(()),
    }
}

/// Inserts a message's attachment rows and body, which queues its conversation `thread` for
/// working out the body's visible text (`crate::text`).
async fn insert_parts(
    exec: &mut dyn Executor,
    thread: &str,
    attachments: Vec<AttachmentRow>,
    body: BodyRow,
) -> Result<()> {
    if !attachments.is_empty() {
        let mut rows = AttachmentRow::create_many();
        for attachment in attachments {
            rows = rows.with_item(|row| {
                row.message_id(attachment.message_id)
                    .part_id(attachment.part_id)
                    .attachment_id(attachment.attachment_id)
                    .filename(attachment.filename)
                    .mime_type(attachment.mime_type)
                    .size(attachment.size)
                    .content_id(attachment.content_id)
                    .inline(attachment.inline)
            });
        }
        rows.exec(exec).await?;
    }
    BodyRow::create()
        .message_id(body.message_id)
        .html(body.html)
        .text(body.text)
        .visible(body.visible)
        .exec(exec)
        .await?;
    crate::text::queue(exec, thread).await
}

/// Deletes messages with their attachments and bodies.
pub(crate) async fn delete_messages(exec: &mut dyn Executor, ids: &[String]) -> Result<()> {
    for chunk in ids.chunks(crate::db::IN_LIST_CHUNK) {
        MessageRow::filter(MessageRow::fields().id().in_list(chunk))
            .delete()
            .exec(exec)
            .await?;
        AttachmentRow::filter(AttachmentRow::fields().message_id().in_list(chunk))
            .delete()
            .exec(exec)
            .await?;
        BodyRow::filter(BodyRow::fields().message_id().in_list(chunk))
            .delete()
            .exec(exec)
            .await?;
    }
    Ok(())
}

/// Deletes a thread Gmail no longer has.
pub(crate) async fn delete_thread(exec: &mut dyn Executor, id: &str) -> Result<()> {
    let ids: Vec<String> = MessageRow::filter_by_thread_id(id)
        .exec(exec)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect();
    delete_messages(exec, &ids).await?;
    ThreadRow::filter_by_id(id).delete().exec(exec).await?;
    ListEntry::filter_by_thread_id(id)
        .delete()
        .exec(exec)
        .await?;
    ParticipantEntry::filter_by_thread_id(id)
        .delete()
        .exec(exec)
        .await?;
    TextDue::filter_by_thread_id(id).delete().exec(exec).await?;
    crate::search::unindex(exec, id).await?;
    // Gone from Gmail, it has nothing to come back to.
    ReminderRow::filter_by_thread_id(id)
        .delete()
        .exec(exec)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[tokio::test]
    async fn delete_thread_unindexes_participants_text_due_and_fts() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-delete-thread-{}", std::process::id()));
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
                headers: vec![
                    gmail::MessagePartHeader {
                        name: "Subject".into(),
                        value: "Zeppelin".into(),
                    },
                    gmail::MessagePartHeader {
                        name: "From".into(),
                        value: "Ada <ada@example.com>".into(),
                    },
                ],
                body: Some(gmail::MessagePartBody {
                    data: Some(b"uniquezeppelinbody".to_vec()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            let parsed = crate::provider::gmail::parse::parse(&msg, true).expect("parses");
            save_message(write.exec(), parsed).await.expect("saves");
            crate::store::reindex(write.exec(), "t1", &store.inner.router())
                .await
                .expect("indexes");
            write.commit().await.expect("commits");
        }
        assert_eq!(
            store
                .recent_threads("ada@example.com", 10)
                .await
                .expect("recent")
                .len(),
            1
        );
        assert_eq!(
            TextDue::filter_by_thread_id("t1")
                .exec(&mut db)
                .await
                .expect("due")
                .len(),
            1
        );

        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            delete_thread(write.exec(), "t1").await.expect("deletes");
            write.commit().await.expect("commits");
        }
        assert!(
            store
                .recent_threads("ada@example.com", 10)
                .await
                .expect("recent")
                .is_empty()
        );
        assert!(
            TextDue::filter_by_thread_id("t1")
                .exec(&mut db)
                .await
                .expect("due")
                .is_empty()
        );
        let mut s = crate::Search::new("uniquezeppelinbody");
        assert!(
            store
                .search(&mut s, 10, false, 10)
                .await
                .expect("search")
                .results
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn save_rows_keeps_the_newest_copy_and_drops_what_the_thread_lost() {
        let dir = std::env::temp_dir().join(format!("exo-store-save-rows-{}", std::process::id()));
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
        let message = |id: &str, version: u64, labels: &[&str], full: bool| {
            let message = gmail::Message {
                id: Some(id.into()),
                thread_id: Some("t1".into()),
                history_id: Some(version),
                label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
                payload: Some(gmail::MessagePart {
                    mime_type: Some("text/plain".into()),
                    body: Some(gmail::MessagePartBody {
                        data: Some(b"body".to_vec()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            crate::provider::gmail::parse::parse(&message, full).expect("parses")
        };
        let thread = |version: u64, messages: Vec<Parsed>| Fetched {
            id: "t1".into(),
            version,
            messages,
        };
        let save = |fetched: Fetched| {
            let store = store.clone();
            async move {
                let mut db = store.inner.db.clone();
                let mut write = store.inner.begin(&mut db).await.expect("tx");
                let saved = save_rows(write.exec(), fetched).await.expect("saves");
                write.commit().await.expect("commits");
                saved
            }
        };
        let labels = |db: &mut toasty::Db| {
            let mut db = db.clone();
            async move {
                let mut rows: Vec<(String, Vec<String>)> = MessageRow::filter_by_thread_id("t1")
                    .exec(&mut db)
                    .await
                    .expect("reads")
                    .into_iter()
                    .map(|row| (row.id, row.label_ids))
                    .collect();
                rows.sort();
                rows
            }
        };

        let saved = save(thread(
            10,
            vec![
                message("m1", 10, &["INBOX"], true),
                message("m2", 10, &["INBOX"], false),
            ],
        ))
        .await;
        assert_eq!(saved.as_deref(), Some("t1"));
        let bodies = BodyRow::all().exec(&mut db).await.expect("bodies");
        assert_eq!(
            bodies
                .iter()
                .map(|body| body.message_id.as_str())
                .collect::<Vec<_>>(),
            ["m1"],
            "only what came in full has a body"
        );

        // An older copy changes nothing.
        assert_eq!(
            save(thread(9, vec![message("m1", 9, &["TRASH"], false)])).await,
            None
        );
        assert_eq!(
            labels(&mut db).await,
            [
                ("m1".to_owned(), vec!["INBOX".to_owned()]),
                ("m2".to_owned(), vec!["INBOX".to_owned()])
            ]
        );

        // A newer one moves the labels on and drops the message the thread lost.
        save(thread(11, vec![message("m1", 11, &["STARRED"], false)])).await;
        assert_eq!(
            labels(&mut db).await,
            [("m1".to_owned(), vec!["STARRED".to_owned()])]
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
