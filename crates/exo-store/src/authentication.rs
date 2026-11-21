//! The headers of stored messages that say how their senders authenticated (Gmail's
//! `Authentication-Results`), which a brand's logo needs.

use crate::db::{MessageRow, ParticipantEntry};
use crate::error::Result;
use crate::model::Header;
use crate::store::Store;

/// How many of a sender's latest conversations are looked through for their newest message.
const CONVERSATIONS: usize = 5;

impl Store {
    /// Message `id`'s headers, when it's stored.
    ///
    /// # Errors
    /// On database failures.
    pub async fn headers(&self, id: &str) -> Result<Option<Vec<Header>>> {
        let mut db = self.inner.db.clone();
        let row = MessageRow::filter_by_id(id).first().exec(&mut db).await?;
        Ok(row.map(|row| row.headers.0))
    }

    /// The headers of the newest stored message `email` sent, among their latest conversations.
    ///
    /// # Errors
    /// On database failures.
    pub async fn latest_headers_from(&self, email: &str) -> Result<Option<Vec<Header>>> {
        let mut db = self.inner.db.clone();
        let email = email.to_lowercase();
        let fields = ParticipantEntry::fields();
        let entries = ParticipantEntry::filter(fields.email().eq(&email))
            .order_by(fields.sort().desc())
            .limit(CONVERSATIONS)
            .exec(&mut db)
            .await?;
        let mut newest: Option<MessageRow> = None;
        for entry in entries {
            let messages = MessageRow::filter_by_thread_id(&entry.thread_id)
                .exec(&mut db)
                .await?;
            for message in messages {
                let sent = message
                    .sender
                    .0
                    .as_ref()
                    .is_some_and(|sender| sender.email.eq_ignore_ascii_case(&email));
                if sent
                    && newest
                        .as_ref()
                        .is_none_or(|newest| message.internal_date > newest.internal_date)
                {
                    newest = Some(message);
                }
            }
        }
        Ok(newest.map(|message| message.headers.0))
    }
}
