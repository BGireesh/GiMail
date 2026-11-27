//! An Outlook account synced through Microsoft Graph: mock-servers serves one mailbox as Gmail
//! and as Graph, so what the store has of it as Outlook is checked against what Graph says, and
//! against the same mailbox stored as Gmail.

use crate::support;

use std::collections::BTreeSet;

use exo_store::{ListId, Remote, Store, ThreadId, labels};
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::{
    FollowupFlag, FollowupFlagStatus, InferenceClassificationType, Message, MessagePatch,
    well_known,
};
use serde_json::json;
use support::{TempDir, options};

/// The store's name for the mock's account as a Microsoft account.
const ACCOUNT: &str = "microsoft maya@brightmoor.co";

/// A mock, an Outlook store on it, and Graph as another client sees it.
struct Outlook {
    server: MockServer,
    dir: TempDir,
    store: Store,
    graph: outlook::Client,
}

impl Outlook {
    async fn start() -> Self {
        Self::start_with(Config::default()).await
    }

    async fn start_with(config: Config) -> Self {
        let server = MockServer::start(config).await.expect("mock starts");
        let dir = TempDir::new();
        let store = open(&server, &dir).await;
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

    async fn synced() -> Self {
        let outlook = Self::start().await;
        outlook.sync_all().await;
        outlook
    }

    async fn sync_all(&self) {
        for _ in 0..80 {
            if !self.store.sync().await.expect("sync").backfilling {
                return;
            }
        }
        panic!("backfill never finished");
    }

    /// Every thread id of `list`.
    async fn ids(&self, list: &ListId) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        let mut after = None;
        loop {
            let page = self
                .store
                .list(list, after.as_ref(), 50)
                .await
                .expect("lists");
            ids.extend(
                page.threads
                    .iter()
                    .map(|thread| thread.id.as_str().to_owned()),
            );
            match page.next {
                Some(next) => after = Some(next),
                None => return ids,
            }
        }
    }

    /// Every message of folder `folder` as Graph has it.
    async fn folder(&self, folder: &str) -> Vec<Message> {
        self.graph
            .mail()
            .folder_messages(folder)
            .select("id,conversationId,subject,isRead,inferenceClassification,receivedDateTime")
            .top(500)
            .all()
            .await
            .expect("lists the folder")
            .value
    }

    /// The conversations Graph has in `folder`.
    async fn conversations(&self, folder: &str) -> BTreeSet<String> {
        self.folder(folder)
            .await
            .into_iter()
            .filter_map(|message| message.conversation_id)
            .collect()
    }

    /// The newest message in the Inbox whose conversation has no other message, and its
    /// conversation.
    async fn lone_inbox_message(&self) -> (String, String) {
        let inbox = self.folder(well_known::INBOX).await;
        let mut lone = Vec::new();
        for message in &inbox {
            let thread = message.conversation_id.clone().expect("a conversation");
            let all = self
                .graph
                .mail()
                .conversation(&thread)
                .select("id")
                .all()
                .await
                .expect("reads the conversation")
                .value;
            if all.len() == 1 {
                lone.push((message.id.clone().expect("an id"), thread));
            }
        }
        lone.into_iter().next().expect("a conversation of one")
    }

    async fn patch(&self, id: &str, patch: MessagePatch) {
        self.graph
            .mail()
            .update_message(id, &patch)
            .await
            .expect("patches");
    }

    async fn post(&self, path: &str, body: serde_json::Value) {
        let response = reqwest::Client::new()
            .post(format!(
                "{}{path}",
                self.server.url().as_str().trim_end_matches('/')
            ))
            .json(&body)
            .send()
            .await
            .expect("the mock answers");
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
    }
}

async fn open(server: &MockServer, dir: &TempDir) -> Store {
    Store::open_with(
        dir.path(),
        ACCOUNT,
        Remote::outlook(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        options(),
    )
    .await
    .expect("store opens")
}

#[tokio::test]
async fn an_outlook_account_syncs_its_folders_into_lists() {
    let outlook = Outlook::synced().await;
    let store = &outlook.store;
    assert_eq!(
        outlook.ids(&ListId::Inbox).await,
        outlook.conversations(well_known::INBOX).await,
        "the Inbox is the conversations with mail in the Inbox folder"
    );
    // Junk Email and Deleted Items are backfilled when they're opened.
    for (list, folder) in [
        (ListId::Spam, well_known::JUNK_EMAIL),
        (ListId::Trash, well_known::DELETED_ITEMS),
    ] {
        while store.backfill(&list, 100).await.expect("backfills") {}
        assert_eq!(
            outlook.ids(&list).await,
            outlook.conversations(folder).await
        );
    }

    // Focused is Important, and what isn't is Other.
    let inbox = outlook.folder(well_known::INBOX).await;
    let focused = |thread: &str| {
        inbox.iter().any(|message| {
            message.conversation_id.as_deref() == Some(thread)
                && message.inference_classification == Some(InferenceClassificationType::Focused)
        })
    };
    let (important, other) = (
        outlook.ids(&ListId::Important).await,
        outlook.ids(&ListId::Other).await,
    );
    assert!(!important.is_empty() && !other.is_empty());
    assert!(
        other.iter().all(|thread| !focused(thread)),
        "no Focused conversation is in Other"
    );
    assert!(
        important.iter().filter(|thread| focused(thread)).count() * 2 > important.len(),
        "Important is Focused, and the user's own mail"
    );

    // User folders and categories are labels by id, and Exo's markers are hidden.
    let listed = store.labels().await.expect("labels");
    let names: BTreeSet<&str> = listed.iter().map(|label| label.name.as_str()).collect();
    for name in ["Inbox/Receipts", "Inbox/Newsletters", "INBOX", "STARRED"] {
        assert!(names.contains(name), "{name} in {names:?}");
    }
    let categories = outlook
        .graph
        .settings()
        .categories()
        .all()
        .await
        .expect("categories")
        .value;
    assert!(!categories.is_empty());
    for category in &categories {
        let id = category.id.as_deref().expect("an id");
        let label = listed
            .iter()
            .find(|label| label.id == id)
            .expect("each category is a label");
        assert_eq!(Some(label.name.as_str()), category.display_name.as_deref());
    }
    assert!(
        listed
            .iter()
            .filter(|label| label.name.starts_with("[Exo]"))
            .all(|label| label.hidden),
        "{listed:?}"
    );
    assert!(
        !names.contains("[Exo]/Muted"),
        "Outlook has no mute, so no mute label"
    );

    // A user folder is a list of its own, and its mail isn't in the Inbox.
    let receipts = listed
        .iter()
        .find(|label| label.name == "Inbox/Receipts")
        .expect("Receipts");
    let list = ListId::Label(receipts.id.clone());
    while store.backfill(&list, 100).await.expect("backfills") {}
    let filed = outlook.ids(&list).await;
    assert!(!filed.is_empty());
    let receipts_folder = outlook.conversations(&receipts.id).await;
    assert_eq!(filed, receipts_folder);
}

#[tokio::test]
async fn rows_read_as_the_same_mailbox_stored_from_gmail() {
    let outlook = Outlook::synced().await;
    let dir = TempDir::new();
    let gmail = Store::open_with(
        dir.path(),
        "maya@brightmoor.co",
        Remote::gmail(
            outlook.server.endpoints(),
            gmail::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        options(),
    )
    .await
    .expect("store opens");
    for _ in 0..80 {
        if !gmail.sync().await.expect("sync").backfilling {
            break;
        }
    }
    let starred = |store: Store| async move {
        let page = store
            .list(&ListId::Starred, None, 100)
            .await
            .expect("lists");
        page.threads
            .into_iter()
            .map(|thread| (thread.subject, thread.snippet, thread.participants))
            .collect::<Vec<_>>()
    };
    let (from_outlook, from_gmail) = (
        starred(outlook.store.clone()).await,
        starred(gmail.clone()).await,
    );
    assert!(!from_gmail.is_empty());
    assert_eq!(
        from_outlook.len(),
        from_gmail.len(),
        "the same starred mail"
    );
    for ((subject, snippet, people), (gmail_subject, gmail_snippet, gmail_people)) in
        from_outlook.iter().zip(&from_gmail)
    {
        assert_eq!(subject, gmail_subject);
        assert_eq!(people, gmail_people, "{subject}");
        // Graph's preview runs longer than Gmail's snippet, and both are decoded alike.
        assert!(
            snippet.starts_with(gmail_snippet.trim_end()),
            "{subject}: {snippet:?} doesn't start with {gmail_snippet:?}"
        );
    }
}

#[tokio::test]
async fn changes_made_in_another_app_come_in_with_the_next_sync() {
    let outlook = Outlook::synced().await;
    let store = &outlook.store;
    let (message, thread) = outlook.lone_inbox_message().await;
    let thread_id = ThreadId::new(thread.clone());
    let summary = |list: ListId| {
        let store = store.clone();
        let thread_id = thread_id.clone();
        async move {
            let page = store.list(&list, None, 500).await.expect("lists");
            page.threads.into_iter().find(|t| t.id == thread_id)
        }
    };

    outlook
        .patch(
            &message,
            MessagePatch {
                is_read: Some(false),
                flag: Some(FollowupFlag::new(FollowupFlagStatus::Flagged)),
                ..MessagePatch::default()
            },
        )
        .await;
    let report = store.sync().await.expect("syncs");
    assert!(!report.rebuilt && report.changed >= 1, "{report:?}");
    assert!(report.arrived.is_empty(), "nothing new arrived");
    let row = summary(ListId::Inbox).await.expect("still in the Inbox");
    assert!(row.unread && row.starred, "{row:?}");
    assert!(summary(ListId::Starred).await.is_some());

    // Read, and the flag completed: neither unread nor starred.
    outlook
        .patch(
            &message,
            MessagePatch {
                is_read: Some(true),
                flag: Some(FollowupFlag::new(FollowupFlagStatus::Complete)),
                ..MessagePatch::default()
            },
        )
        .await;
    store.sync().await.expect("syncs");
    let row = summary(ListId::Inbox).await.expect("still in the Inbox");
    assert!(!row.unread && !row.starred, "{row:?}");

    // Archived: done.
    outlook
        .graph
        .mail()
        .move_message(&message, well_known::ARCHIVE)
        .await
        .expect("moves");
    store.sync().await.expect("syncs");
    assert!(summary(ListId::Inbox).await.is_none(), "out of the Inbox");
    assert!(summary(ListId::Done).await.is_some(), "done");
    let detail = store
        .thread(&thread_id)
        .await
        .expect("reads")
        .expect("stored");
    assert!(
        !detail.labels.iter().any(|label| label == labels::INBOX),
        "{:?}",
        detail.labels
    );

    // Deleted Items is Trash.
    outlook
        .graph
        .mail()
        .move_message(&message, well_known::DELETED_ITEMS)
        .await
        .expect("moves");
    store.sync().await.expect("syncs");
    assert!(summary(ListId::Trash).await.is_some());

    // Deleted for good: gone, and so is its conversation of one.
    outlook
        .graph
        .mail()
        .delete_message(&message)
        .await
        .expect("deletes");
    let report = store.sync().await.expect("syncs");
    assert!(!report.rebuilt);
    assert!(
        store.thread(&thread_id).await.expect("reads").is_none(),
        "the conversation is gone"
    );
}

#[tokio::test]
async fn a_category_put_on_elsewhere_is_a_label_on_the_thread() {
    let outlook = Outlook::synced().await;
    let (message, thread) = outlook.lone_inbox_message().await;
    // One the master list doesn't have yet: Outlook lets a message carry any category, and the
    // mock adds it to the list.
    outlook
        .patch(
            &message,
            MessagePatch {
                categories: Some(vec!["Travel plans".into()]),
                ..MessagePatch::default()
            },
        )
        .await;
    outlook.store.sync().await.expect("syncs");
    let travel = outlook
        .store
        .labels()
        .await
        .expect("labels")
        .into_iter()
        .find(|label| label.name == "Travel plans")
        .expect("the new category is a label");
    assert_eq!(
        outlook.ids(&ListId::Label(travel.id.clone())).await,
        BTreeSet::from([thread.clone()])
    );
    let detail = outlook
        .store
        .thread(&ThreadId::new(thread))
        .await
        .expect("reads")
        .expect("stored");
    assert!(detail.labels.contains(&travel.id), "keyed by id, not name");
}

#[tokio::test]
async fn new_mail_arrives_and_mail_moved_in_doesnt() {
    let outlook = Outlook::synced().await;
    outlook
        .post(
            "/__mock/deliver",
            json!({
                "from": "Ada Lovelace <ada@analytical.example>",
                "subject": "Engine notes",
                "text": "The second engine's plans are attached.",
            }),
        )
        .await;
    let report = outlook.store.sync().await.expect("syncs");
    assert_eq!(report.arrived.len(), 1, "{report:?}");
    let arrival = &report.arrived[0];
    assert_eq!(arrival.subject, "Engine notes");
    assert_eq!(arrival.snippet, "The second engine's plans are attached.");
    assert!(arrival.unread && arrival.inbox);
    assert_eq!(
        arrival.from.as_ref().map(|from| from.email.as_str()),
        Some("ada@analytical.example")
    );

    // Old mail moved back into the Inbox comes in, but isn't news.
    let fresh = Outlook::start_with(Config::default()).await;
    fresh.store.sync().await.expect("the first round");
    let archived = fresh.folder(well_known::ARCHIVE).await;
    let oldest = archived
        .iter()
        .min_by_key(|message| message.received_date_time)
        .and_then(|message| message.id.clone())
        .expect("archived mail");
    fresh
        .graph
        .mail()
        .move_message(&oldest, well_known::INBOX)
        .await
        .expect("moves");
    let report = fresh.store.sync().await.expect("syncs");
    assert!(report.arrived.is_empty(), "{:?}", report.arrived);
    fresh.sync_all().await;
    assert_eq!(
        fresh.ids(&ListId::Inbox).await,
        fresh.conversations(well_known::INBOX).await
    );
}

#[tokio::test]
async fn an_expired_delta_lists_its_folders_again_without_a_rebuild() {
    let outlook = Outlook::synced().await;
    let (message, thread) = outlook.lone_inbox_message().await;
    outlook.post("/__mock/history/expire", json!({})).await;
    outlook
        .graph
        .mail()
        .move_message(&message, well_known::ARCHIVE)
        .await
        .expect("moves");
    let report = outlook.store.sync().await.expect("syncs");
    assert!(!report.rebuilt, "only the folders are listed again");
    assert!(report.arrived.is_empty(), "nothing listed again is news");
    assert!(!outlook.ids(&ListId::Inbox).await.contains(&thread));
    assert_eq!(
        outlook.ids(&ListId::Inbox).await,
        outlook.conversations(well_known::INBOX).await
    );
    // And the folders' deltas go on from there.
    outlook
        .graph
        .mail()
        .move_message(&message, well_known::INBOX)
        .await
        .expect("moves back");
    outlook.store.sync().await.expect("syncs");
    assert!(outlook.ids(&ListId::Inbox).await.contains(&thread));
}

#[tokio::test]
async fn changes_read_in_the_same_second_keep_the_later_one() {
    let outlook = Outlook::synced().await;
    let (message, thread) = outlook.lone_inbox_message().await;
    let unread = || async {
        outlook
            .store
            .thread(&ThreadId::new(thread.clone()))
            .await
            .expect("reads")
            .expect("stored")
            .labels
            .iter()
            .any(|label| label == labels::UNREAD)
    };
    for read in [false, true, false, true] {
        outlook
            .patch(
                &message,
                MessagePatch {
                    is_read: Some(read),
                    ..MessagePatch::default()
                },
            )
            .await;
        outlook.store.sync().await.expect("syncs");
        assert_eq!(unread().await, !read, "the latest state, read {read}");
    }
}

#[tokio::test]
async fn bodies_and_files_are_read_from_graph() {
    let outlook = Outlook::synced().await;
    // A conversation with a file.
    let with_files = outlook
        .graph
        .mail()
        .messages()
        .filter("hasAttachments eq true")
        .select("id,conversationId")
        .top(1)
        .await
        .expect("finds one")
        .value
        .into_iter()
        .next()
        .expect("mail with a file");
    let thread = ThreadId::new(with_files.conversation_id.clone().expect("a conversation"));
    outlook.store.load_bodies(&thread).await.expect("loads");
    assert!(outlook.store.bodies_stored(&thread).await.expect("reads"));
    let detail = outlook
        .store
        .thread(&thread)
        .await
        .expect("reads")
        .expect("stored");
    let message = detail
        .messages
        .iter()
        .find(|message| Some(&message.id) == with_files.id.as_ref())
        .expect("the message");
    let file = message
        .attachments
        .iter()
        .find(|file| !file.inline)
        .expect("its file");
    let bytes = outlook
        .store
        .part_data(&message.id, file.attachment_id.as_deref(), &file.part_id)
        .await
        .expect("downloads");
    let graph = outlook
        .graph
        .mail()
        .attachment_content(&message.id, &file.part_id)
        .await
        .expect("Graph has it");
    assert_eq!(bytes, graph.data);
    assert_eq!(bytes.len() as u64, u64::from(file.size));
}

#[tokio::test]
async fn sync_keeps_to_four_requests_at_once() {
    // The mock refuses a fifth request at once with 429, as Outlook does.
    let outlook = Outlook::start_with(Config {
        graph_concurrency: Some(4),
        ..Config::default()
    })
    .await;
    outlook.sync_all().await;
    assert_eq!(
        outlook.ids(&ListId::Inbox).await,
        outlook.conversations(well_known::INBOX).await
    );
}

#[tokio::test]
async fn a_store_reopened_picks_up_where_it_was() {
    let mut outlook = Outlook::synced().await;
    let before = outlook.ids(&ListId::Inbox).await;
    outlook.store = open(&outlook.server, &outlook.dir).await;
    let report = outlook.store.sync().await.expect("syncs");
    assert!(!report.rebuilt && report.arrived.is_empty(), "{report:?}");
    assert_eq!(outlook.ids(&ListId::Inbox).await, before);
}

#[tokio::test]
async fn a_folder_made_elsewhere_is_a_label_and_deleting_it_trashes_its_mail() {
    let outlook = Outlook::synced().await;
    let (message, thread) = outlook.lone_inbox_message().await;
    let travel = outlook
        .graph
        .mail()
        .create_folder(
            Some(well_known::INBOX),
            &outlook::MailFolder {
                display_name: Some("Travel".into()),
                ..outlook::MailFolder::default()
            },
        )
        .await
        .expect("makes a folder")
        .id
        .expect("an id");
    outlook
        .graph
        .mail()
        .move_message(&message, &travel)
        .await
        .expect("moves");
    let report = outlook.store.sync().await.expect("syncs");
    assert!(report.arrived.is_empty(), "moved, not new");
    let labels = outlook.store.labels().await.expect("labels");
    assert!(
        labels
            .iter()
            .any(|label| label.id == travel && label.name == "Inbox/Travel"),
        "{labels:?}"
    );
    assert!(!outlook.ids(&ListId::Inbox).await.contains(&thread));
    assert_eq!(
        outlook.ids(&ListId::Label(travel.clone())).await,
        BTreeSet::from([thread.clone()])
    );

    outlook
        .graph
        .mail()
        .delete_folder(&travel)
        .await
        .expect("deletes the folder");
    outlook.store.sync().await.expect("syncs");
    assert!(
        !outlook
            .store
            .labels()
            .await
            .expect("labels")
            .iter()
            .any(|label| label.id == travel),
        "the folder's label is gone"
    );
    assert!(outlook.ids(&ListId::Trash).await.contains(&thread));
}
