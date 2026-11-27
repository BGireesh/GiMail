//! End-to-end reminders on Outlook: snoozing into Archive with `build.miguel.exo.snoozed`,
//! returning due reminders to Inbox with `build.miguel.exo.reminderUnread`, `MarkRead` and
//! `MarkNotDone`, cancelling and undoing, reply-abort, sending with a reminder, `clear_cache`
//! persistence, and recovering orphaned snoozes from Graph open extensions on a fresh store.

use crate::support;

use std::collections::BTreeSet;

use exo_store::{
    Address, Draft, DraftAction, DraftRecord, DraftReminder, ListId, Mutation, ReminderStatus,
    Remote, Store, ThreadId, ThreadSummary,
};
use jiff::{Timestamp, ToSpan};
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::well_known;
use serde_json::json;
use support::{TempDir, unpaced};

const ACCOUNT: &str = "microsoft maya@brightmoor.co";
const ADDRESS: &str = "maya@brightmoor.co";
const SNOOZED_EXT: &str = "build.miguel.exo.snoozed";
const REMINDER_UNREAD_EXT: &str = "build.miguel.exo.reminderUnread";

struct OutlookHarness {
    server: MockServer,
    dir: TempDir,
    store: Store,
    graph: outlook::Client,
}

impl OutlookHarness {
    async fn synced() -> Self {
        let server = MockServer::start(Config::default())
            .await
            .expect("mock starts");
        let dir = TempDir::new();
        let store = open_store(&dir, &server).await;
        sync_all(&store).await;
        let graph = outlook::Client::new(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        );
        Self {
            server,
            dir,
            store,
            graph,
        }
    }

    async fn inbox_threads(&self) -> Vec<ThreadSummary> {
        self.store
            .list(&ListId::Inbox, None, 200)
            .await
            .expect("lists inbox")
            .threads
            .into_iter()
            .filter(|thread| thread.drafts == 0)
            .collect()
    }

    async fn ids(&self, list: &ListId) -> BTreeSet<String> {
        self.store
            .list(list, None, 200)
            .await
            .expect("lists")
            .threads
            .into_iter()
            .map(|row| row.id.as_str().to_owned())
            .collect()
    }

    async fn summary(&self, list: &ListId, id: &ThreadId) -> Option<ThreadSummary> {
        self.store
            .list(list, None, 200)
            .await
            .expect("lists")
            .threads
            .into_iter()
            .find(|row| row.id == *id)
    }

    async fn flush(&self) {
        self.store.flush().await.expect("flushes");
        assert_eq!(
            self.store.pending().await.expect("pending"),
            0,
            "Outlook took every queued change"
        );
    }

    /// Folder id for well-known folder `name`.
    async fn folder_id(&self, name: &str) -> String {
        self.graph
            .mail()
            .folder(name)
            .select("id")
            .await
            .expect("folder")
            .id
            .expect("folder id")
    }

    /// Conversation `id`'s messages as Graph currently has them, with open extensions expanded.
    async fn graph_conversation(&self, id: &ThreadId) -> Vec<outlook::Message> {
        let expand = format!(
            "extensions($filter=id eq '{}' or id eq '{}')",
            SNOOZED_EXT, REMINDER_UNREAD_EXT
        );
        self.graph
            .mail()
            .conversation(id.as_str())
            .select("id,conversationId,parentFolderId,isRead")
            .expand(&expand)
            .top(100)
            .all()
            .await
            .expect("lists conversation")
            .value
    }

    /// Extension short names present across any message in conversation `id`.
    async fn graph_extensions(&self, id: &ThreadId) -> BTreeSet<String> {
        self.graph_conversation(id)
            .await
            .into_iter()
            .flat_map(|message| message.extensions)
            .filter_map(|ext| {
                let raw = ext.extension_name.or(ext.id)?;
                Some(
                    raw.rsplit_once(".OpenTypeExtension.")
                        .or_else(|| raw.rsplit_once(".openTypeExtension."))
                        .map_or(raw.as_str(), |(_, short)| short)
                        .to_owned(),
                )
            })
            .collect()
    }

    /// Delivers a new message via the mock's delivery endpoint.
    async fn deliver(&self, body: serde_json::Value) {
        let url = self
            .server
            .url()
            .join("/__mock/deliver")
            .expect("valid url");
        let response = reqwest::Client::new()
            .post(url)
            .json(&body)
            .send()
            .await
            .expect("deliver request");
        assert!(
            response.status().is_success(),
            "deliver failed: {}",
            response.status()
        );
    }
}

async fn open_store(dir: &TempDir, server: &MockServer) -> Store {
    Store::open_with(
        dir.path(),
        ACCOUNT,
        Remote::outlook(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        unpaced(),
    )
    .await
    .expect("store opens")
}

async fn sync_all(store: &Store) {
    for _ in 0..80 {
        if !store.sync().await.expect("sync").backfilling {
            return;
        }
    }
    panic!("sync did not finish backfilling");
}

fn from_now(span: jiff::Span) -> Timestamp {
    Timestamp::now()
        .checked_add(span)
        .unwrap()
        .round(jiff::Unit::Second)
        .unwrap()
}

fn snooze_until(at: Timestamp, keep_on_reply: bool) -> Mutation {
    Mutation::Remind {
        at,
        keep_on_reply,
        snooze: true,
    }
}

#[tokio::test]
async fn snoozed_thread_moves_to_archive_with_extension_and_returns_unread_when_due() {
    let outlook = OutlookHarness::synced().await;
    let id = outlook.inbox_threads().await[0].id.clone();
    let inbox_id = outlook.folder_id(well_known::INBOX).await;
    let archive_id = outlook.folder_id(well_known::ARCHIVE).await;
    let at = from_now(2.hours());

    outlook
        .store
        .apply(std::slice::from_ref(&id), snooze_until(at, false))
        .await
        .expect("sets reminder");
    assert!(!outlook.ids(&ListId::Inbox).await.contains(id.as_str()));
    let waiting = outlook
        .summary(&ListId::Reminders, &id)
        .await
        .expect("in Reminders");
    assert_eq!(
        waiting.reminder,
        Some(ReminderStatus::Set {
            at,
            keep_on_reply: false
        })
    );
    assert_eq!(outlook.store.next_reminder().await.unwrap(), Some(at));

    outlook.flush().await;
    let messages = outlook.graph_conversation(&id).await;
    assert!(!messages.is_empty());
    assert!(
        messages
            .iter()
            .all(|m| m.parent_folder_id.as_deref() != Some(inbox_id.as_str())),
        "left Inbox on Graph"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.parent_folder_id.as_deref() == Some(archive_id.as_str())),
        "moved to Archive on Graph"
    );
    let exts = outlook.graph_extensions(&id).await;
    assert!(
        exts.contains(SNOOZED_EXT) && !exts.contains(REMINDER_UNREAD_EXT),
        "snoozed marker set on Graph: {exts:?}"
    );

    // Not due yet.
    assert!(
        outlook
            .store
            .return_reminders(at.checked_sub(1.second()).unwrap())
            .await
            .unwrap()
            .is_empty()
    );

    // Due now: returns to Inbox with reminder-unread marker.
    let back = at.checked_add(5.minutes()).unwrap();
    assert_eq!(
        outlook.store.return_reminders(back).await.unwrap(),
        std::slice::from_ref(&id)
    );
    assert!(
        outlook
            .store
            .return_reminders(back)
            .await
            .unwrap()
            .is_empty(),
        "returned once"
    );
    assert!(!outlook.ids(&ListId::Reminders).await.contains(id.as_str()));
    let returned = outlook
        .summary(&ListId::Inbox, &id)
        .await
        .expect("back in Inbox");
    assert_eq!(
        returned.reminder,
        Some(ReminderStatus::Returned {
            at: back,
            unread: true
        })
    );
    assert!(returned.unread);

    outlook.flush().await;
    let messages = outlook.graph_conversation(&id).await;
    assert!(
        messages
            .iter()
            .any(|m| m.parent_folder_id.as_deref() == Some(inbox_id.as_str())),
        "moved back to Inbox on Graph"
    );
    let exts = outlook.graph_extensions(&id).await;
    assert!(
        exts.contains(REMINDER_UNREAD_EXT) && !exts.contains(SNOOZED_EXT),
        "reminderUnread set and snoozed cleared on Graph: {exts:?}"
    );

    // Marking read clears the reminderUnread extension both locally and on Graph.
    outlook
        .store
        .apply(std::slice::from_ref(&id), Mutation::MarkRead)
        .await
        .expect("marks read");
    let read = outlook.summary(&ListId::Inbox, &id).await.unwrap();
    assert_eq!(
        (read.reminder, read.unread),
        (
            Some(ReminderStatus::Returned {
                at: back,
                unread: false
            }),
            false
        )
    );
    outlook.flush().await;
    let exts = outlook.graph_extensions(&id).await;
    assert!(
        !exts.contains(REMINDER_UNREAD_EXT),
        "reminderUnread cleared on Graph: {exts:?}"
    );
}

#[tokio::test]
async fn mark_not_done_works_on_outlook_without_a_mute_label() {
    let outlook = OutlookHarness::synced().await;
    let id = outlook.inbox_threads().await[0].id.clone();
    let inbox_id = outlook.folder_id(well_known::INBOX).await;

    outlook
        .store
        .apply(std::slice::from_ref(&id), Mutation::MarkDone)
        .await
        .expect("marks done");
    outlook.flush().await;
    assert!(!outlook.ids(&ListId::Inbox).await.contains(id.as_str()));

    let undo = outlook
        .store
        .apply(std::slice::from_ref(&id), Mutation::MarkNotDone)
        .await
        .expect("MarkNotDone works on Outlook");
    assert!(outlook.ids(&ListId::Inbox).await.contains(id.as_str()));
    outlook.flush().await;
    assert!(
        outlook
            .graph_conversation(&id)
            .await
            .iter()
            .any(|m| m.parent_folder_id.as_deref() == Some(inbox_id.as_str())),
        "moved back to Inbox on Graph"
    );

    outlook.store.undo(undo).await.expect("undoes MarkNotDone");
    outlook.flush().await;
    assert!(!outlook.ids(&ListId::Inbox).await.contains(id.as_str()));
}

#[tokio::test]
async fn cancel_and_undo_reminder_manage_outlook_extensions_and_folders() {
    let outlook = OutlookHarness::synced().await;
    let id = outlook.inbox_threads().await[0].id.clone();
    let inbox_id = outlook.folder_id(well_known::INBOX).await;
    let archive_id = outlook.folder_id(well_known::ARCHIVE).await;
    let at = from_now(24.hours());

    outlook
        .store
        .apply(std::slice::from_ref(&id), snooze_until(at, false))
        .await
        .expect("snoozes");
    outlook.flush().await;
    assert!(outlook.graph_extensions(&id).await.contains(SNOOZED_EXT));

    let undo = outlook
        .store
        .apply(
            std::slice::from_ref(&id),
            Mutation::CancelReminder { to_inbox: true },
        )
        .await
        .expect("cancels");
    assert!(outlook.ids(&ListId::Inbox).await.contains(id.as_str()));
    assert!(!outlook.ids(&ListId::Reminders).await.contains(id.as_str()));
    outlook.flush().await;
    assert!(
        !outlook.graph_extensions(&id).await.contains(SNOOZED_EXT),
        "snoozed extension removed on cancel"
    );
    assert!(
        outlook
            .graph_conversation(&id)
            .await
            .iter()
            .any(|m| m.parent_folder_id.as_deref() == Some(inbox_id.as_str())),
        "returned to Inbox on cancel(to_inbox=true)"
    );

    // Undo restores both Archive placement and snoozed extension.
    outlook.store.undo(undo).await.expect("undoes cancel");
    outlook.flush().await;
    assert!(outlook.ids(&ListId::Reminders).await.contains(id.as_str()));
    assert!(outlook.graph_extensions(&id).await.contains(SNOOZED_EXT));
    assert!(
        outlook
            .graph_conversation(&id)
            .await
            .iter()
            .any(|m| m.parent_folder_id.as_deref() == Some(archive_id.as_str())),
        "back in Archive after undo"
    );
}

#[tokio::test]
async fn reply_aborts_if_no_reply_reminder_and_keeps_regardless_on_outlook() {
    let outlook = OutlookHarness::synced().await;
    let threads = outlook.inbox_threads().await;
    let (if_no_reply, regardless) = (threads[0].id.clone(), threads[1].id.clone());
    let at = from_now(24.hours());

    outlook
        .store
        .apply(std::slice::from_ref(&if_no_reply), snooze_until(at, false))
        .await
        .unwrap();
    outlook
        .store
        .apply(std::slice::from_ref(&regardless), snooze_until(at, true))
        .await
        .unwrap();
    outlook.flush().await;

    // Deliver a reply into each conversation via RFC 822 `In-Reply-To`/`References`.
    for (idx, thread) in [&if_no_reply, &regardless].into_iter().enumerate() {
        let detail = outlook
            .store
            .thread(thread)
            .await
            .unwrap()
            .expect("thread detail");
        let last = detail.messages.last().expect("message");
        let rfc822 = last
            .rfc822_id
            .as_deref()
            .expect("message carries rfc822_id");
        let reply = gmail::mime::Email {
            from: Some(gmail::mime::Mailbox::new("Ana Ruiz", "ana@example.com")),
            to: vec![gmail::mime::Mailbox::new("Maya Okafor", ADDRESS)],
            subject: format!("Re: {}", detail.subject),
            text: Some("Got an update for you!".into()),
            message_id: Some(format!("reply-{idx}@example.com")),
            in_reply_to: Some(rfc822.to_owned()),
            references: vec![rfc822.to_owned()],
            ..gmail::mime::Email::default()
        };
        outlook
            .deliver(json!({
                "raw": gmail::wire::encode_base64url(&reply.to_rfc822()),
                "labelIds": ["INBOX", "UNREAD", "CATEGORY_PERSONAL"],
            }))
            .await;
    }
    sync_all(&outlook.store).await;

    let aborted = outlook
        .summary(&ListId::Inbox, &if_no_reply)
        .await
        .expect("reply brought thread back to Inbox");
    let Some(ReminderStatus::Aborted { by }) = aborted.reminder else {
        panic!("expected aborted reminder, got {:?}", aborted.reminder);
    };
    assert_eq!(by.email, "ana@example.com");
    assert!(
        !outlook
            .ids(&ListId::Reminders)
            .await
            .contains(if_no_reply.as_str())
    );
    assert_eq!(
        outlook
            .summary(&ListId::Reminders, &regardless)
            .await
            .expect("regardless reminder stays set")
            .reminder,
        Some(ReminderStatus::Set {
            at,
            keep_on_reply: true
        })
    );

    // `return_reminders` strips the snoozed extension from the aborted thread while keeping the
    // regardless thread waiting.
    assert!(
        outlook
            .store
            .return_reminders(Timestamp::now())
            .await
            .unwrap()
            .is_empty()
    );
    outlook.flush().await;
    assert!(
        !outlook
            .graph_extensions(&if_no_reply)
            .await
            .contains(SNOOZED_EXT),
        "aborted reminder's extension removed"
    );
    assert!(
        outlook
            .graph_extensions(&regardless)
            .await
            .contains(SNOOZED_EXT),
        "regardless reminder keeps its extension"
    );
    assert_eq!(
        outlook.store.return_reminders(at).await.unwrap(),
        [regardless]
    );
}

#[tokio::test]
async fn clear_cache_keeps_reminders_and_fresh_store_recovers_orphaned_snoozes_from_markers() {
    let mut outlook = OutlookHarness::synced().await;
    let id = outlook.inbox_threads().await[0].id.clone();
    let inbox_id = outlook.folder_id(well_known::INBOX).await;
    let at = from_now(12.hours());

    outlook
        .store
        .apply(std::slice::from_ref(&id), snooze_until(at, false))
        .await
        .unwrap();
    outlook.flush().await;

    // 1. Clearing the local mail cache keeps reminders; syncing restores the snoozed thread.
    let old = std::mem::replace(
        &mut outlook.store,
        open_store(&TempDir::new(), &outlook.server).await,
    );
    drop(old);
    Store::clear_cache(outlook.dir.path(), ACCOUNT)
        .await
        .expect("clears cache");
    outlook.store = open_store(&outlook.dir, &outlook.server).await;
    sync_all(&outlook.store).await;
    assert_eq!(outlook.store.next_reminder().await.unwrap(), Some(at));
    assert_eq!(
        outlook
            .summary(&ListId::Reminders, &id)
            .await
            .expect("still in Reminders after clear_cache")
            .reminder,
        Some(ReminderStatus::Set {
            at,
            keep_on_reply: false
        })
    );

    // 2. A fresh store (no local `reminders` table) discovers the `build.miguel.exo.snoozed`
    // marker on Graph and returns the orphaned snooze to Inbox.
    let fresh_dir = TempDir::new();
    let fresh = open_store(&fresh_dir, &outlook.server).await;
    sync_all(&fresh).await;
    let recovered = fresh
        .return_orphaned_snoozes()
        .await
        .expect("recovers orphaned snoozes");
    assert_eq!(recovered.as_slice(), std::slice::from_ref(&id));
    assert!(
        fresh
            .return_orphaned_snoozes()
            .await
            .expect("runs once")
            .is_empty()
    );
    fresh.flush().await.expect("flushes recovery");
    assert_eq!(fresh.pending().await.unwrap(), 0);
    assert!(
        outlook
            .graph_conversation(&id)
            .await
            .iter()
            .any(|m| m.parent_folder_id.as_deref() == Some(inbox_id.as_str())),
        "orphaned snooze returned to Inbox on Graph"
    );
    let exts = outlook.graph_extensions(&id).await;
    assert!(
        exts.contains(REMINDER_UNREAD_EXT) && !exts.contains(SNOOZED_EXT),
        "orphaned snooze marked reminderUnread and unsnoozed: {exts:?}"
    );
}

#[tokio::test]
async fn new_message_sent_with_reminder_attaches_to_sent_conversation_and_marks_graph() {
    let outlook = OutlookHarness::synced().await;
    let at = from_now(48.hours());
    let draft_id = "draft00000000000091";
    let email = gmail::mime::Email {
        from: Some(gmail::mime::Mailbox::new("Maya Okafor", ADDRESS)),
        to: vec![gmail::mime::Mailbox::new("Ann", "ann@example.com")],
        subject: "Follow up on Q4".into(),
        html: Some("<div>Let me know!</div>".into()),
        text: Some("Let me know!".into()),
        message_id: Some(format!("{draft_id}@exo")),
        ..gmail::mime::Email::default()
    };
    let record = DraftRecord {
        action: DraftAction::Compose,
        from: Some(Address {
            name: Some("Maya Okafor".into()),
            email: ADDRESS.into(),
        }),
        to: vec![Address {
            name: Some("Ann".into()),
            email: "ann@example.com".into(),
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "Follow up on Q4".into(),
        body: "<div>Let me know!</div>".into(),
        quoted_content: None,
        quoted_inlined: false,
        in_reply_to: None,
        in_reply_to_rfc822: None,
        references: Vec::new(),
        rfc822_id: format!("{draft_id}@exo"),
        fingerprint: String::new(),
        attachments: Vec::new(),
        send_later: None,
        reminder: Some(DraftReminder {
            at,
            keep_on_reply: false,
            automatic: false,
        }),
    };
    outlook
        .store
        .save_draft(Draft {
            id: draft_id.into(),
            thread: None,
            adopted: None,
            record,
            raw: String::from_utf8(email.to_rfc822()).unwrap(),
            complete: true,
            sending: true,
        })
        .await
        .expect("saves draft");
    outlook.store.push_drafts().await.expect("writes draft");
    outlook
        .store
        .send_draft(draft_id, Timestamp::now())
        .await
        .expect("marks for send");
    let report = outlook.store.push_drafts().await.expect("sends draft");
    let [sent] = report.sent.as_slice() else {
        panic!("expected one sent draft: {report:?}");
    };
    outlook.flush().await;

    // Verify Graph's Sent Items message matches `sent.thread` and carries `build.miguel.exo.snoozed`.
    let sent_graph = outlook
        .graph
        .mail()
        .message(&sent.message)
        .select("id,conversationId")
        .await
        .expect("sent message on Graph");
    assert_eq!(
        sent_graph.conversation_id.as_deref(),
        Some(sent.thread.as_str()),
        "SentDraft.thread matches Graph's actual conversationId"
    );
    assert_eq!(
        outlook
            .summary(&ListId::Reminders, &sent.thread)
            .await
            .expect("sent thread in Reminders")
            .reminder,
        Some(ReminderStatus::Set {
            at,
            keep_on_reply: false
        })
    );
    assert!(
        outlook
            .graph_extensions(&sent.thread)
            .await
            .contains(SNOOZED_EXT),
        "snoozed extension persisted on sent message"
    );
    assert_eq!(
        outlook.store.return_reminders(at).await.unwrap().as_slice(),
        std::slice::from_ref(&sent.thread)
    );
}
