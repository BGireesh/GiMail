//! Drafts and sending on an Outlook account: a draft is a message in Drafts, made again when it
//! changes, and sent only by sending that draft.

use crate::support;

use exo_store::{Address, Draft, DraftAction, DraftRecord, ListId, Remote, Store};
use jiff::Timestamp;
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::{Message, well_known};
use serde_json::json;
use support::{TempDir, unpaced};

const ADDRESS: &str = "maya@brightmoor.co";

struct Outlook {
    server: MockServer,
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
            unpaced(),
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
            server,
            _dir: dir,
            store,
            graph,
        }
    }

    /// The messages in `folder` with `subject`.
    async fn in_folder(&self, folder: &str, subject: &str) -> Vec<Message> {
        self.graph
            .mail()
            .folder_messages(folder)
            .select("id,subject,conversationId,isDraft")
            .top(500)
            .all()
            .await
            .expect("lists")
            .value
            .into_iter()
            .filter(|message| message.subject.as_deref() == Some(subject))
            .collect()
    }

    async fn fault(&self, fault: serde_json::Value) {
        let url = self.server.url().join("/__mock/faults").expect("a URL");
        reqwest::Client::new()
            .post(url)
            .json(&fault)
            .send()
            .await
            .expect("the mock takes it");
    }
}

fn draft(id: &str, subject: &str, body: &str, files: &[(&str, usize)]) -> Draft {
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
        subject: subject.into(),
        body: body.into(),
        quoted_content: None,
        quoted_inlined: false,
        in_reply_to: None,
        in_reply_to_rfc822: None,
        references: Vec::new(),
        rfc822_id: format!("{id}@exo"),
        fingerprint: String::new(),
        attachments: Vec::new(),
        send_later: None,
        reminder: None,
    };
    let email = gmail::mime::Email {
        from: Some(gmail::mime::Mailbox::new("Maya Okafor", ADDRESS)),
        to: vec![gmail::mime::Mailbox::new("Ann", "ann@example.com")],
        subject: subject.into(),
        html: Some(format!("<div>{body}</div>")),
        text: Some(body.into()),
        message_id: Some(format!("{id}@exo")),
        attachments: files
            .iter()
            .map(|(name, size)| gmail::mime::Attachment {
                filename: (*name).into(),
                content_type: "application/pdf".into(),
                data: vec![b'%'; *size],
                content_id: None,
            })
            .collect(),
        ..gmail::mime::Email::default()
    };
    Draft {
        id: id.into(),
        thread: None,
        adopted: None,
        record,
        raw: String::from_utf8(email.to_rfc822()).expect("MIME is ASCII"),
        complete: true,
        sending: false,
    }
}

#[tokio::test]
async fn a_draft_is_made_again_when_it_changes() {
    let outlook = Outlook::synced().await;
    let id = "draft00000000000001";
    outlook
        .store
        .save_draft(draft(id, "Lunch", "Hi", &[]))
        .await
        .expect("saves");
    let report = outlook.store.push_drafts().await.expect("pushes");
    assert_eq!(report.saved, [id], "{report:?}");
    let stored = outlook
        .store
        .draft(id)
        .await
        .expect("reads")
        .expect("stored");
    let first = stored.draft_id.clone().expect("an Outlook draft");
    assert_eq!(
        stored.message_id.as_deref(),
        Some(first.as_str()),
        "one id for both"
    );
    let drafts = outlook.in_folder(well_known::DRAFTS, "Lunch").await;
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].id.as_deref(), Some(first.as_str()));
    let thread = stored.thread.clone().expect("a conversation");
    assert_eq!(drafts[0].conversation_id.as_deref(), Some(thread.as_str()));

    outlook
        .store
        .save_draft(draft(id, "Lunch tomorrow", "Hi there", &[]))
        .await
        .expect("saves again");
    outlook.store.push_drafts().await.expect("pushes");
    let stored = outlook
        .store
        .draft(id)
        .await
        .expect("reads")
        .expect("stored");
    let second = stored.draft_id.clone().expect("an Outlook draft");
    assert_ne!(second, first, "a new draft");
    assert!(
        outlook
            .in_folder(well_known::DRAFTS, "Lunch")
            .await
            .is_empty(),
        "the old one's gone"
    );
    let drafts = outlook
        .in_folder(well_known::DRAFTS, "Lunch tomorrow")
        .await;
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].id.as_deref(), Some(second.as_str()));
    outlook.store.sync().await.expect("syncs");
    let thread = stored.thread.expect("a conversation");
    let detail = outlook
        .store
        .thread(&thread)
        .await
        .expect("reads")
        .expect("stored");
    assert_eq!(
        detail.messages.len(),
        1,
        "one draft in the thread: {detail:?}"
    );
    assert_eq!(detail.messages[0].id, second);
}

#[tokio::test]
async fn a_draft_is_sent_by_sending_it_and_only_once() {
    let outlook = Outlook::synced().await;
    let id = "draft00000000000002";
    outlook
        .store
        .save_draft(draft(id, "Quarterly plans", "Attached.", &[]))
        .await
        .expect("saves");
    outlook.store.push_drafts().await.expect("pushes");
    let message = outlook
        .store
        .draft(id)
        .await
        .expect("reads")
        .and_then(|draft| draft.draft_id)
        .expect("an Outlook draft");
    // Graph sends it but the answer is lost, so the send is tried again.
    outlook
        .fault(json!({
            "method": "POST",
            "path": format!("/v1.0/me/messages/{message}/send"),
            "status": 500,
            "after": true,
        }))
        .await;
    outlook
        .store
        .send_draft(id, Timestamp::now())
        .await
        .expect("sends now");
    let report = outlook.store.push_drafts().await.expect("pushes");
    let [sent] = report.sent.as_slice() else {
        panic!("sent one: {report:?}")
    };
    assert_eq!(sent.message, message, "Sent Items keeps the draft's id");
    let copies = outlook
        .in_folder(well_known::SENT_ITEMS, "Quarterly plans")
        .await;
    assert_eq!(copies.len(), 1, "sent once: {copies:?}");
    assert!(
        outlook
            .in_folder(well_known::DRAFTS, "Quarterly plans")
            .await
            .is_empty()
    );
    assert!(outlook.store.draft(id).await.expect("reads").is_none());
    assert!(
        outlook
            .store
            .list(&ListId::Sent, None, 50)
            .await
            .expect("lists")
            .threads
            .iter()
            .any(|row| row.id == sent.thread),
        "in Sent at once"
    );
}

#[tokio::test]
async fn a_large_file_goes_up_in_an_upload_session() {
    let outlook = Outlook::synced().await;
    let id = "draft00000000000003";
    outlook
        .store
        .save_draft(draft(
            id,
            "The deck",
            "Here it is.",
            &[("deck.pdf", 5 << 20), ("notes.pdf", 2_000)],
        ))
        .await
        .expect("saves");
    let report = outlook.store.push_drafts().await.expect("pushes");
    assert_eq!(report.saved, [id], "{report:?}");
    let message = outlook
        .store
        .draft(id)
        .await
        .expect("reads")
        .and_then(|draft| draft.draft_id)
        .expect("an Outlook draft");
    let attachments = outlook
        .graph
        .mail()
        .attachments(&message)
        .all()
        .await
        .expect("lists")
        .value;
    let mut files: Vec<(String, u64)> = attachments
        .into_iter()
        .map(|file| (file.name.unwrap_or_default(), file.size.unwrap_or_default()))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            ("deck.pdf".to_owned(), 5 << 20),
            ("notes.pdf".to_owned(), 2_000)
        ]
    );
}
