//! OData's envelope as Graph uses it: collections a page at a time, with `@odata.nextLink` to the
//! next page and, at the end of a delta round, `@odata.deltaLink` to the next round.

use serde::{Deserialize, Serialize};
use url::Url;

/// One page of a collection or of a delta round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    /// The page's items. In a delta, an item that left the collection carries only its `id` and
    /// `@removed`.
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    /// The next page. Follow it with [`Client::follow`](crate::Client::follow) until it's gone.
    #[serde(
        rename = "@odata.nextLink",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_link: Option<Url>,
    /// On a delta round's last page: where the next round starts. Keep it, and follow it with
    /// [`Client::follow`](crate::Client::follow) to get what changed since.
    #[serde(
        rename = "@odata.deltaLink",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub delta_link: Option<Url>,
    /// The collection's size, when asked for with `$count=true`.
    #[serde(
        rename = "@odata.count",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub count: Option<u64>,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            value: Vec::new(),
            next_link: None,
            delta_link: None,
            count: None,
        }
    }
}

/// `@removed` on a delta item: it left the collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    /// Why.
    pub reason: RemovedReason,
}

/// [`Removed::reason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemovedReason {
    /// Deleted for good, or (for messages and folders) moved out of the collection.
    Deleted,
    /// Removed but restorable (directory objects).
    Changed,
    /// A reason this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// `text` as an OData string literal for `$filter`: single-quoted, with quotes doubled.
#[must_use]
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_links_and_removals() {
        #[derive(Debug, Deserialize)]
        struct Item {
            id: String,
            #[serde(rename = "@removed")]
            removed: Option<Removed>,
        }
        let page: Page<Item> = serde_json::from_str(
            r#"{"@odata.context":"x","@odata.deltaLink":"https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta?$deltatoken=t","value":[{"id":"a"},{"id":"b","@removed":{"reason":"deleted"}},{"id":"c","@removed":{"reason":"somethingNew"}}]}"#,
        )
        .unwrap();
        assert_eq!(page.next_link, None);
        assert!(
            page.delta_link
                .unwrap()
                .query()
                .unwrap()
                .contains("$deltatoken=t")
        );
        assert_eq!(page.value[0].id, "a");
        assert_eq!(page.value[0].removed, None);
        assert_eq!(
            page.value[1].removed.as_ref().map(|removed| removed.reason),
            Some(RemovedReason::Deleted)
        );
        assert_eq!(
            page.value[2].removed.as_ref().map(|removed| removed.reason),
            Some(RemovedReason::UnknownFutureValue)
        );
    }

    #[test]
    fn quotes_literals() {
        assert_eq!(quote("AAQk=="), "'AAQk=='");
        assert_eq!(quote("it's"), "'it''s'");
    }
}
