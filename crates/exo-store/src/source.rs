//! A thread's raw source as the original-message viewer shows it: Gmail's `threads.get` response
//! (with HTML entities in snippets decoded) or Microsoft Graph's conversation messages, written
//! the way `JSON.stringify(thread, null, 4)` writes it.

use serde::Serialize as _;
use serde_json::{Serializer, Value, ser::PrettyFormatter};

use crate::{entities::unescape_html, error::Result, store::Store};

impl Store {
    /// Thread `id`'s raw JSON source for the original-message viewer: Gmail's `threads.get`
    /// (`format=full`, falling back to per-message fetches when too large) or Outlook's Graph
    /// conversation messages (`id`, `messages`), pretty-printed four spaces deep.
    ///
    /// # Errors
    /// [`crate::Error::Offline`] without a network; [`crate::Error::UnknownThread`] if the
    /// conversation doesn't exist; [`crate::Error::Gmail`] or [`crate::Error::Outlook`] when the
    /// provider refuses.
    pub async fn thread_source(&self, id: &str) -> Result<String> {
        let thread = self.inner.mail.thread_source(id).await?;
        Ok(stringify(&thread))
    }
}

/// Decodes the HTML entities in each Gmail message's `snippet`.
pub(crate) fn normalize_snippets(thread: &mut Value) {
    let Some(messages) = thread.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages {
        if let Some(Value::String(snippet)) = message.get_mut("snippet") {
            *snippet = unescape_html(snippet);
        }
    }
}

/// `JSON.stringify(value, null, 4)` with member order preserved.
fn stringify(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut serializer =
        Serializer::with_formatter(&mut buf, PrettyFormatter::with_indent(b"    "));
    value
        .serialize(&mut serializer)
        .expect("serializing JSON into memory doesn't fail");
    String::from_utf8(buf).expect("serde_json writes UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(json: &str) -> String {
        let mut thread: Value = serde_json::from_str(json).expect("valid JSON");
        normalize_snippets(&mut thread);
        stringify(&thread)
    }

    #[test]
    fn writes_what_gmail_sent_in_its_order_four_spaces_deep() {
        let json = r#"{"id":"t1","historyId":"42","messages":[{"id":"m1","labelIds":["INBOX","UNREAD"],"sizeEstimate":1234,"payload":{"headers":[],"parts":[{"partId":"0","body":{"size":0}}]},"draft":false,"x":null}]}"#;
        let expected = r#"{
    "id": "t1",
    "historyId": "42",
    "messages": [
        {
            "id": "m1",
            "labelIds": [
                "INBOX",
                "UNREAD"
            ],
            "sizeEstimate": 1234,
            "payload": {
                "headers": [],
                "parts": [
                    {
                        "partId": "0",
                        "body": {
                            "size": 0
                        }
                    }
                ]
            },
            "draft": false,
            "x": null
        }
    ]
}"#;
        assert_eq!(source(json), expected);
    }

    #[test]
    fn snippets_are_decoded_and_strings_quoted_as_json_stringify_does() {
        let json = r#"{"messages":[{"snippet":"today&#39;s <b> &amp; more","payload":{"headers":[{"name":"To","value":"\"Maya\" <m@x.co>,\tPriya\u0001 é"}]}}]}"#;
        let shown = source(json);
        assert!(
            shown.contains(r#""snippet": "today's <b> & more""#),
            "{shown}"
        );
        assert!(
            shown.contains(r#""value": "\"Maya\" <m@x.co>,\tPriya\u0001 é""#),
            "{shown}"
        );
    }
}
