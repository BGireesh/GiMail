//! Gmail's drafts (`users.drafts`), sending them, and the sent message a send made.

use super::Gmail;
use crate::api::Failure;
use crate::drafts::SendFailure;
use crate::ids::labels::SENT;
use crate::provider::{DraftHandle, Labelled, Sent};
use crate::quota::Lane;

/// Attempts at `drafts.send` before Gmail's server errors fail the send (with the call's backoff,
/// about 7 s of trying).
const SEND_ATTEMPTS: u32 = 4;

impl Gmail {
    /// The message draft `id` holds now (`drafts.get`, `format=minimal`). `None` when the draft
    /// is gone.
    pub(super) async fn draft_message(&self, id: &str) -> Result<Option<String>, Failure> {
        match self
            .api
            .call(Lane::Act, 2, |gmail| {
                gmail.draft(id, gmail::Format::Minimal)
            })
            .await
        {
            Ok(draft) => Ok(draft.message.and_then(|held| held.id)),
            Err(failure) if failure.is_not_found() => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    /// Thread `id`'s messages with their labels (`threads.get`, `format=minimal`). `None` when
    /// the thread is gone.
    pub(super) async fn thread_labels(&self, id: &str) -> Result<Option<Labelled>, Failure> {
        let fetched = match self
            .api
            .call(Lane::Act, 2, |gmail| {
                gmail.thread(id, gmail::Format::Minimal, &[])
            })
            .await
        {
            Err(failure) if failure.is_not_found() => return Ok(None),
            fetched => fetched?,
        };
        Ok(Some(
            fetched
                .messages
                .into_iter()
                .filter_map(|message| Some((message.id?, message.label_ids)))
                .collect(),
        ))
    }

    /// The draft that holds message `message`, looking through every page of `drafts.list`.
    pub(super) async fn find_draft(&self, message: &str) -> Result<Option<String>, Failure> {
        let mut page_token = None;
        loop {
            let query = gmail::DraftQuery {
                max_results: Some(500),
                page_token: page_token.take(),
                ..gmail::DraftQuery::default()
            };
            let page = self
                .api
                .call(Lane::Act, 3, |gmail| gmail.drafts(&query))
                .await?;
            let found = page.drafts.into_iter().find(|draft| {
                draft.message.as_ref().and_then(|m| m.id.as_deref()) == Some(message)
            });
            if let Some(draft) = found {
                return Ok(draft.id);
            }
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(None),
            }
        }
    }

    /// The drafts whose message has Message-ID `rfc822_id` (`rfc822msgid:`).
    pub(super) async fn drafts_with(&self, rfc822_id: &str) -> Result<Vec<String>, Failure> {
        let query = gmail::DraftQuery {
            q: Some(format!("rfc822msgid:{rfc822_id}")),
            ..gmail::DraftQuery::default()
        };
        Ok(self
            .api
            .call(Lane::Act, 2, |gmail| gmail.drafts(&query))
            .await?
            .drafts
            .into_iter()
            .filter_map(|draft| draft.id)
            .collect())
    }

    /// A new draft of `raw` (`drafts.create`), in `thread` when there's one.
    pub(super) async fn create_draft(
        &self,
        raw: &[u8],
        thread: Option<&str>,
        progress: Option<&gmail::Progress>,
    ) -> Result<DraftHandle, Failure> {
        let draft = self
            .api
            .call(Lane::Act, 1, |gmail| {
                reporting(gmail.create_draft(raw.to_vec(), thread), progress)
            })
            .await?;
        Ok(handle(draft))
    }

    /// Draft `id` holding `raw` now (`drafts.update`), in `thread` when there's one.
    pub(super) async fn update_draft(
        &self,
        id: &str,
        raw: &[u8],
        thread: Option<&str>,
        progress: Option<&gmail::Progress>,
    ) -> Result<DraftHandle, Failure> {
        let draft = self
            .api
            .call(Lane::Act, 1, |gmail| {
                reporting(gmail.update_draft(id, raw.to_vec(), thread), progress)
            })
            .await?;
        Ok(handle(draft))
    }

    /// Sends draft `id` (`drafts.send`).
    pub(super) async fn send_draft(&self, id: &str) -> Result<Sent, Failure> {
        let message = self
            .api
            .call(Lane::Act, SEND_ATTEMPTS, |gmail| gmail.send_draft(id))
            .await?;
        Ok(Sent {
            message: message.id,
            thread: message.thread_id,
        })
    }

    /// The sent message with Message-ID `rfc822_id`, if there is one.
    pub(super) async fn find_sent(&self, rfc822_id: &str) -> Result<Option<Sent>, Failure> {
        let query = gmail::ListQuery {
            q: Some(format!("rfc822msgid:{rfc822_id}")),
            label_ids: vec![SENT.to_owned()],
            ..gmail::ListQuery::default()
        };
        let page = self
            .api
            .call(Lane::Act, 2, |gmail| gmail.messages(&query))
            .await?;
        Ok(page.messages.into_iter().next().map(|message| Sent {
            message: message.id,
            thread: message.thread_id,
        }))
    }

    /// Deletes draft `id` (`drafts.delete`).
    pub(super) async fn delete_draft(&self, id: &str) -> Result<(), Failure> {
        self.api
            .call(Lane::Act, 1, |gmail| gmail.delete_draft(id))
            .await
    }
}

/// What a draft Gmail wrote is: its id, its message's and its thread's.
fn handle(draft: gmail::Draft) -> DraftHandle {
    let message = draft.message.unwrap_or_default();
    DraftHandle {
        id: draft.id,
        message: message.id,
        thread: message.thread_id,
    }
}

/// `call`, reporting its upload to `progress` when there's one.
fn reporting<'a>(
    call: gmail::Call<'a, gmail::Draft>,
    progress: Option<&gmail::Progress>,
) -> gmail::Call<'a, gmail::Draft> {
    match progress {
        Some(progress) => call.on_progress(progress.clone()),
        None => call,
    }
}

/// Why Gmail refused to send a draft, from its error.
pub(super) fn refusal(error: &gmail::Error) -> SendFailure {
    // Google's upload front end may answer an oversized message with a page, not its JSON.
    if error.status() == Some(413) {
        return SendFailure::TooLarge;
    }
    let gmail::Error::Api(api) = error else {
        return SendFailure::Other;
    };
    let reason = |wanted: &str| api.errors.iter().any(|e| e.reason == wanted);
    if reason("dailyLimitExceeded") || reason("quotaExceeded") {
        SendFailure::QuotaExceeded
    } else if api.message.to_lowercase().contains("too large") {
        SendFailure::TooLarge
    } else if api.message.contains("Invalid To header")
        || api.message.contains("Invalid Cc header")
        || api.message.contains("Invalid Bcc header")
        || api.message.contains("Recipient address required")
    {
        SendFailure::InvalidRecipients
    } else {
        SendFailure::Other
    }
}
