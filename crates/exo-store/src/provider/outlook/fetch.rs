//! Reading conversations and messages from Graph, parsed into the store's rows.

use std::collections::HashSet;

use outlook::Message;

use super::Outlook;
use super::graph::failed;
use super::mailbox::Mailbox;
use super::parse::{FULL, ROW, expand, needs_structure, parse};
use crate::api::Failure;
use crate::error::{Error, Result};
use crate::ids::ThreadId;
use crate::provider::{Fetched, Parsed};
use crate::quota::Lane;

/// Messages a page of a conversation carries.
const PAGE_SIZE: u32 = 100;

impl Outlook {
    /// Conversation `id` for its rows: every message's row, and in full those that hold files or
    /// are invites, unless they're among `stored`. `None` when the conversation is gone.
    pub(super) async fn rows(
        &self,
        id: &str,
        stored: &HashSet<String>,
        lane: Lane,
        attempts: u32,
    ) -> Result<Option<Fetched>, Failure> {
        let mailbox = self.mailbox().await?;
        let mut messages = self
            .conversation(id, false, &mailbox, lane, attempts)
            .await?;
        if messages.is_empty() {
            return Ok(None);
        }
        let wanted: Vec<String> = messages
            .iter()
            .filter(|message| needs_structure(message))
            .filter_map(|message| message.id.clone())
            .filter(|id| !stored.contains(id))
            .collect();
        let mut full = HashSet::new();
        // A message gone since the conversation was read keeps its row until a delta says so.
        for message in self
            .messages(&wanted, true, lane, attempts)
            .await?
            .into_iter()
            .flatten()
        {
            if let Some(slot) = messages.iter_mut().find(|slot| slot.id == message.id) {
                full.extend(message.id.clone());
                *slot = message;
            }
        }
        let mailbox = self.mailbox_for(messages.iter()).await?;
        Ok(Some(
            self.fetched(id, &messages, &mailbox, |id| full.contains(id)),
        ))
    }

    /// Conversation `id` with every message in full. `None` when it's gone.
    pub(super) async fn thread(
        &self,
        id: &str,
        lane: Lane,
        attempts: u32,
    ) -> Result<Option<Fetched>, Failure> {
        let mailbox = self.mailbox().await?;
        let messages = self
            .conversation(id, true, &mailbox, lane, attempts)
            .await?;
        let mailbox = self.mailbox_for(messages.iter()).await?;
        Ok((!messages.is_empty()).then(|| self.fetched(id, &messages, &mailbox, |_| true)))
    }

    /// Message `id` in full, or its row alone unless `full`. `None` when it's gone, or in a
    /// folder Exo doesn't read.
    pub(super) async fn message(
        &self,
        id: &str,
        full: bool,
        lane: Lane,
        attempts: u32,
    ) -> Result<Option<Parsed>, Failure> {
        let expand = expand(full);
        let found = self
            .graph
            .call(lane, attempts, |graph| {
                graph
                    .mail()
                    .message(id)
                    .select(if full { FULL } else { ROW })
                    .expand(&expand)
            })
            .await;
        match found {
            Ok(message) => {
                let mailbox = self.mailbox_for(std::iter::once(&message)).await?;
                Ok(self.parsed(&message, full, &mailbox))
            }
            Err(failure) if failure.is_not_found() => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    /// The `missing` messages of `thread` in full. Those gone are left out.
    pub(super) async fn bodies(
        &self,
        _thread: &str,
        missing: &[String],
        lane: Lane,
        attempts: u32,
    ) -> Result<Vec<Parsed>, Failure> {
        let messages = self.messages(missing, true, lane, attempts).await?;
        let mailbox = self.mailbox_for(messages.iter().flatten()).await?;
        Ok(messages
            .iter()
            .flatten()
            .filter_map(|message| self.parsed(message, true, &mailbox))
            .collect())
    }

    /// Conversation `id`'s raw JSON source from Graph (`FULL` properties, attachment metadata,
    /// and Exo's open extensions).
    pub(super) async fn thread_source(&self, id: &str) -> Result<serde_json::Value> {
        const SOURCE_ATTEMPTS: u32 = 3;
        let mailbox = self.mailbox().await?;
        let messages = self
            .conversation(id, true, &mailbox, Lane::Act, SOURCE_ATTEMPTS)
            .await?;
        if messages.is_empty() {
            return Err(Error::UnknownThread(ThreadId::new(id)));
        }
        Ok(serde_json::json!({
            "id": id,
            "messages": messages,
        }))
    }

    /// The bytes of attachment `attachment` (stored as part `part`) of message `message`.
    pub(super) async fn part(
        &self,
        message: &str,
        attachment: Option<&str>,
        part: &str,
        lane: Lane,
    ) -> Result<Vec<u8>> {
        let id = attachment.unwrap_or(part);
        match self
            .graph
            .call(lane, 3, |graph| {
                graph.mail().attachment_content(message, id)
            })
            .await
        {
            Ok(media) => Ok(media.data),
            Err(failure) if failure.is_not_found() => Err(Error::MissingPart {
                message: message.to_owned(),
                part: part.to_owned(),
            }),
            Err(failure) => Err(failure.into()),
        }
    }

    /// Conversation `id`'s messages in the folders Exo reads, oldest first, for their rows or in
    /// full.
    async fn conversation(
        &self,
        id: &str,
        full: bool,
        mailbox: &Mailbox,
        lane: Lane,
        attempts: u32,
    ) -> Result<Vec<Message>, Failure> {
        let expand = expand(full);
        let mut messages = self
            .graph
            .call(lane, attempts, |graph| {
                graph
                    .mail()
                    .conversation(id)
                    .select(if full { FULL } else { ROW })
                    .expand(&expand)
                    .top(PAGE_SIZE)
                    .all()
            })
            .await?
            .value;
        messages.retain(|message| {
            message
                .parent_folder_id
                .as_deref()
                .is_none_or(|folder| mailbox.is_read(folder))
        });
        Ok(messages)
    }

    /// Messages `ids`, for their rows or in full, each `None` when it's gone.
    pub(super) async fn messages(
        &self,
        ids: &[String],
        full: bool,
        lane: Lane,
        attempts: u32,
    ) -> Result<Vec<Option<Message>>, Failure> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let expand = expand(full);
        self.graph
            .each(lane, attempts, ids.len(), |graph, at| {
                graph
                    .mail()
                    .message(&ids[at])
                    .select(if full { FULL } else { ROW })
                    .expand(&expand)
            })
            .await?
            .into_iter()
            .map(|found| match found.map_err(failed) {
                Ok(message) => Ok(Some(message)),
                Err(failure) if failure.is_not_found() => Ok(None),
                Err(failure) => Err(failure),
            })
            .collect()
    }

    /// `message` parsed now, unless it's in a folder Exo doesn't read.
    fn parsed(&self, message: &Message, full: bool, mailbox: &Mailbox) -> Option<Parsed> {
        let folder = message.parent_folder_id.as_deref().unwrap_or_default();
        if !mailbox.is_read(folder) {
            return None;
        }
        let version = self.versions.read(message.last_modified_date_time);
        parse(message, full, mailbox, folder, version)
    }

    /// Conversation `id` as `messages`, those `full` picks in full.
    fn fetched(
        &self,
        id: &str,
        messages: &[Message],
        mailbox: &Mailbox,
        full: impl Fn(&str) -> bool,
    ) -> Fetched {
        let messages: Vec<Parsed> = messages
            .iter()
            .filter_map(|message| {
                let whole = message.id.as_deref().is_some_and(&full);
                self.parsed(message, whole, mailbox)
            })
            .collect();
        Fetched {
            id: id.to_owned(),
            version: messages
                .iter()
                .map(|parsed| parsed.message.version)
                .max()
                .unwrap_or_default(),
            messages,
        }
    }
}
