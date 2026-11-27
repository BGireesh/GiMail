//! Triage on an Outlook account: Exo's label changes made as Graph's moves and per-message edits,
//! checked against what Graph has afterwards.

use crate::support;

use std::collections::BTreeSet;

use exo_store::{
    BulkTarget, ListCutoff, ListId, Mutation, Progress, Relabelled, Remote, Scope, Store, ThreadId,
};
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::{
    FollowupFlagStatus, InferenceClassificationType, Message, OpenTypeExtension, well_known,
};
use support::{TempDir, options};

/// A mock, an Outlook store synced from it, and Graph as another client sees it.
struct Outlook {
    _server: MockServer,
    _dir: TempDir,
    store: Store,
    graph: outlook::Client,
}

impl Outlook {
    async fn synced() -> Self {
        let server = MockServer::start(Config::default())
            .await
            .expect("mock starts");
        let dir = TempDir::new();
        let store = Store::open_with(
            dir.path(),
            "microsoft maya@brightmoor.co",
            Remote::outlook(
                outlook::Endpoints::mock(&server.url()),
                outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
            ),
            options(),
        )
        .await
        .expect("store opens");
        let graph = outlook::Client::new(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        );
        for _ in 0..80 {
            if !store.sync().await.expect("sync").backfilling {
                break;
            }
        }
        Self {
            _server: server,
            _dir: dir,
            store,
            graph,
        }
    }

    /// Conversation `thread`'s messages as Graph has them now.
    async fn messages(&self, thread: &ThreadId) -> Vec<Message> {
        self.graph
            .mail()
            .conversation(thread.as_str())
            .select("id,parentFolderId,isRead,flag,categories,inferenceClassification,isDraft")
            .expand(&OpenTypeExtension::expand("build.miguel.exo.snoozed"))
            .all()
            .await
            .expect("reads the conversation")
            .value
    }

    async fn folder_id(&self, name: &str) -> String {
        self.graph
            .mail()
            .folder(name)
            .await
            .expect("a folder")
            .id
            .expect("an id")
    }

    /// The newest Inbox conversation whose every message is in the Inbox.
    async fn inbox_conversation(&self, more_than_one: bool) -> ThreadId {
        let inbox = self.folder_id(well_known::INBOX).await;
        let page = self
            .store
            .list(&ListId::Inbox, None, 100)
            .await
            .expect("lists");
        for row in page.threads {
            let messages = self.messages(&row.id).await;
            if (messages.len() > 1) == more_than_one
                && messages
                    .iter()
                    .all(|message| message.parent_folder_id.as_deref() == Some(inbox.as_str()))
            {
                return row.id;
            }
        }
        panic!("no such conversation");
    }

    async fn apply(&self, thread: &ThreadId, mutation: Mutation) -> exo_store::Undo {
        let undo = self
            .store
            .apply(std::slice::from_ref(thread), mutation)
            .await
            .expect("applies");
        let report = self.store.flush().await.expect("flushes");
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        undo
    }
}

#[tokio::test]
async fn done_moves_the_conversation_to_archive_and_undo_brings_it_back() {
    let outlook = Outlook::synced().await;
    let thread = outlook.inbox_conversation(true).await;
    let (inbox, archive) = (
        outlook.folder_id(well_known::INBOX).await,
        outlook.folder_id(well_known::ARCHIVE).await,
    );
    let ids: BTreeSet<Option<String>> = outlook
        .messages(&thread)
        .await
        .into_iter()
        .map(|message| message.id)
        .collect();
    let undo = outlook.apply(&thread, Mutation::MarkDone).await;
    let moved = outlook.messages(&thread).await;
    assert!(
        moved
            .iter()
            .all(|message| message.parent_folder_id.as_deref() == Some(archive.as_str())),
        "every message is in Archive"
    );
    assert_eq!(
        moved
            .iter()
            .map(|message| message.id.clone())
            .collect::<BTreeSet<_>>(),
        ids,
        "immutable ids survive the move"
    );
    outlook.store.sync().await.expect("syncs");
    let done = outlook
        .store
        .list(&ListId::Done, None, 500)
        .await
        .expect("lists");
    assert!(done.threads.iter().any(|row| row.id == thread), "done");

    outlook.store.undo(undo).await.expect("undoes");
    outlook.store.flush().await.expect("flushes");
    assert!(
        outlook
            .messages(&thread)
            .await
            .iter()
            .all(|message| message.parent_folder_id.as_deref() == Some(inbox.as_str())),
        "back in the Inbox"
    );
}

#[tokio::test]
async fn archiving_leaves_sent_items_where_they_are() {
    let outlook = Outlook::synced().await;
    let sent = outlook.folder_id(well_known::SENT_ITEMS).await;
    let inbox = outlook.folder_id(well_known::INBOX).await;
    // An Inbox conversation the user replied in.
    let page = outlook
        .store
        .list(&ListId::Inbox, None, 200)
        .await
        .expect("lists");
    let mut found = None;
    for row in page.threads {
        let messages = outlook.messages(&row.id).await;
        let folders: BTreeSet<_> = messages
            .iter()
            .map(|m| m.parent_folder_id.clone())
            .collect();
        if folders.contains(&Some(sent.clone())) && folders.contains(&Some(inbox.clone())) {
            found = Some(row.id);
            break;
        }
    }
    let thread = found.expect("a conversation with a reply in Sent Items");
    outlook.apply(&thread, Mutation::MarkDone).await;
    let archive = outlook.folder_id(well_known::ARCHIVE).await;
    for message in outlook.messages(&thread).await {
        let folder = message.parent_folder_id.expect("a folder");
        assert!(folder == sent || folder == archive, "{folder}");
        assert_ne!(folder, inbox);
    }
    assert!(
        outlook
            .messages(&thread)
            .await
            .iter()
            .any(|message| message.parent_folder_id.as_deref() == Some(sent.as_str())),
        "the reply stays in Sent Items"
    );
}

#[tokio::test]
async fn read_flags_and_trash_are_graphs_own() {
    let outlook = Outlook::synced().await;
    let thread = outlook.inbox_conversation(false).await;
    let message = || async { outlook.messages(&thread).await.remove(0) };

    outlook.apply(&thread, Mutation::MarkUnread).await;
    assert_eq!(message().await.is_read, Some(false));
    outlook.apply(&thread, Mutation::MarkRead).await;
    assert_eq!(message().await.is_read, Some(true));

    outlook.apply(&thread, Mutation::Star).await;
    assert_eq!(
        message().await.flag.and_then(|flag| flag.flag_status),
        Some(FollowupFlagStatus::Flagged)
    );
    outlook.apply(&thread, Mutation::Unstar).await;
    assert_eq!(
        message().await.flag.and_then(|flag| flag.flag_status),
        Some(FollowupFlagStatus::Complete),
        "unstarring completes the flag"
    );

    let undo = outlook.apply(&thread, Mutation::Trash).await;
    let deleted = outlook.folder_id(well_known::DELETED_ITEMS).await;
    assert_eq!(message().await.parent_folder_id, Some(deleted));
    outlook.store.undo(undo).await.expect("undoes");
    outlook.store.flush().await.expect("flushes");
    let inbox = outlook.folder_id(well_known::INBOX).await;
    assert_eq!(message().await.parent_folder_id, Some(inbox));

    outlook.apply(&thread, Mutation::Spam).await;
    let junk = outlook.folder_id(well_known::JUNK_EMAIL).await;
    assert_eq!(message().await.parent_folder_id, Some(junk));
}

#[tokio::test]
async fn other_and_important_are_focused_inbox() {
    let outlook = Outlook::synced().await;
    let important = outlook
        .store
        .list(&ListId::Important, None, 100)
        .await
        .expect("lists");
    let thread = important
        .threads
        .iter()
        .find(|row| row.messages == 1)
        .map(|row| row.id.clone())
        .expect("an Important conversation of one");
    outlook
        .apply(
            &thread,
            Mutation::Move {
                from: Some("SH_IMPORTANT".into()),
                to: "SH_OTHER".into(),
            },
        )
        .await;
    let message = outlook.messages(&thread).await.remove(0);
    assert_eq!(
        message.inference_classification,
        Some(InferenceClassificationType::Other)
    );
    let inbox = outlook.folder_id(well_known::INBOX).await;
    assert_eq!(
        message.parent_folder_id.as_deref(),
        Some(inbox.as_str()),
        "still in the Inbox"
    );
    outlook.store.sync().await.expect("syncs");
    let other = outlook
        .store
        .list(&ListId::Other, None, 200)
        .await
        .expect("lists");
    assert!(other.threads.iter().any(|row| row.id == thread));

    outlook
        .apply(
            &thread,
            Mutation::Move {
                from: Some("SH_OTHER".into()),
                to: "SH_IMPORTANT".into(),
            },
        )
        .await;
    assert_eq!(
        outlook.messages(&thread).await[0].inference_classification,
        Some(InferenceClassificationType::Focused)
    );
}

#[tokio::test]
async fn categories_and_folders_are_labels_to_add() {
    let outlook = Outlook::synced().await;
    let labels = outlook.store.labels().await.expect("labels");
    let category = outlook
        .graph
        .settings()
        .categories()
        .all()
        .await
        .expect("categories")
        .value
        .into_iter()
        .next()
        .expect("a category");
    let (id, name) = (
        category.id.expect("an id"),
        category.display_name.expect("a name"),
    );
    let thread = outlook.inbox_conversation(false).await;
    outlook.apply(&thread, Mutation::AddLabel(id.clone())).await;
    assert!(
        outlook.messages(&thread).await[0]
            .categories
            .contains(&name)
    );
    outlook.apply(&thread, Mutation::RemoveLabel(id)).await;
    assert!(
        !outlook.messages(&thread).await[0]
            .categories
            .contains(&name)
    );

    // Moving to a user folder takes the conversation there.
    let receipts = labels
        .iter()
        .find(|label| label.name == "Inbox/Receipts")
        .expect("Receipts");
    outlook
        .apply(
            &thread,
            Mutation::Move {
                from: Some("INBOX".into()),
                to: receipts.id.clone(),
            },
        )
        .await;
    assert_eq!(
        outlook.messages(&thread).await[0]
            .parent_folder_id
            .as_deref(),
        Some(receipts.id.as_str())
    );
}

#[tokio::test]
async fn a_reminder_marks_the_conversation_after_moving_it() {
    let outlook = Outlook::synced().await;
    let thread = outlook.inbox_conversation(false).await;
    let at = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(48);
    let undo = outlook
        .apply(
            &thread,
            Mutation::Remind {
                at,
                keep_on_reply: false,
                snooze: true,
            },
        )
        .await;
    let message = outlook.messages(&thread).await.remove(0);
    let archive = outlook.folder_id(well_known::ARCHIVE).await;
    assert_eq!(
        message.parent_folder_id,
        Some(archive),
        "snoozed out of the Inbox"
    );
    assert_eq!(message.extensions.len(), 1, "and marked");
    // The marker is found across folders, as a new store's orphaned-reminder pass looks for it.
    let found = outlook
        .graph
        .mail()
        .messages()
        .filter(&OpenTypeExtension::filter("build.miguel.exo.snoozed"))
        .select("id")
        .all()
        .await
        .expect("filters")
        .value;
    assert_eq!(found.len(), 1);

    outlook.store.undo(undo).await.expect("undoes");
    outlook.store.flush().await.expect("flushes");
    let message = outlook.messages(&thread).await.remove(0);
    assert!(message.extensions.is_empty(), "the marker is gone");
}

#[tokio::test]
async fn bulk_relabels_and_select_all_reach_every_conversation() {
    let outlook = Outlook::synced().await;
    let inbox = outlook
        .store
        .list(&ListId::Inbox, None, 500)
        .await
        .expect("lists");
    let newest = inbox.threads.first().expect("mail").date;
    let target = BulkTarget {
        list: Some(ListCutoff {
            scope: Scope::List(ListId::Inbox),
            filter: None,
            keep: Vec::new(),
            before: newest,
        }),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let mut done = Relabelled::default();
    let progress = Progress::default();
    outlook
        .store
        .bulk(&target, &Mutation::MarkRead, &progress, &mut done)
        .await
        .expect("marks the Inbox read");
    let unread = outlook
        .graph
        .mail()
        .folder_messages(well_known::INBOX)
        .filter("isRead eq false")
        .select("id")
        .all()
        .await
        .expect("lists")
        .value;
    assert!(unread.is_empty(), "{} still unread", unread.len());
    outlook
        .store
        .undo_bulk(&done, &progress)
        .await
        .expect("undoes");
    let unread_again = outlook
        .graph
        .mail()
        .folder_messages(well_known::INBOX)
        .filter("isRead eq false")
        .select("id")
        .all()
        .await
        .expect("lists")
        .value;
    assert!(!unread_again.is_empty(), "undo marks them unread again");
}
