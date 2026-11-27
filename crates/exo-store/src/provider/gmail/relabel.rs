//! Label changes: the outbox's steps and bulk's relabels, as `messages.batchModify`,
//! `threads.modify` and `messages.modify`.

use gmail::{BatchModifyMessagesRequest, ModifyMessageRequest, ModifyThreadRequest};

use super::Gmail;
use crate::api::Failure;
use crate::bulk::ATTEMPTS;
use crate::mutation::Step;
use crate::quota::Lane;

impl Gmail {
    /// One outbox step, in one call.
    pub(super) async fn apply(&self, thread: &str, step: &Step) -> Result<(), Failure> {
        match step {
            Step::RemoveFromMessages { ids, labels } => {
                self.batch_modify(Lane::Act, 1, ids, &[], labels).await
            }
            Step::ModifyThread { add, remove } => {
                unless_gone(self.modify_thread(Lane::Act, 1, thread, add, remove).await)
            }
            Step::AddToMessage { id, label } => {
                let request = ModifyMessageRequest {
                    add_label_ids: vec![label.clone()],
                    ..ModifyMessageRequest::default()
                };
                unless_gone(
                    self.api
                        .call(Lane::Act, 1, |gmail| gmail.modify_message(id, &request))
                        .await
                        .map(drop),
                )
            }
            // A refresh reads the thread again ([`crate::provider::Provider::rows`]).
            Step::Refresh => Ok(()),
        }
    }

    /// Bulk's change to messages `ids`, all at once.
    pub(super) async fn relabel_messages(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), Failure> {
        self.batch_modify(Lane::Sync, ATTEMPTS, ids, add, remove)
            .await
    }

    /// Bulk's change to every message of thread `id`.
    pub(super) async fn relabel_thread(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), Failure> {
        self.modify_thread(Lane::Sync, ATTEMPTS, id, add, remove)
            .await
    }

    /// `messages.batchModify`.
    async fn batch_modify(
        &self,
        lane: Lane,
        attempts: u32,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), Failure> {
        let request = BatchModifyMessagesRequest {
            ids: ids.to_vec(),
            add_label_ids: add.to_vec(),
            remove_label_ids: remove.to_vec(),
            ..BatchModifyMessagesRequest::default()
        };
        self.api
            .call(lane, attempts, |gmail| {
                gmail.batch_modify_messages(&request)
            })
            .await
    }

    /// `threads.modify`.
    async fn modify_thread(
        &self,
        lane: Lane,
        attempts: u32,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), Failure> {
        let request = ModifyThreadRequest {
            add_label_ids: add.to_vec(),
            remove_label_ids: remove.to_vec(),
        };
        self.api
            .call(lane, attempts, |gmail| gmail.modify_thread(id, &request))
            .await
            .map(drop)
    }
}

/// A label change on a thread, message or label that no longer exists has nothing left to do.
fn unless_gone(sent: Result<(), Failure>) -> Result<(), Failure> {
    match sent {
        Err(failure)
            if failure.is_not_found() || failure.message().contains("labelId not found") =>
        {
            Ok(())
        }
        sent => sent,
    }
}
