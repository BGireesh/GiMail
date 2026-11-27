//! A client against an in-process mock, and the mock's admin endpoints.

#![allow(dead_code)] // Each test binary uses a different part of this module.

use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::{Client, Endpoints, StaticToken};
use serde_json::{Value, json};

/// The mock's mailbox.
pub const EMAIL: &str = "maya@brightmoor.co";

pub struct Mock {
    pub server: MockServer,
    pub client: Client,
}

impl Mock {
    pub async fn start() -> Self {
        Self::with(Config::default()).await
    }

    pub async fn with(config: Config) -> Self {
        let server = MockServer::start(config).await.unwrap();
        let client = Client::new(
            Endpoints::mock(&server.url()),
            StaticToken(DEV_ACCESS_TOKEN.into()),
        );
        Self { server, client }
    }

    /// POSTs `body` to the mock's admin endpoint `path`.
    pub async fn admin(&self, path: &str, body: Value) -> Value {
        let response = reqwest::Client::new()
            .post(self.server.url().join(path).unwrap())
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response.json().await.unwrap_or(Value::Null)
    }

    /// Delivers a message from `from` with `subject` to the Inbox.
    pub async fn deliver(&self, from: &str, subject: &str) {
        self.admin(
            "/__mock/deliver",
            json!({ "from": from, "subject": subject, "text": "Hello" }),
        )
        .await;
    }

    /// Makes the next `times` requests matching `path` fail with `status`.
    pub async fn fault(&self, fault: Value) {
        self.admin("/__mock/faults", fault).await;
    }

    /// The Inbox message with `subject`.
    pub async fn inbox_message(&self, subject: &str) -> outlook::Message {
        let page = self
            .client
            .mail()
            .folder_messages(outlook::well_known::INBOX)
            .filter(&format!("subject eq {}", outlook::odata::quote(subject)))
            .await
            .unwrap();
        page.value
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no {subject:?} in the Inbox"))
    }
}
