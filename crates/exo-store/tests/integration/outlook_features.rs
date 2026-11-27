//! Integration tests for Outlook features in `exo-store`:
//! 1. Sender rules (`Block` via `messageRules`, `Important`/`Other` via Focused Inbox overrides,
//!    undo, `delete_block_filters`, lost-answer recovery, `find_threads`).
//! 2. Raw thread source (`Store::thread_source`).
//! 3. Provider-neutral Out of Office (`Store::auto_reply`, `Store::update_auto_reply`,
//!    `OutOfOfficeResponder::sync_auto_reply`) on both Outlook and Gmail.
//! 4. Provider-neutral Contacts & Directory sync (`Store::sync_contacts`,
//!    `Store::my_photo_metadata`, `Store::contact_photo`) on Outlook (work + personal) and Gmail.
//! 5. Large-attachment upload progress on Outlook drafts (`Store::uploads`).

use crate::support;

use std::collections::BTreeSet;

use exo_store::{
    Address, AttachmentSource, AutoReply, AutoReplyStatus, Draft, DraftAction, DraftAttachment,
    DraftRecord, Error, ExternalAudience, ListId, OutOfOfficeResponder, OutlookAutoReply,
    PhotoMetadata, Remote, SenderRule, SenderRuleUndo, Store,
};
use jiff::Timestamp;
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::InferenceClassificationType;
use outlook::settings::{MessageRule, MessageRuleActions, MessageRulePredicates};
use serde_json::{Value, json};
use support::{TempDir, unpaced};

const ACCOUNT: &str = "microsoft maya@brightmoor.co";
const ADDRESS: &str = "maya@brightmoor.co";

struct OutlookEnv {
    server: MockServer,
    _dir: TempDir,
    store: Store,
    graph: outlook::Client,
}

impl OutlookEnv {
    async fn start(config: Config) -> Self {
        let server = MockServer::start(config).await.expect("mock starts");
        let dir = TempDir::new();
        let store = Store::open_with(
            dir.path(),
            ACCOUNT,
            Remote::outlook(
                outlook::Endpoints::mock(&server.url()),
                outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
            ),
            unpaced(),
        )
        .await
        .expect("store opens");
        for _ in 0..80 {
            if !store.sync().await.expect("sync").backfilling {
                break;
            }
        }
        let graph = outlook::Client::new(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        );
        Self {
            server,
            _dir: dir,
            store,
            graph,
        }
    }

    async fn fault(&self, fault: Value) {
        let url = self.server.url().join("/__mock/faults").expect("url");
        let response = reqwest::Client::new()
            .post(url)
            .json(&fault)
            .send()
            .await
            .expect("fault post");
        assert!(response.status().is_success());
    }
}

#[tokio::test]
async fn outlook_block_sender_and_domain_rules_undo_lost_answer_and_find_threads() {
    let env = OutlookEnv::start(Config::default()).await;
    let baseline = env
        .graph
        .settings()
        .rules()
        .all()
        .await
        .unwrap()
        .value
        .len();

    // 1. Block a single email address -> creates an Inbox rule moving to Deleted Items.
    let undo_email = env
        .store
        .set_sender_rule("spammer@noise.example", SenderRule::Block)
        .await
        .expect("blocks sender");
    let SenderRuleUndo::OutlookRule { ref id } = undo_email else {
        panic!("expected OutlookRule undo: {undo_email:?}");
    };
    let rules = env.graph.settings().rules().all().await.unwrap().value;
    assert_eq!(rules.len(), baseline + 1);
    let created_rule = rules
        .iter()
        .find(|rule| rule.id.as_deref() == Some(id.as_str()))
        .expect("created rule exists");
    assert_eq!(
        created_rule
            .conditions
            .as_ref()
            .and_then(|c| c.from_addresses.first())
            .and_then(|r| r.email_address.as_ref())
            .and_then(|e| e.address.as_deref()),
        Some("spammer@noise.example")
    );

    // Blocking the same sender again is a no-op with `SenderRuleUndo::None`.
    let duplicate = env
        .store
        .set_sender_rule("SPAMMER@noise.example", SenderRule::Block)
        .await
        .expect("idempotent block");
    assert_eq!(duplicate, SenderRuleUndo::None);

    // Undo deletes the created rule.
    env.store
        .undo_sender_rule(&undo_email)
        .await
        .expect("undoes block");
    assert_eq!(
        env.graph
            .settings()
            .rules()
            .all()
            .await
            .unwrap()
            .value
            .len(),
        baseline
    );

    // 2. Block a domain + verify `delete_block_filters` deletes only matching block rules.
    let _undo_domain = env
        .store
        .set_sender_rule("promo.example", SenderRule::Block)
        .await
        .expect("blocks domain");
    // Add a non-block rule for the same domain (has a subject filter and marks read, doesn't delete).
    let non_block = MessageRule {
        display_name: Some("Keep receipts".into()),
        sequence: Some(2),
        is_enabled: Some(true),
        conditions: Some(MessageRulePredicates {
            sender_contains: vec!["@promo.example".into()],
            subject_contains: vec!["Receipt".into()],
            ..MessageRulePredicates::default()
        }),
        actions: Some(MessageRuleActions {
            mark_as_read: Some(true),
            ..MessageRuleActions::default()
        }),
        ..MessageRule::default()
    };
    let kept = env
        .graph
        .settings()
        .create_rule(&non_block)
        .await
        .expect("creates non-block rule");
    let deleted = env
        .store
        .delete_block_filters("promo.example")
        .await
        .expect("unblocks domain");
    assert_eq!(deleted, 1, "deleted only the block rule");
    let remaining = env.graph.settings().rules().all().await.unwrap().value;
    assert_eq!(remaining.len(), baseline + 1);
    assert!(
        remaining.iter().any(|rule| rule.id == kept.id),
        "unrelated rule stays"
    );

    // 3. Lost-answer recovery when `POST messageRules` succeeds on server then fails 500.
    env.fault(json!({
        "method": "POST",
        "path": "/v1.0/me/mailFolders/inbox/messageRules",
        "status": 500,
        "after": true,
    }))
    .await;
    let recovered = env
        .store
        .set_sender_rule("lost@noise.example", SenderRule::Block)
        .await
        .expect("recovers lost-answer block rule");
    assert!(matches!(recovered, SenderRuleUndo::OutlookRule { .. }));
    env.store
        .undo_sender_rule(&recovered)
        .await
        .expect("undoes recovered rule");
    assert_eq!(
        env.graph
            .settings()
            .rules()
            .all()
            .await
            .unwrap()
            .value
            .len(),
        baseline + 1,
        "only the baseline and non-block rules remain"
    );

    // 4. `find_threads` searches Outlook cleanly.
    let inbox = env.store.list(&ListId::Inbox, None, 50).await.unwrap();
    let sample = env
        .store
        .thread(&inbox.threads[0].id)
        .await
        .unwrap()
        .expect("thread");
    let sender = sample
        .messages
        .iter()
        .filter_map(|m| m.from.as_ref().map(|s| s.email.clone()))
        .find(|email| !email.eq_ignore_ascii_case(ADDRESS))
        .expect("external sender");
    let found = env
        .store
        .find_threads(&format!("from:{sender}"))
        .await
        .expect("find_threads on Outlook");
    assert!(
        found.contains(&inbox.threads[0].id),
        "found thread from {sender}: {found:?}"
    );
}

#[tokio::test]
async fn outlook_focused_overrides_create_replace_noop_undo_and_reject_domains() {
    let env = OutlookEnv::start(Config::default()).await;

    // Bare domain is rejected cleanly as Unsupported.
    let domain_err = env
        .store
        .set_sender_rule("example.com", SenderRule::Important)
        .await
        .expect_err("domains unsupported for Focused overrides");
    assert!(matches!(domain_err, Error::Unsupported(_)), "{domain_err}");

    // Create Other override.
    let created = env
        .store
        .set_sender_rule("news@example.com", SenderRule::Other)
        .await
        .expect("creates override");
    assert!(matches!(
        created,
        SenderRuleUndo::OutlookOverride { previous: None, .. }
    ));

    // Re-applying identical override is a no-op.
    assert_eq!(
        env.store
            .set_sender_rule("NEWS@example.com", SenderRule::Other)
            .await
            .expect("noop override"),
        SenderRuleUndo::None
    );

    // Flipping to Important updates the existing override and records `previous: Some(Other)`.
    let flipped = env
        .store
        .set_sender_rule("news@example.com", SenderRule::Important)
        .await
        .expect("updates override");
    assert!(matches!(
        flipped,
        SenderRuleUndo::OutlookOverride {
            previous: Some(InferenceClassificationType::Other),
            ..
        }
    ));
    let overrides = env.graph.settings().overrides().all().await.unwrap().value;
    assert_eq!(overrides.len(), 1);
    assert_eq!(
        overrides[0].classify_as,
        Some(InferenceClassificationType::Focused)
    );

    // Undo flip restores `Other`.
    env.store
        .undo_sender_rule(&flipped)
        .await
        .expect("restores Other");
    let overrides = env.graph.settings().overrides().all().await.unwrap().value;
    assert_eq!(
        overrides[0].classify_as,
        Some(InferenceClassificationType::Other)
    );

    // Undo initial create deletes the override.
    env.store
        .undo_sender_rule(&created)
        .await
        .expect("deletes override");
    assert!(
        env.graph
            .settings()
            .overrides()
            .all()
            .await
            .unwrap()
            .value
            .is_empty()
    );
}

#[tokio::test]
async fn outlook_thread_source_returns_pretty_json_with_headers_and_body() {
    let env = OutlookEnv::start(Config::default()).await;
    let id = env
        .store
        .list(&ListId::Inbox, None, 10)
        .await
        .unwrap()
        .threads[0]
        .id
        .clone();

    let raw = env
        .store
        .thread_source(id.as_str())
        .await
        .expect("Outlook thread_source succeeds");
    assert!(raw.contains("\n    \"id\":"), "4-space pretty JSON: {raw}");
    let parsed: Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(parsed["id"].as_str(), Some(id.as_str()));
    let messages = parsed["messages"].as_array().expect("messages array");
    assert!(!messages.is_empty());
    assert!(messages[0]["body"]["content"].is_string());
    assert!(
        messages[0]["internetMessageHeaders"]
            .as_array()
            .is_some_and(|headers| !headers.is_empty()),
        "includes internetMessageHeaders"
    );

    let missing = env
        .store
        .thread_source("AAQkNonExistentConversation")
        .await
        .expect_err("missing conversation fails");
    assert!(matches!(missing, Error::UnknownThread(_)), "{missing}");
}

#[tokio::test]
async fn auto_reply_round_trips_on_outlook_and_gmail_with_stable_polling() {
    let env = OutlookEnv::start(Config::default()).await;

    // Sending a mismatched Gmail AutoReply variant on Outlook returns Unsupported.
    assert!(matches!(
        env.store
            .update_auto_reply(&AutoReply::Gmail(gmail::VacationSettings::default()))
            .await,
        Err(Error::Unsupported(_))
    ));

    // Initial Outlook automatic reply is disabled.
    let initial = env.store.auto_reply().await.expect("reads auto_reply");
    let AutoReply::Outlook(initial_outlook) = initial else {
        panic!("expected Outlook auto reply: {initial:?}");
    };
    assert_eq!(initial_outlook.status, AutoReplyStatus::Disabled);
    assert_eq!(initial_outlook.start_time, None);

    // Update to Scheduled with distinct internal/external bodies.
    let start = "2026-11-20T09:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_millisecond();
    let end = "2026-11-25T18:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_millisecond();
    let scheduled = AutoReply::Outlook(OutlookAutoReply {
        status: AutoReplyStatus::Scheduled,
        external_audience: ExternalAudience::ContactsOnly,
        internal_reply_html: "<p>Internal reply</p>".into(),
        external_reply_html: "<p>External reply</p>".into(),
        start_time: Some(start),
        end_time: Some(end),
    });
    let saved = env
        .store
        .update_auto_reply(&scheduled)
        .await
        .expect("updates scheduled auto reply");
    assert_eq!(saved, scheduled);
    let polled = env.store.auto_reply().await.expect("polls auto reply");
    assert_eq!(polled, scheduled);

    let now: Timestamp = "2026-11-21T12:00:00Z".parse().unwrap();
    let copy = OutOfOfficeResponder::sync_auto_reply(&polled, None, now).expect("creates copy");
    assert!(copy.is_enabled());
    assert!(copy.is_out_of_office(now));
    assert_eq!(
        OutOfOfficeResponder::sync_auto_reply(&polled, Some(&copy), now),
        None,
        "polling unchanged Outlook OOO does not rewrite"
    );

    // Switch to AlwaysEnabled -> scheduled timestamps clear.
    let always = AutoReply::Outlook(OutlookAutoReply {
        status: AutoReplyStatus::AlwaysEnabled,
        external_audience: ExternalAudience::All,
        internal_reply_html: "<p>Away indefinitely</p>".into(),
        external_reply_html: "<p>Away indefinitely</p>".into(),
        start_time: None,
        end_time: None,
    });
    let saved_always = env
        .store
        .update_auto_reply(&always)
        .await
        .expect("updates alwaysEnabled");
    assert_eq!(saved_always, always);

    // Gmail provider also supports `auto_reply` / `update_auto_reply`.
    let gmail_dir = TempDir::new();
    let gmail_store = Store::open_with(
        gmail_dir.path(),
        ADDRESS,
        Remote::gmail(
            gmail::Endpoints::mock(&env.server.url()),
            gmail::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        unpaced(),
    )
    .await
    .expect("opens gmail");
    let vacation = gmail::VacationSettings {
        enable_auto_reply: Some(true),
        response_subject: Some("OOO".into()),
        response_body_html: Some("<div>Back soon</div>".into()),
        restrict_to_contacts: Some(true),
        restrict_to_domain: Some(false),
        start_time: Some(start),
        end_time: Some(end),
        response_body_plain_text: None,
    };
    let gmail_saved = gmail_store
        .update_auto_reply(&AutoReply::Gmail(vacation.clone()))
        .await
        .expect("updates gmail auto reply");
    assert_eq!(gmail_saved, AutoReply::Gmail(vacation.clone()));
    assert_eq!(
        gmail_store.auto_reply().await.expect("reads gmail"),
        AutoReply::Gmail(vacation)
    );
}

#[tokio::test]
async fn contacts_directory_people_and_photos_sync_on_outlook_and_gmail() {
    let env = OutlookEnv::start(Config {
        photo: true,
        contact_photos: true,
        ..Config::default()
    })
    .await;

    // 1. Outlook work account syncs saved contacts, directory (with photo locators), and people.
    let (contacts, complete) = env.store.sync_contacts().await.expect("syncs contacts");
    assert!(complete, "every list succeeded");
    assert!(!contacts.saved.contacts.is_empty(), "saved contacts synced");
    assert!(
        contacts.saved.sync_token.is_some(),
        "contacts delta link kept"
    );
    assert!(
        !contacts.directory.contacts.is_empty(),
        "directory colleagues synced"
    );
    assert!(
        contacts.directory.sync_token.is_some(),
        "users delta link kept"
    );
    assert!(!contacts.other.contacts.is_empty(), "/me/people synced");
    assert_eq!(
        env.store.contacts().await.unwrap().as_ref(),
        Some(&contacts),
        "persisted to kv"
    );

    // Second sync uses stored delta links and preserves entries.
    let (again, complete_again) = env.store.sync_contacts().await.expect("delta sync");
    assert!(complete_again);
    let before_emails: BTreeSet<&str> = contacts
        .saved
        .contacts
        .iter()
        .map(|c| c.email.as_str())
        .collect();
    let after_emails: BTreeSet<&str> = again
        .saved
        .contacts
        .iter()
        .map(|c| c.email.as_str())
        .collect();
    assert_eq!(before_emails, after_emails);

    // Photo metadata and photo bytes.
    let meta = env
        .store
        .my_photo_metadata()
        .await
        .expect("photo metadata")
        .expect("account has photo");
    assert_eq!(
        meta,
        PhotoMetadata {
            id: Some("240X240".into()),
            width: Some(240),
            height: Some(240),
        }
    );
    let my_bytes = env
        .store
        .contact_photo("outlook://me/photo")
        .await
        .expect("me photo")
        .expect("png bytes");
    assert!(my_bytes.starts_with(b"\x89PNG"), "valid PNG");

    let colleague_locator = contacts
        .directory
        .contacts
        .iter()
        .find_map(|c| c.photo.clone())
        .expect("directory user has photo locator");
    let colleague_bytes = env
        .store
        .contact_photo(&colleague_locator)
        .await
        .expect("colleague photo")
        .expect("colleague png");
    assert!(colleague_bytes.starts_with(b"\x89PNG"));
    assert_eq!(
        env.store
            .contact_photo("outlook://users/does-not-exist/photo")
            .await
            .expect("missing photo is None"),
        None
    );

    // 2. Personal Outlook account (403 on `/users/delta`) degrades directory cleanly.
    let personal_dir = TempDir::new();
    let personal = Store::open_with(
        personal_dir.path(),
        "microsoft personal@outlook.com",
        Remote::outlook(
            outlook::Endpoints::mock(&env.server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        unpaced(),
    )
    .await
    .expect("opens personal");
    env.fault(json!({
        "method": "GET",
        "path": "/v1.0/users/delta",
        "status": 403,
        "code": "Authorization_RequestDenied",
    }))
    .await;
    let (personal_contacts, personal_complete) = personal
        .sync_contacts()
        .await
        .expect("personal sync succeeds");
    assert!(
        personal_complete,
        "403 directory counts as empty, not failure"
    );
    assert!(personal_contacts.directory.contacts.is_empty());
    assert!(!personal_contacts.saved.contacts.is_empty());

    // 3. Gmail provider also supports `Store::sync_contacts`.
    let gmail_dir = TempDir::new();
    let gmail_store = Store::open_with(
        gmail_dir.path(),
        ADDRESS,
        Remote::gmail(
            gmail::Endpoints::mock(&env.server.url()),
            gmail::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        unpaced(),
    )
    .await
    .expect("opens gmail");
    let (gmail_contacts, gmail_complete) = gmail_store
        .sync_contacts()
        .await
        .expect("gmail syncs contacts");
    assert!(gmail_complete);
    assert!(!gmail_contacts.saved.contacts.is_empty());
    assert!(!gmail_contacts.directory.contacts.is_empty());
}

#[tokio::test]
async fn outlook_large_draft_upload_reports_progress_through_store_uploads() {
    let env = OutlookEnv::start(Config::default()).await;
    let draft_id = "draft00000000000077";
    let file_uuid = "file-deck-uuid";
    let size = 5 << 20;

    let email = gmail::mime::Email {
        from: Some(gmail::mime::Mailbox::new("Maya Okafor", ADDRESS)),
        to: vec![gmail::mime::Mailbox::new("Ann", "ann@example.com")],
        subject: "Big Deck".into(),
        html: Some("<div>Attached</div>".into()),
        text: Some("Attached".into()),
        message_id: Some(format!("{draft_id}@exo")),
        attachments: vec![gmail::mime::Attachment {
            filename: "deck.pdf".into(),
            content_type: "application/pdf".into(),
            data: vec![b'%'; size],
            content_id: None,
        }],
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
        subject: "Big Deck".into(),
        body: "<div>Attached</div>".into(),
        quoted_content: None,
        quoted_inlined: false,
        in_reply_to: None,
        in_reply_to_rfc822: None,
        references: Vec::new(),
        rfc822_id: format!("{draft_id}@exo"),
        fingerprint: String::new(),
        attachments: vec![DraftAttachment {
            uuid: file_uuid.into(),
            name: "deck.pdf".into(),
            mime_type: "application/pdf".into(),
            size: size as u64,
            content_id: None,
            source: AttachmentSource::Local,
        }],
        send_later: None,
        reminder: None,
    };
    env.store
        .save_draft(Draft {
            id: draft_id.into(),
            thread: None,
            adopted: None,
            record,
            raw: String::from_utf8(email.to_rfc822()).unwrap(),
            complete: true,
            sending: false,
        })
        .await
        .expect("saves draft");

    let mut uploads = env.store.uploads();
    let watcher = tokio::spawn(async move {
        let mut seen = Vec::new();
        while uploads.changed().await.is_ok() {
            let snapshot = uploads.borrow().clone();
            if let Some(entry) = snapshot.get(draft_id) {
                if let Some((_, pct)) = &entry.sending {
                    seen.push(*pct);
                }
                if entry.gmail_has.contains(file_uuid) && entry.sending.is_none() {
                    break;
                }
            }
        }
        seen
    });

    let report = env.store.push_drafts().await.expect("pushes large draft");
    assert_eq!(report.saved, [draft_id]);
    let seen = tokio::time::timeout(std::time::Duration::from_secs(5), watcher)
        .await
        .expect("watcher completes")
        .expect("watcher joins");
    assert!(
        seen.iter().any(|&pct| pct > 0.0 && pct < 100.0),
        "expected intermediate upload progress updates, observed: {seen:?}"
    );
    let final_uploads = env.store.uploads().borrow().clone();
    let final_entry = final_uploads.get(draft_id).expect("draft entry present");
    assert!(final_entry.gmail_has.contains(file_uuid));
    assert!(final_entry.sending.is_none());
}
