//! Regression tests for Outlook provider audit findings:
//! - Blocker #2: Graph changing `conversationId` when sending a new draft updates local rows &
//!   attaches follow-up reminders to the real sent conversation.
//! - Blocker #4: `Outlook::load` propagates 401/429 on well-known folders instead of swallowing
//!   non-404 `< 500` errors.
//! - Blocker #5: `Graph::each` fails fast on inner 429 (no storm), inner 401 (no blind 424 retry
//!   storm), and all-424 batches (no infinite loop).
//! - Blocker #6: Outlook search expands Exo markers (`is:snoozed`), selects `ROW` fields for
//!   custom Split Inbox routing, and refreshes master categories via `mailbox_for`.
//! - Blocker #9: Delta sync resolves move races by `lastModifiedDateTime` regardless of folder
//!   order, refreshes categories after `where_now`, and avoids false arrivals on folder moves or
//!   410 re-lists on an initially empty mailbox.
//! - Blocker #11: Whitespace-only `userPrincipalName` never creates an empty-email alias.

use crate::support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use exo_store::{
    Address, AttachmentSource, Definitions, Draft, DraftAction, DraftAttachment, DraftRecord,
    DraftReminder, Error, ListId, Mutation, ReminderStatus, Remote, Search, SenderRule,
    SenderRuleUndo, Settings, SplitInbox, Store, ThreadId,
};
use jiff::{Timestamp, ToSpan};
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::settings::{CategoryColor, OutlookCategory};
use serde_json::{Value, json};
use support::{TempDir, unpaced};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

type Handler = Arc<dyn Fn(&str, &str, &[u8]) -> (u16, Vec<(String, String)>, Value) + Send + Sync>;

/// Minimal HTTP/1.1 JSON stub server for deterministic Graph protocol edge-case tests.
struct StubGraph {
    url: Url,
    task: tokio::task::JoinHandle<()>,
}

impl StubGraph {
    async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let url = Url::parse(&format!("http://{addr}/")).expect("url");
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let header_end = loop {
                        let Ok(n) = stream.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let mut lines = head.lines();
                    let first = lines.next().unwrap_or_default();
                    let mut parts = first.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_owned();
                    let path = parts.next().unwrap_or_default().to_owned();
                    let mut content_len = 0usize;
                    for line in lines {
                        if let Some((name, val)) = line.split_once(':')
                            && name.trim().eq_ignore_ascii_case("content-length")
                        {
                            content_len = val.trim().parse().unwrap_or(0);
                        }
                    }
                    while buf.len() < header_end + content_len {
                        let Ok(n) = stream.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body = &buf[header_end..buf.len().min(header_end + content_len)];
                    let (status, extra_headers, json_body) = handler(&method, &path, body);
                    let payload = if status == 202 || status == 204 {
                        Vec::new()
                    } else {
                        serde_json::to_vec(&json_body).expect("json")
                    };
                    let reason = match status {
                        200 => "OK",
                        201 => "Created",
                        202 => "Accepted",
                        204 => "No Content",
                        401 => "Unauthorized",
                        404 => "Not Found",
                        410 => "Gone",
                        424 => "Failed Dependency",
                        429 => "Too Many Requests",
                        _ => "Error",
                    };
                    let mut response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                        payload.len()
                    );
                    for (k, v) in extra_headers {
                        response.push_str(&format!("{k}: {v}\r\n"));
                    }
                    response.push_str("\r\n");
                    let _ = stream.write_all(response.as_bytes()).await;
                    if !payload.is_empty() {
                        let _ = stream.write_all(&payload).await;
                    }
                });
            }
        });
        Self { url, task }
    }
}

impl Drop for StubGraph {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn ok(body: Value) -> (u16, Vec<(String, String)>, Value) {
    (200, Vec::new(), body)
}

fn batch_item(id: &str, status: u16, headers: Value, body: Value) -> Value {
    json!({
        "id": id,
        "status": status,
        "headers": headers,
        "body": body,
    })
}

async fn open_outlook(url: &Url) -> (TempDir, Store) {
    let dir = TempDir::new();
    let store = Store::open_with(
        dir.path(),
        "microsoft maya@brightmoor.co",
        Remote::outlook(
            outlook::Endpoints::mock(url),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        unpaced(),
    )
    .await
    .expect("opens");
    (dir, store)
}

#[tokio::test]
async fn blocker_2_send_reindexes_and_reminds_real_post_send_conversation_id() {
    let sent = Arc::new(AtomicBool::new(false));
    let sent_clone = sent.clone();
    let post_send_gets = Arc::new(AtomicUsize::new(0));
    let post_send_gets_clone = post_send_gets.clone();
    let base_url_slot: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
    let base_ref = base_url_slot.clone();

    let stub = StubGraph::start(Arc::new(move |method, path, body| {
        let base = base_ref.lock().unwrap().clone();
        if method == "POST" && path == "/v1.0/$batch" {
            let req: Value = serde_json::from_slice(body).unwrap();
            let mut responses = Vec::new();
            for item in req["requests"].as_array().unwrap() {
                let id = item["id"].as_str().unwrap();
                let url = item["url"].as_str().unwrap();
                if let Some(folder) = url.strip_prefix("/me/mailFolders/").and_then(|s| s.split('?').next()) {
                    if folder.ends_with("/messages/delta") {
                        let fid = folder.trim_end_matches("/messages/delta");
                        responses.push(batch_item(
                            id,
                            200,
                            json!({}),
                            json!({
                                "value": [],
                                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/{fid}/messages/delta?$deltatoken=d0"),
                            }),
                        ));
                    } else if matches!(folder, "inbox" | "drafts" | "sentitems" | "deleteditems" | "junkemail" | "archive") {
                        responses.push(batch_item(id, 200, json!({}), json!({ "id": format!("f-{folder}") })));
                    } else {
                        responses.push(batch_item(
                            id,
                            404,
                            json!({}),
                            json!({ "error": { "code": "ErrorItemNotFound", "message": "not found" } }),
                        ));
                    }
                } else {
                    responses.push(batch_item(id, 200, json!({}), json!({})));
                }
            }
            return ok(json!({ "responses": responses }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/mailFolders/delta") {
            return ok(json!({
                "value": [
                    { "id": "f-inbox", "displayName": "Inbox", "parentFolderId": "root" },
                    { "id": "f-drafts", "displayName": "Drafts", "parentFolderId": "root" },
                    { "id": "f-sentitems", "displayName": "Sent Items", "parentFolderId": "root" },
                    { "id": "f-deleteditems", "displayName": "Deleted Items", "parentFolderId": "root" },
                    { "id": "f-junkemail", "displayName": "Junk Email", "parentFolderId": "root" },
                    { "id": "f-archive", "displayName": "Archive", "parentFolderId": "root" },
                ],
                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/delta?$deltatoken=t0"),
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/outlook/masterCategories") {
            return ok(json!({ "value": [] }));
        }
        if method == "GET" && path.starts_with("/v1.0/me?") {
            return ok(json!({
                "id": "u1",
                "displayName": "Maya Okafor",
                "mail": "maya@brightmoor.co",
                "userPrincipalName": "maya@brightmoor.co",
            }));
        }
        if method == "POST" && path == "/v1.0/me/messages" {
            return (
                201,
                Vec::new(),
                json!({
                    "id": "msg-draft-1",
                    "conversationId": "conv-draft-old",
                    "parentFolderId": "f-drafts",
                    "isDraft": true,
                }),
            );
        }
        if method == "POST" && path == "/v1.0/me/messages/msg-draft-1/send" {
            sent_clone.store(true, Ordering::SeqCst);
            return (202, Vec::new(), Value::Null);
        }
        if method == "GET" && path.starts_with("/v1.0/me/messages/msg-draft-1") {
            let is_sent = sent_clone.load(Ordering::SeqCst);
            if is_sent {
                post_send_gets_clone.fetch_add(1, Ordering::SeqCst);
            }
            return ok(json!({
                "id": "msg-draft-1",
                "conversationId": if is_sent { "conv-sent-new" } else { "conv-draft-old" },
                "parentFolderId": if is_sent { "f-sentitems" } else { "f-drafts" },
                "isDraft": !is_sent,
                "isRead": true,
                "receivedDateTime": "2026-11-17T14:00:00Z",
                "lastModifiedDateTime": if is_sent { "2026-11-17T14:00:05Z" } else { "2026-11-17T14:00:00Z" },
                "subject": "New message",
                "bodyPreview": "Hello",
                "body": { "contentType": "html", "content": "<div>Hello</div>" },
                "from": { "emailAddress": { "name": "Maya Okafor", "address": "maya@brightmoor.co" } },
                "toRecipients": [{ "emailAddress": { "name": "Ann", "address": "ann@example.com" } }],
                "internetMessageId": "<d-blocker2@exo>",
                "extensions": [],
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/messages?") {
            // Conversation lookup or backfill list.
            if path.contains("conv-draft-old") && !sent_clone.load(Ordering::SeqCst) {
                return ok(json!({
                    "value": [{
                        "id": "msg-draft-1",
                        "conversationId": "conv-draft-old",
                        "parentFolderId": "f-drafts",
                        "isDraft": true,
                        "isRead": true,
                        "receivedDateTime": "2026-11-17T14:00:00Z",
                        "lastModifiedDateTime": "2026-11-17T14:00:00Z",
                        "subject": "New message",
                        "bodyPreview": "Hello",
                        "from": { "emailAddress": { "name": "Maya Okafor", "address": "maya@brightmoor.co" } },
                        "toRecipients": [{ "emailAddress": { "name": "Ann", "address": "ann@example.com" } }],
                        "internetMessageId": "<d-blocker2@exo>",
                        "extensions": [],
                    }]
                }));
            }
            return ok(json!({ "value": [] }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    *base_url_slot.lock().unwrap() = stub.url.to_string();

    let (_dir, store) = open_outlook(&stub.url).await;
    store.sync().await.expect("initial sync");

    let remind_at = Timestamp::now()
        .checked_add(24.hours())
        .unwrap()
        .round(jiff::Unit::Second)
        .unwrap();
    let email = gmail::mime::Email {
        from: Some(gmail::mime::Mailbox::new(
            "Maya Okafor",
            "maya@brightmoor.co",
        )),
        to: vec![gmail::mime::Mailbox::new("Ann", "ann@example.com")],
        subject: "New message".into(),
        html: Some("<div>Hello</div>".into()),
        text: Some("Hello".into()),
        message_id: Some("d-blocker2@exo".into()),
        ..gmail::mime::Email::default()
    };
    let record = DraftRecord {
        action: DraftAction::Compose,
        from: Some(Address {
            name: Some("Maya Okafor".into()),
            email: "maya@brightmoor.co".into(),
        }),
        to: vec![Address {
            name: Some("Ann".into()),
            email: "ann@example.com".into(),
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "New message".into(),
        body: "<div>Hello</div>".into(),
        quoted_content: None,
        quoted_inlined: false,
        in_reply_to: None,
        in_reply_to_rfc822: None,
        references: Vec::new(),
        rfc822_id: "d-blocker2@exo".into(),
        fingerprint: String::new(),
        attachments: Vec::new(),
        send_later: None,
        reminder: Some(DraftReminder {
            at: remind_at,
            keep_on_reply: false,
            automatic: false,
        }),
    };
    store
        .save_draft(Draft {
            id: "draft-b2".into(),
            thread: None,
            adopted: None,
            record,
            raw: String::from_utf8(email.to_rfc822()).unwrap(),
            complete: true,
            sending: true,
        })
        .await
        .expect("saves");
    store.push_drafts().await.expect("creates draft");
    // Draft is initially cached under `conv-draft-old`.
    assert!(
        store
            .thread(&exo_store::ThreadId::new("conv-draft-old"))
            .await
            .unwrap()
            .is_some()
    );

    store
        .send_draft("draft-b2", Timestamp::now())
        .await
        .expect("marks send");
    let report = store.push_drafts().await.expect("sends");
    assert_eq!(report.sent.len(), 1);
    assert_eq!(
        report.sent[0].thread.as_str(),
        "conv-sent-new",
        "SentDraft reports new conversationId"
    );
    assert_eq!(
        post_send_gets.load(Ordering::SeqCst),
        1,
        "only one post-send GET resolves both full message and real conversationId"
    );
    assert!(
        store
            .thread(&exo_store::ThreadId::new("conv-draft-old"))
            .await
            .unwrap()
            .is_none(),
        "old draft thread removed"
    );
    let sent_thread = store
        .thread(&exo_store::ThreadId::new("conv-sent-new"))
        .await
        .unwrap()
        .expect("new conversation indexed");
    assert_eq!(sent_thread.messages.len(), 1);
    assert_eq!(sent_thread.messages[0].id, "msg-draft-1");
    let reminder_rows = store.list(&ListId::Reminders, None, 20).await.unwrap();
    assert_eq!(reminder_rows.threads.len(), 1);
    assert_eq!(reminder_rows.threads[0].id.as_str(), "conv-sent-new");
    assert_eq!(
        reminder_rows.threads[0].reminder,
        Some(ReminderStatus::Set {
            at: remind_at,
            keep_on_reply: false
        })
    );
}

#[tokio::test]
async fn blocker_4_and_5_batch_errors_never_swallow_auth_or_storm_or_loop() {
    // 1. Inner 429 + 424s: must fail with Throttled in a SINGLE `$batch` call (no storm).
    let calls_429 = Arc::new(AtomicUsize::new(0));
    let c429 = calls_429.clone();
    let stub_429 = StubGraph::start(Arc::new(move |method, path, body| {
        if method == "POST" && path == "/v1.0/$batch" {
            c429.fetch_add(1, Ordering::SeqCst);
            let req: Value = serde_json::from_slice(body).unwrap();
            let responses: Vec<Value> = req["requests"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let id = item["id"].as_str().unwrap();
                    if i == 0 {
                        batch_item(
                            id,
                            429,
                            json!({ "Retry-After": "120" }),
                            json!({ "error": { "code": "ApplicationThrottled", "message": "slow down" } }),
                        )
                    } else {
                        batch_item(
                            id,
                            424,
                            json!({}),
                            json!({ "error": { "code": "FailedDependency", "message": "skipped" } }),
                        )
                    }
                })
                .collect();
            return ok(json!({ "responses": responses }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    let (_d1, store_429) = open_outlook(&stub_429.url).await;
    let err = store_429
        .sync()
        .await
        .expect_err("inner 429 must fail load");
    assert!(matches!(err, Error::Throttled { .. }), "{err}");
    assert_eq!(
        calls_429.load(Ordering::SeqCst),
        1,
        "no 429 retry storm across 424 items"
    );

    // 2. All-424 batch: must terminate cleanly in ONE `$batch` call (no infinite loop).
    let calls_424 = Arc::new(AtomicUsize::new(0));
    let c424 = calls_424.clone();
    let stub_424 = StubGraph::start(Arc::new(move |method, path, body| {
        if method == "POST" && path == "/v1.0/$batch" {
            c424.fetch_add(1, Ordering::SeqCst);
            let req: Value = serde_json::from_slice(body).unwrap();
            let responses: Vec<Value> = req["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    batch_item(
                        item["id"].as_str().unwrap(),
                        424,
                        json!({}),
                        json!({ "error": { "code": "FailedDependency", "message": "stuck" } }),
                    )
                })
                .collect();
            return ok(json!({ "responses": responses }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    let (_d2, store_424) = open_outlook(&stub_424.url).await;
    let err = store_424.sync().await.expect_err("all-424 batch must fail");
    assert!(matches!(err, Error::Outlook(_)), "{err}");
    assert_eq!(
        calls_424.load(Ordering::SeqCst),
        1,
        "all-424 breaks immediately without looping"
    );

    // 3. Persistent inner 401 + 424s: fails Unauthorized cleanly without swallowing 401 on load.
    let calls_401 = Arc::new(AtomicUsize::new(0));
    let c401 = calls_401.clone();
    let stub_401 = StubGraph::start(Arc::new(move |method, path, body| {
        if method == "POST" && path == "/v1.0/$batch" {
            c401.fetch_add(1, Ordering::SeqCst);
            let req: Value = serde_json::from_slice(body).unwrap();
            let responses: Vec<Value> = req["requests"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let id = item["id"].as_str().unwrap();
                    if i == 0 {
                        batch_item(
                            id,
                            401,
                            json!({}),
                            json!({ "error": { "code": "InvalidAuthenticationToken", "message": "expired" } }),
                        )
                    } else {
                        batch_item(
                            id,
                            424,
                            json!({}),
                            json!({ "error": { "code": "FailedDependency", "message": "skipped" } }),
                        )
                    }
                })
                .collect();
            return ok(json!({ "responses": responses }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    let (_d3, store_401) = open_outlook(&stub_401.url).await;
    let err = store_401
        .sync()
        .await
        .expect_err("inner 401 must not be swallowed");
    assert!(matches!(err, Error::Unauthorized(_)), "{err}");
    assert!(
        calls_401.load(Ordering::SeqCst) <= 2,
        "at most one token-refresh retry, never N 424 retries"
    );

    // 4. Inner 403 on well-known folder `inbox`: must fail (Blocker #4: only 404 is ignored).
    let stub_403 = StubGraph::start(Arc::new(move |method, path, body| {
        if method == "POST" && path == "/v1.0/$batch" {
            let req: Value = serde_json::from_slice(body).unwrap();
            let responses: Vec<Value> = req["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    let id = item["id"].as_str().unwrap();
                    let url = item["url"].as_str().unwrap();
                    if url.contains("/inbox") {
                        batch_item(
                            id,
                            403,
                            json!({}),
                            json!({ "error": { "code": "ErrorAccessDenied", "message": "denied" } }),
                        )
                    } else {
                        batch_item(id, 404, json!({}), json!({ "error": { "code": "ErrorItemNotFound", "message": "nope" } }))
                    }
                })
                .collect();
            return ok(json!({ "responses": responses }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    let (_d4, store_403) = open_outlook(&stub_403.url).await;
    let err = store_403
        .sync()
        .await
        .expect_err("inner 403 on inbox must fail load");
    assert!(matches!(err, Error::Outlook(_)), "{err}");
}

#[tokio::test]
async fn blocker_6_outlook_search_expands_markers_routes_splits_and_refreshes_categories() {
    let server = MockServer::start(Config::default()).await.expect("mock");
    let (_dir, store) = open_outlook(&server.url()).await;
    for _ in 0..80 {
        if !store.sync().await.expect("sync").backfilling {
            break;
        }
    }
    let graph = outlook::Client::new(
        outlook::Endpoints::mock(&server.url()),
        outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
    );

    let inbox = store.list(&ListId::Inbox, None, 50).await.unwrap().threads;
    let target = inbox[0].id.clone();
    let detail = store.thread(&target).await.unwrap().expect("detail");
    let sender = detail
        .messages
        .iter()
        .filter_map(|m| m.from.as_ref().map(|s| s.email.clone()))
        .find(|email| !email.eq_ignore_ascii_case("maya@brightmoor.co"))
        .expect("sender");

    // 1. Add a custom Split matching `from:{sender}` and verify remote Outlook search routes it.
    let mut settings: Settings = store.settings().await.unwrap();
    let labels = store.labels().await.unwrap();
    let me = vec!["maya@brightmoor.co".to_owned()];
    let split = SplitInbox::new(
        "VIP",
        &format!("from:{sender}"),
        None,
        None,
        Definitions {
            labels: &labels,
            me: &me,
        },
    );
    let split_id = split.id.clone();
    settings.add_split(split);
    store.save_settings(&settings).await.unwrap();
    // Search remotely via `find_threads` (hits `Outlook::searched` + `hit()`).
    let split_hits = store
        .find_threads(&format!("in:{split_id}"))
        .await
        .expect("split search");
    assert!(
        split_hits.contains(&target),
        "ROW fields allow custom split routing on remote search hits: {split_hits:?}"
    );

    // 2. Snooze a thread and verify remote search finds `is:snoozed` via expanded extensions.
    let snooze_target = inbox[1].id.clone();
    let at = Timestamp::now().checked_add(12.hours()).unwrap();
    store
        .apply(
            std::slice::from_ref(&snooze_target),
            Mutation::Remind {
                at,
                keep_on_reply: false,
                snooze: true,
            },
        )
        .await
        .unwrap();
    store.flush().await.unwrap();
    let snoozed_query = format!("label:\"{}\"", exo_store::labels::EXO_SNOOZED);
    let snoozed_hits = store
        .find_threads(&snoozed_query)
        .await
        .expect("snoozed search");
    assert!(
        snoozed_hits.contains(&snooze_target),
        "remote search expands build.miguel.exo.snoozed: {snoozed_hits:?}"
    );

    // 3. Create a brand-new category directly on Graph after sync, assign it to a message, and
    // verify remote search refreshes the master category list via `mailbox_for`.
    let created_cat = graph
        .settings()
        .create_category(&OutlookCategory {
            id: None,
            display_name: Some("UrgentAudit".into()),
            color: Some(CategoryColor::Preset(0)),
        })
        .await
        .expect("creates category");
    let cat_id = created_cat.id.expect("category id");
    let msg_id = detail.messages[0].id.clone();
    graph
        .mail()
        .update_message(
            &msg_id,
            &outlook::MessagePatch {
                categories: Some(vec!["UrgentAudit".into()]),
                ..outlook::MessagePatch::default()
            },
        )
        .await
        .expect("tags message");
    // Search via `Store::search` with `remote = true` and `label:<cat_id>` (or `find_threads`).
    let query = format!("label:{cat_id}");
    let mut search = Search::new(&query);
    let _ = store
        .search(&mut search, 50, true, 50)
        .await
        .expect("search");
    let cat_hits = store
        .find_threads(&format!("label:{cat_id}"))
        .await
        .expect("category search");
    assert!(
        cat_hits.contains(&target),
        "mailbox_for picked up new category {cat_id} during search: {cat_hits:?}"
    );
}

#[tokio::test]
async fn blocker_9_and_11_delta_move_race_where_now_category_no_false_arrival_and_empty_upn() {
    let round = Arc::new(AtomicUsize::new(0));
    let round_clone = round.clone();
    let base_slot: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let base_ref = base_slot.clone();

    let stub = StubGraph::start(Arc::new(move |method, path, body| {
        let base = base_ref.lock().unwrap().clone();
        let phase = round_clone.load(Ordering::SeqCst);
        if method == "POST" && path == "/v1.0/$batch" {
            let req: Value = serde_json::from_slice(body).unwrap();
            let mut responses = Vec::new();
            for item in req["requests"].as_array().unwrap() {
                let id = item["id"].as_str().unwrap();
                let url = item["url"].as_str().unwrap();
                if url.contains("/messages/delta") {
                    if phase >= 1 && url.contains("deltatoken=d0") {
                        // Phase 1:
                        // - `f-aaa-archive` (lexicographically earlier!) has NEWER state of `m1`
                        //   moved to archive at 14:00:10Z.
                        // - `f-zzz-inbox` (lexicographically later!) STILL has OLDER state of `m1`
                        //   at 14:00:05Z (simulating a move race!), PLUS `@removed` for `m2`
                        //   which moved elsewhere and will be resolved via `where_now` with a brand
                        //   new category `"FreshCat"`.
                        // - `f-quiet` returns 410 SyncStateNotFound on phase 1!
                        if url.contains("/f-aaa-archive/") {
                            responses.push(batch_item(
                                id,
                                200,
                                json!({}),
                                json!({
                                    "value": [{
                                        "id": "m1",
                                        "conversationId": "c1",
                                        "parentFolderId": "f-aaa-archive",
                                        "receivedDateTime": "2026-11-17T14:00:00Z",
                                        "lastModifiedDateTime": "2026-11-17T14:00:10Z",
                                        "isRead": true,
                                        "isDraft": false,
                                        "categories": [],
                                        "extensions": [],
                                    }],
                                    "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/f-aaa-archive/messages/delta?$deltatoken=d1"),
                                }),
                            ));
                        } else if url.contains("/f-zzz-inbox/") {
                            responses.push(batch_item(
                                id,
                                200,
                                json!({}),
                                json!({
                                    "value": [
                                        {
                                            "id": "m1",
                                            "conversationId": "c1",
                                            "parentFolderId": "f-zzz-inbox",
                                            "receivedDateTime": "2026-11-17T14:00:00Z",
                                            "lastModifiedDateTime": "2026-11-17T14:00:05Z",
                                            "isRead": false,
                                            "isDraft": false,
                                            "categories": [],
                                            "extensions": [],
                                        },
                                        {
                                            "id": "m2",
                                            "@removed": { "reason": "deleted" },
                                        },
                                    ],
                                    "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/f-zzz-inbox/messages/delta?$deltatoken=d1"),
                                }),
                            ));
                        } else if url.contains("/f-quiet/") {
                            responses.push(batch_item(
                                id,
                                410,
                                json!({}),
                                json!({ "error": { "code": "SyncStateNotFound", "message": "expired" } }),
                            ));
                        } else {
                            responses.push(batch_item(
                                id,
                                200,
                                json!({}),
                                json!({
                                    "value": [],
                                    "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/f-other/messages/delta?$deltatoken=d1"),
                                }),
                            ));
                        }
                    } else if url.contains("/f-quiet/") && phase >= 1 {
                        // 410 re-list of `f-quiet`: contains pre-existing `m3` from before; must
                        // NOT be marked as a fresh arrival even though mailbox started empty!
                        responses.push(batch_item(
                            id,
                            200,
                            json!({}),
                            json!({
                                "value": [{
                                    "id": "m3",
                                    "conversationId": "c3",
                                    "parentFolderId": "f-quiet",
                                    "receivedDateTime": "2026-11-17T12:00:00Z",
                                    "lastModifiedDateTime": "2026-11-17T12:00:00Z",
                                    "isRead": true,
                                    "isDraft": false,
                                    "categories": [],
                                    "extensions": [],
                                }],
                                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/f-quiet/messages/delta?$deltatoken=d1"),
                            }),
                        ));
                    } else {
                        let fid = url
                            .strip_prefix("/me/mailFolders/")
                            .and_then(|s| s.split('/').next())
                            .unwrap_or("f-zzz-inbox");
                        responses.push(batch_item(
                            id,
                            200,
                            json!({}),
                            json!({
                                "value": [],
                                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/{fid}/messages/delta?$deltatoken=d0"),
                            }),
                        ));
                    }
                } else if url.starts_with("/me/messages/m2") {
                    // `where_now` resolves `m2` carrying newly created category `"FreshCat"`.
                    responses.push(batch_item(
                        id,
                        200,
                        json!({}),
                        json!({
                            "id": "m2",
                            "conversationId": "c2",
                            "parentFolderId": "f-aaa-archive",
                            "receivedDateTime": "2026-11-17T13:00:00Z",
                            "lastModifiedDateTime": "2026-11-17T14:00:12Z",
                            "isRead": true,
                            "isDraft": false,
                            "categories": ["FreshCat"],
                            "extensions": [],
                        }),
                    ));
                } else if let Some(folder) = url
                    .strip_prefix("/me/mailFolders/")
                    .and_then(|s| s.split('?').next())
                {
                    let mapped = match folder {
                        "inbox" => Some("f-zzz-inbox"),
                        "archive" => Some("f-aaa-archive"),
                        _ => None,
                    };
                    match mapped {
                        Some(fid) => responses.push(batch_item(id, 200, json!({}), json!({ "id": fid }))),
                        None => responses.push(batch_item(
                            id,
                            404,
                            json!({}),
                            json!({ "error": { "code": "ErrorItemNotFound", "message": "none" } }),
                        )),
                    }
                } else {
                    responses.push(batch_item(id, 200, json!({}), json!({})));
                }
            }
            return ok(json!({ "responses": responses }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/mailFolders/delta") {
            if path.contains("deltatoken=") {
                return ok(json!({
                    "value": [],
                    "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/delta?$deltatoken=t1"),
                }));
            }
            return ok(json!({
                "value": [
                    { "id": "f-zzz-inbox", "displayName": "Inbox", "parentFolderId": "root" },
                    { "id": "f-aaa-archive", "displayName": "Archive", "parentFolderId": "root" },
                    { "id": "f-quiet", "displayName": "Quiet", "parentFolderId": "root" },
                ],
                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/delta?$deltatoken=t0"),
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/outlook/masterCategories") {
            let cats = if phase >= 1 {
                json!([{ "id": "cat-fresh", "displayName": "FreshCat", "color": "preset0" }])
            } else {
                json!([])
            };
            return ok(json!({ "value": cats }));
        }
        if method == "GET" && path.starts_with("/v1.0/me?") {
            // Blocker #11: `mail: null` and whitespace-only `userPrincipalName`!
            return ok(json!({
                "id": "u1",
                "displayName": "Maya",
                "mail": null,
                "userPrincipalName": "   ",
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/messages?") {
            // Conversation fetches for c1, c2, c3 during history apply.
            if path.contains("%27c1%27") || path.contains("'c1'") {
                return ok(json!({
                    "value": [{
                        "id": "m1",
                        "conversationId": "c1",
                        "parentFolderId": "f-aaa-archive",
                        "receivedDateTime": "2026-11-17T14:00:00Z",
                        "lastModifiedDateTime": "2026-11-17T14:00:10Z",
                        "isRead": true,
                        "isDraft": false,
                        "subject": "Moved",
                        "bodyPreview": "Moved",
                        "from": { "emailAddress": { "name": "Ann", "address": "ann@example.com" } },
                        "categories": [],
                        "extensions": [],
                    }]
                }));
            }
            if path.contains("%27c2%27") || path.contains("'c2'") {
                return ok(json!({
                    "value": [{
                        "id": "m2",
                        "conversationId": "c2",
                        "parentFolderId": "f-aaa-archive",
                        "receivedDateTime": "2026-11-17T13:00:00Z",
                        "lastModifiedDateTime": "2026-11-17T14:00:12Z",
                        "isRead": true,
                        "isDraft": false,
                        "subject": "Categorized Move",
                        "bodyPreview": "Categorized",
                        "from": { "emailAddress": { "name": "Ann", "address": "ann@example.com" } },
                        "categories": ["FreshCat"],
                        "extensions": [],
                    }]
                }));
            }
            if path.contains("%27c3%27") || path.contains("'c3'") {
                return ok(json!({
                    "value": [{
                        "id": "m3",
                        "conversationId": "c3",
                        "parentFolderId": "f-quiet",
                        "receivedDateTime": "2026-11-17T12:00:00Z",
                        "lastModifiedDateTime": "2026-11-17T12:00:00Z",
                        "isRead": true,
                        "isDraft": false,
                        "subject": "Old Quiet",
                        "bodyPreview": "Quiet",
                        "from": { "emailAddress": { "name": "Ann", "address": "ann@example.com" } },
                        "categories": [],
                        "extensions": [],
                    }]
                }));
            }
            return ok(json!({ "value": [] }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    *base_slot.lock().unwrap() = stub.url.to_string();

    let (_dir, store) = open_outlook(&stub.url).await;
    // Initial sync on an empty mailbox.
    store.sync().await.expect("initial sync");

    // Blocker #11: whitespace UPN + null mail must NOT produce an empty-email alias.
    assert!(
        store.aliases().await.unwrap().is_empty(),
        "empty/whitespace UPN never creates an empty-email Alias"
    );

    // Phase 1 delta sync: exercises move race, where_now + new category, and 410 re-list.
    round.store(1, Ordering::SeqCst);
    let report = store.sync().await.expect("delta sync");

    // Neither the moved `m1`, the `where_now`-resolved `m2`, nor the 410 re-listed `m3` should
    // trigger false arrival notifications!
    assert!(
        report.arrived.is_empty(),
        "no false arrivals from moves or 410 re-list on empty mailbox: {:?}",
        report.arrived
    );

    // `m1` kept its newer Archive state (Done, NOT stuck in Inbox from older `f-zzz-inbox` entry).
    let inbox_ids: Vec<String> = store
        .list(&ListId::Inbox, None, 20)
        .await
        .unwrap()
        .threads
        .into_iter()
        .map(|t| t.id.as_str().to_owned())
        .collect();
    assert!(
        !inbox_ids.contains(&"c1".to_owned()),
        "newer Archive state won over older Inbox delta entry: {inbox_ids:?}"
    );
    let c1 = store
        .thread(&exo_store::ThreadId::new("c1"))
        .await
        .unwrap()
        .expect("c1 synced");
    assert!(!c1.messages[0].labels.contains(&"INBOX".to_owned()));

    // `m2` resolved via `where_now` picked up the newly created `"FreshCat"` category (`cat-fresh`).
    let c2 = store
        .thread(&exo_store::ThreadId::new("c2"))
        .await
        .unwrap()
        .expect("c2 synced");
    assert!(
        c2.messages[0].labels.contains(&"cat-fresh".to_owned()),
        "mailbox_for ran after where_now and applied FreshCat: {:?}",
        c2.messages[0].labels
    );

    // `m3` from the 410 re-list WAS synced into SQLite cleanly (just not flagged as arrival).
    assert!(
        store
            .thread(&exo_store::ThreadId::new("c3"))
            .await
            .unwrap()
            .is_some(),
        "410 re-listed thread stored"
    );
}

#[tokio::test]
async fn contacts_delta_handles_stale_400_mid_page_410_and_repeated_ids() {
    let phase = Arc::new(AtomicUsize::new(0));
    let phase_ref = phase.clone();
    let base_slot: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let base_ref = base_slot.clone();

    let stub = StubGraph::start(Arc::new(move |method, path, _body| {
        let base = base_ref.lock().unwrap().clone();
        let step = phase_ref.load(Ordering::SeqCst);
        if method == "GET" && path.starts_with("/v1.0/me/contacts/delta") {
            if step == 0 {
                // Initial contacts sync spans two pages with repeated IDs:
                // - c-gone appears on page 1, then @removed on page 2 (must NOT resurrect)
                // - c-dup appears on page 1 (v1), then page 2 (v2) (must NOT duplicate)
                if path.contains("skiptoken=p2") {
                    return ok(json!({
                        "value": [
                            { "id": "c-gone", "@removed": { "reason": "deleted" } },
                            {
                                "id": "c-dup",
                                "displayName": "Dup V2",
                                "emailAddresses": [{ "address": "dup-v2@example.com" }],
                            },
                        ],
                        "@odata.deltaLink": format!("{base}v1.0/me/contacts/delta?$deltatoken=c-d0"),
                    }));
                }
                return ok(json!({
                    "value": [
                        {
                            "id": "c-gone",
                            "displayName": "Ghost",
                            "emailAddresses": [{ "address": "ghost@example.com" }],
                        },
                        {
                            "id": "c-dup",
                            "displayName": "Dup V1",
                            "emailAddresses": [{ "address": "dup-v1@example.com" }],
                        },
                    ],
                    "@odata.nextLink": format!("{base}v1.0/me/contacts/delta?$skiptoken=p2"),
                }));
            }
            // Step 1: incremental contacts delta expires MID-PAGE (page 2 returns 410),
            // which must trigger a full re-list rather than failing the sync.
            if path.contains("deltatoken=c-d0") {
                return ok(json!({
                    "value": [{
                        "id": "c-temp",
                        "displayName": "Temp",
                        "emailAddresses": [{ "address": "temp@example.com" }],
                    }],
                    "@odata.nextLink": format!("{base}v1.0/me/contacts/delta?$skiptoken=expired-p2"),
                }));
            }
            if path.contains("skiptoken=expired-p2") {
                return (
                    410,
                    Vec::new(),
                    json!({ "error": { "code": "SyncStateNotFound", "message": "expired mid-page" } }),
                );
            }
            return ok(json!({
                "value": [{
                    "id": "c-relisted",
                    "displayName": "Relisted Contact",
                    "emailAddresses": [{ "address": "relisted@example.com" }],
                }],
                "@odata.deltaLink": format!("{base}v1.0/me/contacts/delta?$deltatoken=c-d1"),
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/users/delta") {
            if step == 0 {
                return ok(json!({
                    "value": [{
                        "id": "u-1",
                        "displayName": "Alice",
                        "mail": "alice@brightmoor.co",
                    }],
                    "@odata.deltaLink": format!("{base}v1.0/users/delta?$deltatoken=u-d0"),
                }));
            }
            // Step 1: stale `$deltatoken=u-d0` returns HTTP 400; must re-list full directory
            // instead of wiping the cached directory as Unavailable.
            if path.contains("deltatoken=u-d0") {
                return (
                    400,
                    Vec::new(),
                    json!({ "error": { "code": "Request_BadRequest", "message": "stale token" } }),
                );
            }
            return ok(json!({
                "value": [
                    { "id": "u-1", "displayName": "Alice", "mail": "alice@brightmoor.co" },
                    { "id": "u-2", "displayName": "Bob", "mail": "bob@brightmoor.co" },
                    { "id": "u-2", "@removed": { "reason": "deleted" } },
                ],
                "@odata.deltaLink": format!("{base}v1.0/users/delta?$deltatoken=u-d1"),
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/people") {
            return ok(json!({ "value": [] }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    *base_slot.lock().unwrap() = stub.url.to_string();

    let (_dir, store) = open_outlook(&stub.url).await;

    // Initial sync: collapses repeated IDs across pages without duplicates or resurrection.
    let (first, complete) = store.sync_contacts().await.expect("initial contacts sync");
    assert!(complete);
    let saved_emails: Vec<&str> = first
        .saved
        .contacts
        .iter()
        .map(|c| c.email.as_str())
        .collect();
    assert_eq!(saved_emails, ["dup-v2@example.com"]);
    let dir_emails: Vec<&str> = first
        .directory
        .contacts
        .iter()
        .map(|c| c.email.as_str())
        .collect();
    assert_eq!(dir_emails, ["alice@brightmoor.co"]);

    // Second sync:
    // - `/me/contacts/delta` expires mid-page (410 on page 2) -> falls back to full re-list.
    // - `/users/delta` rejects stale `$deltatoken` with 400 -> falls back to full re-list (not
    //   wiped!) and collapses `[u-2, @removed(u-2)]` cleanly.
    phase.store(1, Ordering::SeqCst);
    let (second, complete) = store.sync_contacts().await.expect("recovery contacts sync");
    assert!(complete);
    let saved_emails: Vec<&str> = second
        .saved
        .contacts
        .iter()
        .map(|c| c.email.as_str())
        .collect();
    assert_eq!(saved_emails, ["relisted@example.com"]);
    let dir_emails: Vec<&str> = second
        .directory
        .contacts
        .iter()
        .map(|c| c.email.as_str())
        .collect();
    assert_eq!(dir_emails, ["alice@brightmoor.co"]);
}

#[tokio::test]
async fn upload_session_401_is_transient_not_logged_out() {
    let upload_puts = Arc::new(AtomicUsize::new(0));
    let upload_puts_ref = upload_puts.clone();
    let base_slot: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let base_ref = base_slot.clone();

    let stub = StubGraph::start(Arc::new(move |method, path, body| {
        let base = base_ref.lock().unwrap().clone();
        if method == "POST" && path == "/v1.0/$batch" {
            let req: Value = serde_json::from_slice(body).unwrap();
            let responses: Vec<Value> = req["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    let id = item["id"].as_str().unwrap();
                    let url = item["url"].as_str().unwrap();
                    if let Some(folder) = url
                        .strip_prefix("/me/mailFolders/")
                        .and_then(|s| s.split('?').next())
                    {
                        if folder.ends_with("/messages/delta") {
                            let fid = folder.trim_end_matches("/messages/delta");
                            batch_item(
                                id,
                                200,
                                json!({}),
                                json!({
                                    "value": [],
                                    "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/{fid}/messages/delta?$deltatoken=d0"),
                                }),
                            )
                        } else if matches!(
                            folder,
                            "inbox" | "drafts" | "sentitems" | "deleteditems" | "junkemail" | "archive"
                        ) {
                            batch_item(id, 200, json!({}), json!({ "id": format!("f-{folder}") }))
                        } else {
                            batch_item(
                                id,
                                404,
                                json!({}),
                                json!({ "error": { "code": "ErrorItemNotFound", "message": "nope" } }),
                            )
                        }
                    } else {
                        batch_item(id, 200, json!({}), json!({}))
                    }
                })
                .collect();
            return ok(json!({ "responses": responses }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/mailFolders/delta") {
            return ok(json!({
                "value": [
                    { "id": "f-inbox", "displayName": "Inbox", "parentFolderId": "root" },
                    { "id": "f-drafts", "displayName": "Drafts", "parentFolderId": "root" },
                ],
                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/delta?$deltatoken=t0"),
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me?") {
            return ok(json!({
                "id": "u1",
                "displayName": "Maya",
                "mail": "maya@brightmoor.co",
            }));
        }
        if method == "POST" && path == "/v1.0/me/messages" {
            return (
                201,
                Vec::new(),
                json!({
                    "id": "draft-big-1",
                    "conversationId": "conv-big-1",
                    "parentFolderId": "f-drafts",
                    "isDraft": true,
                }),
            );
        }
        if method == "POST" && path == "/v1.0/me/messages/draft-big-1/attachments/createUploadSession"
        {
            return (
                201,
                Vec::new(),
                json!({
                    "uploadUrl": format!("{base}upload/session-expired-1"),
                    "expirationDateTime": "2026-11-17T15:00:00Z",
                }),
            );
        }
        if method == "PUT" && path.starts_with("/upload/session-expired-1") {
            upload_puts_ref.fetch_add(1, Ordering::SeqCst);
            return (
                401,
                Vec::new(),
                json!({
                    "error": {
                        "code": "InvalidAuthenticationToken",
                        "message": "upload session token expired",
                    }
                }),
            );
        }
        if method == "DELETE" && path == "/v1.0/me/messages/draft-big-1" {
            return (204, Vec::new(), Value::Null);
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    *base_slot.lock().unwrap() = stub.url.to_string();

    let (_dir, store) = open_outlook(&stub.url).await;
    store.sync().await.expect("initial sync");

    let size = 3 * 1024 * 1024 + 4096;
    let email = gmail::mime::Email {
        from: Some(gmail::mime::Mailbox::new("Maya", "maya@brightmoor.co")),
        to: vec![gmail::mime::Mailbox::new("Ann", "ann@example.com")],
        subject: "Expired Upload Session".into(),
        html: Some("<div>Attached</div>".into()),
        message_id: Some("d-upload-401@exo".into()),
        attachments: vec![gmail::mime::Attachment {
            filename: "deck.bin".into(),
            content_type: "application/octet-stream".into(),
            data: vec![b'x'; size],
            content_id: None,
        }],
        ..gmail::mime::Email::default()
    };
    let record = DraftRecord {
        action: DraftAction::Compose,
        from: Some(Address {
            name: Some("Maya".into()),
            email: "maya@brightmoor.co".into(),
        }),
        to: vec![Address {
            name: Some("Ann".into()),
            email: "ann@example.com".into(),
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "Expired Upload Session".into(),
        body: "<div>Attached</div>".into(),
        quoted_content: None,
        quoted_inlined: false,
        in_reply_to: None,
        in_reply_to_rfc822: None,
        references: Vec::new(),
        rfc822_id: "d-upload-401@exo".into(),
        fingerprint: String::new(),
        attachments: vec![DraftAttachment {
            uuid: "u-deck".into(),
            name: "deck.bin".into(),
            mime_type: "application/octet-stream".into(),
            size: size as u64,
            content_id: None,
            source: AttachmentSource::Local,
        }],
        send_later: None,
        reminder: None,
    };
    store
        .save_draft(Draft {
            id: "draft-upload-401".into(),
            thread: None,
            adopted: None,
            record,
            raw: String::from_utf8(email.to_rfc822()).unwrap(),
            complete: true,
            sending: true,
        })
        .await
        .expect("saves draft");
    store
        .send_draft("draft-upload-401", Timestamp::now())
        .await
        .expect("marks due send");

    let report = store.push_drafts().await.expect("pushes");
    assert!(
        report.failed.is_empty(),
        "upload session 401 must NOT fail the send as LoggedOut: {:?}",
        report.failed
    );
    assert_eq!(
        report.retrying,
        ["draft-upload-401"],
        "upload session 401 schedules a retry with a fresh session"
    );
    let stored = store
        .draft("draft-upload-401")
        .await
        .unwrap()
        .expect("draft still queued");
    assert_eq!(stored.failure, None, "no LoggedOut failure stored on draft");
    assert!(stored.send_at.is_some(), "send stays queued for retry");
    assert_eq!(
        upload_puts.load(Ordering::SeqCst),
        1,
        "expired upload session URL is not retried"
    );
}

#[tokio::test]
async fn same_folder_multipage_delta_preserves_arrivals_and_respects_page2_removed() {
    let phase = Arc::new(AtomicUsize::new(0));
    let phase_ref = phase.clone();
    let where_now_m_gone = Arc::new(AtomicUsize::new(0));
    let where_now_ref = where_now_m_gone.clone();
    let base_slot: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let base_ref = base_slot.clone();

    let stub = StubGraph::start(Arc::new(move |method, path, body| {
        let base = base_ref.lock().unwrap().clone();
        let step = phase_ref.load(Ordering::SeqCst);
        if method == "POST" && path == "/v1.0/$batch" {
            let req: Value = serde_json::from_slice(body).unwrap();
            let mut responses = Vec::new();
            for item in req["requests"].as_array().unwrap() {
                let id = item["id"].as_str().unwrap();
                let url = item["url"].as_str().unwrap();
                if url.contains("/messages/delta") {
                    if step >= 1 && url.contains("/f-inbox/") && url.contains("deltatoken=d0") {
                        // Page 1 of Inbox incremental delta:
                        // - m-new arrives at 14:10:00Z (modified 14:10:00Z)
                        // - m-gone appears on page 1, then will be @removed on page 2
                        responses.push(batch_item(
                            id,
                            200,
                            json!({}),
                            json!({
                                "value": [
                                    {
                                        "id": "m-new",
                                        "conversationId": "c-new",
                                        "parentFolderId": "f-inbox",
                                        "receivedDateTime": "2026-11-17T14:10:00Z",
                                        "lastModifiedDateTime": "2026-11-17T14:10:00Z",
                                        "isRead": false,
                                        "isDraft": false,
                                        "categories": [],
                                        "extensions": [],
                                    },
                                    {
                                        "id": "m-gone",
                                        "conversationId": "c-gone",
                                        "parentFolderId": "f-inbox",
                                        "receivedDateTime": "2026-11-17T13:00:00Z",
                                        "lastModifiedDateTime": "2026-11-17T14:10:00Z",
                                        "isRead": true,
                                        "isDraft": false,
                                        "categories": [],
                                        "extensions": [],
                                    },
                                ],
                                "@odata.nextLink": format!("{base}v1.0/me/mailFolders/f-inbox/messages/delta?$skiptoken=p2"),
                            }),
                        ));
                    } else if step >= 1 && url.contains("/f-inbox/") && url.contains("skiptoken=p2") {
                        // Page 2 of Inbox incremental delta:
                        // - m-new updated in the SAME folder at 14:10:05Z (must STILL arrive!)
                        // - m-gone @removed on page 2 (must trigger where_now and move to Archive)
                        responses.push(batch_item(
                            id,
                            200,
                            json!({}),
                            json!({
                                "value": [
                                    {
                                        "id": "m-new",
                                        "conversationId": "c-new",
                                        "parentFolderId": "f-inbox",
                                        "receivedDateTime": "2026-11-17T14:10:00Z",
                                        "lastModifiedDateTime": "2026-11-17T14:10:05Z",
                                        "isRead": false,
                                        "isDraft": false,
                                        "categories": [],
                                        "extensions": [],
                                    },
                                    {
                                        "id": "m-gone",
                                        "@removed": { "reason": "deleted" },
                                    },
                                ],
                                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/f-inbox/messages/delta?$deltatoken=d1"),
                            }),
                        ));
                    } else {
                        let fid = url
                            .strip_prefix("/me/mailFolders/")
                            .and_then(|s| s.split('/').next())
                            .unwrap_or("f-inbox");
                        let tok = if step >= 1 { "d1" } else { "d0" };
                        responses.push(batch_item(
                            id,
                            200,
                            json!({}),
                            json!({
                                "value": [],
                                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/{fid}/messages/delta?$deltatoken={tok}"),
                            }),
                        ));
                    }
                } else if url.starts_with("/me/messages/m-gone") {
                    where_now_ref.fetch_add(1, Ordering::SeqCst);
                    responses.push(batch_item(
                        id,
                        200,
                        json!({}),
                        json!({
                            "id": "m-gone",
                            "conversationId": "c-gone",
                            "parentFolderId": "f-archive",
                            "receivedDateTime": "2026-11-17T13:00:00Z",
                            "lastModifiedDateTime": "2026-11-17T14:10:06Z",
                            "isRead": true,
                            "isDraft": false,
                            "categories": [],
                            "extensions": [],
                        }),
                    ));
                } else if let Some(folder) = url
                    .strip_prefix("/me/mailFolders/")
                    .and_then(|s| s.split('?').next())
                {
                    let mapped = match folder {
                        "inbox" => Some("f-inbox"),
                        "archive" => Some("f-archive"),
                        _ => None,
                    };
                    match mapped {
                        Some(fid) => responses.push(batch_item(id, 200, json!({}), json!({ "id": fid }))),
                        None => responses.push(batch_item(
                            id,
                            404,
                            json!({}),
                            json!({ "error": { "code": "ErrorItemNotFound", "message": "none" } }),
                        )),
                    }
                } else {
                    responses.push(batch_item(id, 200, json!({}), json!({})));
                }
            }
            return ok(json!({ "responses": responses }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/mailFolders/delta") {
            if path.contains("deltatoken=") {
                return ok(json!({
                    "value": [],
                    "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/delta?$deltatoken=t1"),
                }));
            }
            return ok(json!({
                "value": [
                    { "id": "f-inbox", "displayName": "Inbox", "parentFolderId": "root" },
                    { "id": "f-archive", "displayName": "Archive", "parentFolderId": "root" },
                ],
                "@odata.deltaLink": format!("{base}v1.0/me/mailFolders/delta?$deltatoken=t0"),
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me?") {
            return ok(json!({
                "id": "u1",
                "displayName": "Maya",
                "mail": "maya@brightmoor.co",
            }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/messages?") {
            if (path.ends_with("top=1") || path.contains("top=1&"))
                && path.contains("receivedDateTime")
            {
                return ok(json!({
                    "value": [{ "receivedDateTime": "2026-11-17T14:00:00Z" }]
                }));
            }
            if path.contains("%27c-new%27") || path.contains("'c-new'") {
                return ok(json!({
                    "value": [{
                        "id": "m-new",
                        "conversationId": "c-new",
                        "parentFolderId": "f-inbox",
                        "receivedDateTime": "2026-11-17T14:10:00Z",
                        "lastModifiedDateTime": "2026-11-17T14:10:05Z",
                        "isRead": false,
                        "isDraft": false,
                        "subject": "Fresh Arrival",
                        "bodyPreview": "Hello",
                        "from": { "emailAddress": { "name": "Ann", "address": "ann@example.com" } },
                        "categories": [],
                        "extensions": [],
                    }]
                }));
            }
            if path.contains("%27c-gone%27") || path.contains("'c-gone'") {
                return ok(json!({
                    "value": [{
                        "id": "m-gone",
                        "conversationId": "c-gone",
                        "parentFolderId": "f-archive",
                        "receivedDateTime": "2026-11-17T13:00:00Z",
                        "lastModifiedDateTime": "2026-11-17T14:10:06Z",
                        "isRead": true,
                        "isDraft": false,
                        "subject": "Archived Mid-Page",
                        "bodyPreview": "Moved",
                        "from": { "emailAddress": { "name": "Ann", "address": "ann@example.com" } },
                        "categories": [],
                        "extensions": [],
                    }]
                }));
            }
            return ok(json!({ "value": [] }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;
    *base_slot.lock().unwrap() = stub.url.to_string();

    let (_dir, store) = open_outlook(&stub.url).await;
    store.sync().await.expect("initial sync");

    phase.store(1, Ordering::SeqCst);
    let report = store.sync().await.expect("multipage delta sync");

    // (a) `m-new` appeared on page 1 and page 2 of Inbox with newer timestamp -> STILL arrives!
    assert_eq!(
        report.arrived.len(),
        1,
        "same-folder multi-page update must not suppress arrival: {:?}",
        report.arrived
    );
    assert_eq!(report.arrived[0].thread.as_str(), "c-new");

    // (b) `m-gone` appeared on page 1 then @removed on page 2 -> where_now ran and moved it out
    // of Inbox into Archive instead of leaving it stuck in Inbox!
    assert_eq!(
        where_now_m_gone.load(Ordering::SeqCst),
        1,
        "page-2 @removed triggered where_now"
    );
    let inbox_ids: Vec<String> = store
        .list(&ListId::Inbox, None, 20)
        .await
        .unwrap()
        .threads
        .into_iter()
        .map(|t| t.id.as_str().to_owned())
        .collect();
    assert!(inbox_ids.contains(&"c-new".to_owned()));
    assert!(
        !inbox_ids.contains(&"c-gone".to_owned()),
        "c-gone is archived, not stuck in Inbox: {inbox_ids:?}"
    );
    let c_gone = store
        .thread(&ThreadId::new("c-gone"))
        .await
        .unwrap()
        .expect("c-gone stored in archive");
    assert!(!c_gone.messages[0].labels.contains(&"INBOX".to_owned()));
}

#[tokio::test]
async fn outlook_rules_handle_missing_id_unknown_override_and_no_probe_on_permanent_error() {
    let rules_gets = Arc::new(AtomicUsize::new(0));
    let rules_gets_ref = rules_gets.clone();
    let delete_calls = Arc::new(AtomicUsize::new(0));
    let delete_calls_ref = delete_calls.clone();
    let patch_bodies: Arc<std::sync::Mutex<Vec<Value>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let patch_bodies_ref = patch_bodies.clone();
    let fail_post_403 = Arc::new(AtomicBool::new(false));
    let fail_post_ref = fail_post_403.clone();

    let stub = StubGraph::start(Arc::new(move |method, path, body| {
        if method == "POST" && path == "/v1.0/$batch" {
            let req: Value = serde_json::from_slice(body).unwrap();
            let responses: Vec<Value> = req["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    let id = item["id"].as_str().unwrap();
                    let url = item["url"].as_str().unwrap();
                    if url.contains("/deleteditems") {
                        batch_item(id, 200, json!({}), json!({ "id": "f-deleted" }))
                    } else if url.contains("/inbox") {
                        batch_item(id, 200, json!({}), json!({ "id": "f-inbox" }))
                    } else {
                        batch_item(
                            id,
                            404,
                            json!({}),
                            json!({ "error": { "code": "ErrorItemNotFound", "message": "none" } }),
                        )
                    }
                })
                .collect();
            return ok(json!({ "responses": responses }));
        }
        if method == "GET" && path.starts_with("/v1.0/me/mailFolders/delta") {
            return ok(json!({
                "value": [
                    { "id": "f-inbox", "displayName": "Inbox", "parentFolderId": "root" },
                    { "id": "f-deleted", "displayName": "Deleted Items", "parentFolderId": "root" },
                ]
            }));
        }
        if method == "GET" && path == "/v1.0/me/mailFolders/inbox/messageRules" {
            rules_gets_ref.fetch_add(1, Ordering::SeqCst);
            return ok(json!({ "value": [] }));
        }
        if method == "POST" && path == "/v1.0/me/mailFolders/inbox/messageRules" {
            if fail_post_ref.load(Ordering::SeqCst) {
                return (
                    403,
                    Vec::new(),
                    json!({ "error": { "code": "ErrorAccessDenied", "message": "denied" } }),
                );
            }
            // Graph omits `id` from the created rule response.
            return (
                201,
                Vec::new(),
                json!({ "displayName": "Block a@b.example" }),
            );
        }
        if method == "DELETE" {
            delete_calls_ref.fetch_add(1, Ordering::SeqCst);
            return (204, Vec::new(), Value::Null);
        }
        if method == "GET" && path == "/v1.0/me/inferenceClassification/overrides" {
            return ok(json!({
                "value": [{
                    "id": "ov-future",
                    "classifyAs": "someFutureClassification",
                    "senderEmailAddress": { "address": "future@example.com" },
                }]
            }));
        }
        if method == "PATCH" && path == "/v1.0/me/inferenceClassification/overrides/ov-future" {
            let parsed: Value = serde_json::from_slice(body).unwrap();
            patch_bodies_ref.lock().unwrap().push(parsed);
            return ok(json!({
                "id": "ov-future",
                "classifyAs": "focused",
                "senderEmailAddress": { "address": "future@example.com" },
            }));
        }
        ok(json!({ "value": [] }))
    }))
    .await;

    let (_dir, store) = open_outlook(&stub.url).await;

    // 1. Missing `id` on created messageRule returns `SenderRuleUndo::None`, and undo issues NO
    // DELETE against `.../messageRules/`.
    let undo_no_id = store
        .set_sender_rule("a@b.example", SenderRule::Block)
        .await
        .expect("block rule with missing id");
    assert_eq!(undo_no_id, SenderRuleUndo::None);
    store
        .undo_sender_rule(&undo_no_id)
        .await
        .expect("undo None is a no-op");
    assert_eq!(delete_calls.load(Ordering::SeqCst), 0);

    // 2. Existing override with `UnknownFutureValue` updates cleanly and returns `None` undo so
    // undo neither PATCHes `"unknownFutureValue"` nor deletes the pre-existing override.
    let undo_unknown = store
        .set_sender_rule("future@example.com", SenderRule::Important)
        .await
        .expect("updates unknown override");
    assert_eq!(undo_unknown, SenderRuleUndo::None);
    store
        .undo_sender_rule(&undo_unknown)
        .await
        .expect("undo None does not touch override");
    assert_eq!(patch_bodies.lock().unwrap().len(), 1);
    assert_eq!(delete_calls.load(Ordering::SeqCst), 0);

    // 3. Permanent 403 error on `POST messageRules` does NOT trigger a redundant recovery probe.
    rules_gets.store(0, Ordering::SeqCst);
    fail_post_403.store(true, Ordering::SeqCst);
    let err = store
        .set_sender_rule("denied@example.com", SenderRule::Block)
        .await
        .expect_err("403 propagates");
    assert!(matches!(err, Error::Outlook(_)), "{err}");
    assert_eq!(
        rules_gets.load(Ordering::SeqCst),
        1,
        "only the initial pre-check GET runs; permanent 403 does not trigger recovery GET"
    );

    // 4. Invalid whitespace / multi-`@` inputs are rejected cleanly.
    assert!(matches!(
        store.set_sender_rule("a@b c", SenderRule::Important).await,
        Err(Error::Unsupported(_))
    ));
    assert_eq!(
        store
            .set_sender_rule("a@b@c", SenderRule::Block)
            .await
            .expect("invalid block target is no-op"),
        SenderRuleUndo::None
    );
}
