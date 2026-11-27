//! Throttling, the mailbox's concurrency limit and `$batch` against mock-servers' Graph.

use crate::support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mock_servers::{Config, DEV_ACCESS_TOKEN};
use outlook::{
    Client, Endpoints, MAX_BATCH, MessagePatch, Order, TokenFuture, TokenProvider, well_known,
};
use serde_json::json;
use support::Mock;

#[tokio::test]
async fn a_throttled_request_waits_out_retry_after() {
    let mock = Mock::start().await;
    mock.fault(json!({ "method": "GET", "path": "/v1.0/me/mailFolders/inbox", "status": 429, "retryAfter": 1 }))
        .await;
    let started = Instant::now();
    let inbox = mock.client.mail().folder(well_known::INBOX).await.unwrap();
    assert_eq!(inbox.display_name.as_deref(), Some("Inbox"));
    assert!(started.elapsed() >= Duration::from_secs(1), "it waited");

    // A 503 is sent again too.
    mock.fault(json!({ "method": "GET", "path": "/v1.0/me/mailFolders/inbox", "status": 503, "retryAfter": 1 }))
        .await;
    assert!(mock.client.mail().folder(well_known::INBOX).await.is_ok());
}

#[tokio::test]
async fn a_long_retry_after_comes_back_to_the_caller() {
    let mock = Mock::start().await;
    mock.fault(json!({ "method": "GET", "path": "/v1.0/me/mailFolders/inbox", "status": 429, "retryAfter": 600 }))
        .await;
    let started = Instant::now();
    let error = mock
        .client
        .mail()
        .folder(well_known::INBOX)
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1), "it didn't wait");
    let outlook::Error::Api(api) = error else {
        panic!("{error:?}");
    };
    assert_eq!(api.status, 429);
    assert_eq!(api.error.code, "TooManyRequests");
    assert_eq!(api.retry_after, Some(Duration::from_secs(600)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mailbox_gets_four_requests_at_a_time() {
    // Outlook's limit, with each request taking 200 ms. A fifth at once would be refused and
    // sent again a second later.
    let mock = Mock::with(Config {
        graph_concurrency: Some(4),
        latency: Duration::from_millis(200),
        ..Config::default()
    })
    .await;
    let started = Instant::now();
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let client = mock.client.clone();
        requests.spawn(async move { client.people().me().await });
    }
    for result in requests.join_all().await {
        result.unwrap();
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(400),
        "two rounds: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "none refused: {elapsed:?}"
    );
}

#[tokio::test]
async fn a_batch_answers_each_request() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    let results = mock
        .client
        .batch(
            vec![
                mail.folder(well_known::INBOX),
                mail.folder("nonexistent-folder"),
                mail.folder(well_known::DRAFTS).select("displayName"),
            ],
            Order::Parallel,
        )
        .await
        .unwrap();
    assert_eq!(
        results[0].as_ref().unwrap().display_name.as_deref(),
        Some("Inbox")
    );
    let error = results[1].as_ref().unwrap_err();
    assert!(
        error.status().is_some_and(|status| status >= 400),
        "{error}"
    );
    assert_eq!(
        results[2].as_ref().unwrap().display_name.as_deref(),
        Some("Drafts")
    );

    let too_many = (0..=MAX_BATCH)
        .map(|_| mail.folder(well_known::INBOX))
        .collect();
    assert!(matches!(
        mock.client.batch(too_many, Order::Parallel).await,
        Err(outlook::Error::Batch(_))
    ));
}

#[tokio::test]
async fn a_sequential_batch_stops_at_a_failure() {
    let mock = Mock::start().await;
    mock.deliver("Ada Lovelace <ada@example.com>", "First")
        .await;
    mock.deliver("Ada Lovelace <ada@example.com>", "Second")
        .await;
    let first = mock.inbox_message("First").await.id.unwrap();
    let second = mock.inbox_message("Second").await.id.unwrap();
    let read = MessagePatch {
        is_read: Some(true),
        ..MessagePatch::default()
    };
    let mail = mock.client.mail();
    let results = mock
        .client
        .batch(
            vec![
                mail.update_message(&first, &read),
                mail.update_message("AAkALgAAmissing", &read),
                mail.update_message(&second, &read),
            ],
            Order::Sequential,
        )
        .await
        .unwrap();
    assert_eq!(results[0].as_ref().unwrap().is_read, Some(true));
    assert!(results[1].is_err());
    let skipped = results[2].as_ref().unwrap_err();
    assert_eq!(skipped.status(), Some(424), "{skipped}");
    assert_eq!(
        mail.message(&second).await.unwrap().is_read,
        Some(false),
        "it never ran"
    );
}

#[tokio::test]
async fn throttled_requests_inside_a_batch_are_sent_again() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    for order in [Order::Parallel, Order::Sequential] {
        // The batch itself succeeds; one request in it is turned away with its own 429.
        mock.fault(json!({ "method": "GET", "path": "/v1.0/me/mailFolders/drafts", "status": 429, "retryAfter": 1 }))
            .await;
        let started = Instant::now();
        let results = mock
            .client
            .batch(
                vec![
                    mail.folder(well_known::INBOX),
                    mail.folder(well_known::DRAFTS),
                    mail.folder(well_known::SENT_ITEMS),
                ],
                order,
            )
            .await
            .unwrap();
        let names: Vec<_> = results
            .into_iter()
            .map(|result| result.unwrap().display_name.unwrap())
            .collect();
        assert_eq!(names, ["Inbox", "Drafts", "Sent Items"], "{order:?}");
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "{order:?} waited"
        );
    }
}

#[tokio::test]
async fn links_off_graph_keep_the_token() {
    let mock = Mock::start().await;
    let elsewhere = url::Url::parse("https://collector.example/v1.0/me/messages").unwrap();
    let error = mock
        .client
        .follow::<outlook::Message>(&elsewhere)
        .await
        .unwrap_err();
    assert!(matches!(error, outlook::Error::ForeignLink(_)), "{error}");
}

#[tokio::test]
async fn an_inner_401_in_a_batch_invalidates_the_token() {
    struct RecordingTokens {
        token: String,
        invalidated: Arc<Mutex<Vec<String>>>,
    }

    impl TokenProvider for RecordingTokens {
        fn access_token(&self) -> TokenFuture<'_> {
            let token = self.token.clone();
            Box::pin(async move { Ok(token) })
        }

        fn invalidate(&self, token: &str) {
            self.invalidated.lock().unwrap().push(token.to_owned());
        }
    }

    let mock = Mock::start().await;
    let invalidated = Arc::new(Mutex::new(Vec::new()));
    let client = Client::new(
        Endpoints::mock(&mock.server.url()),
        RecordingTokens {
            token: DEV_ACCESS_TOKEN.into(),
            invalidated: invalidated.clone(),
        },
    );
    mock.fault(json!({
        "method": "GET",
        "path": "/v1.0/me/mailFolders/drafts",
        "status": 401
    }))
    .await;

    let mail = client.mail();
    let results = client
        .batch(
            vec![
                mail.folder(well_known::INBOX),
                mail.folder(well_known::DRAFTS),
            ],
            Order::Parallel,
        )
        .await
        .unwrap();
    assert!(results[0].is_ok());
    assert_eq!(results[1].as_ref().unwrap_err().status(), Some(401));
    assert_eq!(
        invalidated.lock().unwrap().as_slice(),
        [DEV_ACCESS_TOKEN],
        "inner 401 invalidates the token used by the batch"
    );
}
