//! Gmail behind the seam: its calls, through [`Api`], and its messages parsed into the store's
//! rows.

mod account;
mod contacts;
mod drafts;
pub(crate) mod fetch;
mod find;
mod history;
pub(crate) mod parse;
mod relabel;
mod rules;
pub(crate) mod select;

use std::collections::HashSet;

use futures::future::BoxFuture;
use jiff::Timestamp;
use tokio::sync::watch;

use super::{
    Account, Changes, Cursors, DraftHandle, Fetched, Find, Found, Labelled, Labels, Listed,
    Listing, Parsed, Provider, Searching, Sent,
};
use crate::api::{Api, Failure};
use crate::bulk::{SenderRule, SenderRuleUndo};
use crate::connection::{Connection, Outage};
use crate::contacts::{PhotoMetadata, ProviderContacts};
use crate::drafts::SendFailure;
use crate::error::{Error, ProviderError, Result};
use crate::ids::ThreadId;
use crate::mutation::Step;
use crate::out_of_office::AutoReply;
use crate::query::LabelIndex;
use crate::quota::Lane;
use crate::view::Router;

/// Gmail's one cursor scope, the mailbox, whose token is its `historyId`.
const MAILBOX: &str = "";

/// The `historyId` that `cursors` have read the mailbox's history up to.
pub(crate) fn history_id(cursors: &Cursors) -> Option<u64> {
    cursors.get(MAILBOX)?.parse().ok()
}

/// The cursors at `historyId` `id`.
pub(crate) fn cursors_at(id: u64) -> Cursors {
    [(MAILBOX.to_owned(), id.to_string())].into_iter().collect()
}

/// A Gmail account.
pub(crate) struct Gmail {
    /// Google's calls for the account, which the features only Gmail has (the calendar, People,
    /// sender filters, vacation, a thread's raw source) make too.
    pub api: Api,
}

impl Provider for Gmail {
    fn connection(&self) -> watch::Receiver<Connection> {
        self.api.connection()
    }

    fn down(&self, outage: Outage) {
        self.api.down(outage);
    }

    fn reached(&self, mail: bool) {
        self.api.reached(mail);
    }

    fn check(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(self.api.ping())
    }

    fn start(&self) -> BoxFuture<'_, Result<Cursors>> {
        Box::pin(self.start())
    }

    fn labels(&self) -> BoxFuture<'_, Result<Labels>> {
        Box::pin(self.labels())
    }

    fn account(&self) -> BoxFuture<'_, Result<Account>> {
        Box::pin(self.account())
    }

    fn changes<'a>(&'a self, since: &'a Cursors) -> BoxFuture<'a, Result<Option<Changes>>> {
        Box::pin(self.changes(since))
    }

    fn list<'a>(&'a self, listing: Listing<'a>, lane: Lane) -> BoxFuture<'a, Result<Listed>> {
        Box::pin(self.list(listing, lane))
    }

    fn rows<'a>(
        &'a self,
        id: &'a str,
        stored: &'a HashSet<String>,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Fetched>, Failure>> {
        Box::pin(fetch::rows(&self.api, lane, attempts, id, stored))
    }

    fn thread<'a>(
        &'a self,
        id: &'a str,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Fetched>, Failure>> {
        Box::pin(fetch::thread(&self.api, lane, attempts, id))
    }

    fn message<'a>(
        &'a self,
        id: &'a str,
        full: bool,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Parsed>, Failure>> {
        if full {
            Box::pin(fetch::message(&self.api, lane, attempts, id))
        } else {
            Box::pin(fetch::row(&self.api, lane, attempts, id))
        }
    }

    fn bodies<'a>(
        &'a self,
        thread: &'a str,
        missing: &'a [String],
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Vec<Parsed>, Failure>> {
        Box::pin(fetch::bodies(&self.api, lane, attempts, thread, missing))
    }

    fn part<'a>(
        &'a self,
        message: &'a str,
        attachment: Option<&'a str>,
        part: &'a str,
        lane: Lane,
        progress: Option<::gmail::Progress>,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(fetch::part(
            &self.api, lane, message, attachment, part, progress,
        ))
    }

    fn search<'a>(&'a self, searching: Searching<'a>, lane: Lane) -> BoxFuture<'a, Result<Found>> {
        Box::pin(self.search(searching, lane))
    }

    fn find<'a>(&'a self, find: Find<'a>) -> BoxFuture<'a, Result<Vec<ThreadId>>> {
        Box::pin(self.find(find))
    }

    fn find_threads<'a>(
        &'a self,
        query: &'a str,
        _labels: &'a LabelIndex,
        _router: &'a Router,
    ) -> BoxFuture<'a, Result<Vec<ThreadId>>> {
        Box::pin(self.find_all(::gmail::ListQuery {
            q: Some(query.to_owned()),
            ..::gmail::ListQuery::default()
        }))
    }

    fn set_sender_rule<'a>(
        &'a self,
        from: &'a str,
        rule: SenderRule,
    ) -> BoxFuture<'a, Result<SenderRuleUndo>> {
        Box::pin(self.set_sender_rule(from, rule))
    }

    fn undo_sender_rule<'a>(&'a self, undo: &'a SenderRuleUndo) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.undo_sender_rule(undo))
    }

    fn delete_block_rules<'a>(&'a self, from: &'a str) -> BoxFuture<'a, Result<usize>> {
        Box::pin(self.delete_block_rules(from))
    }

    fn thread_source<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<serde_json::Value>> {
        Box::pin(fetch::thread_source(&self.api, id))
    }

    fn auto_reply(&self) -> BoxFuture<'_, Result<AutoReply>> {
        Box::pin(async move {
            let settings = self
                .api
                .call(Lane::Act, 3, |gmail| gmail.vacation())
                .await?;
            Ok(AutoReply::Gmail(settings))
        })
    }

    fn update_auto_reply<'a>(&'a self, reply: &'a AutoReply) -> BoxFuture<'a, Result<AutoReply>> {
        Box::pin(async move {
            let AutoReply::Gmail(settings) = reply else {
                return Err(Error::Unsupported("Outlook automatic replies"));
            };
            let saved = self
                .api
                .call(Lane::Act, 1, |gmail| gmail.update_vacation(settings))
                .await?;
            Ok(AutoReply::Gmail(saved))
        })
    }

    fn sync_contacts<'a>(
        &'a self,
        account: &'a str,
        kept: Option<&'a ProviderContacts>,
        listed: Timestamp,
    ) -> BoxFuture<'a, Result<(ProviderContacts, bool)>> {
        Box::pin(self.sync_contacts(account, kept, listed))
    }

    fn my_photo_metadata(&self) -> BoxFuture<'_, Result<Option<PhotoMetadata>>> {
        Box::pin(self.my_photo_metadata())
    }

    fn contact_photo<'a>(&'a self, locator: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>> {
        Box::pin(self.contact_photo(locator))
    }

    fn apply<'a>(&'a self, thread: &'a str, step: &'a Step) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.apply(thread, step))
    }

    fn relabel_messages<'a>(
        &'a self,
        ids: &'a [String],
        add: &'a [String],
        remove: &'a [String],
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.relabel_messages(ids, add, remove))
    }

    fn relabel_thread<'a>(
        &'a self,
        id: &'a str,
        add: &'a [String],
        remove: &'a [String],
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.relabel_thread(id, add, remove))
    }

    fn thread_labels<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Labelled>, Failure>> {
        Box::pin(self.thread_labels(id))
    }

    fn draft_message<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<String>, Failure>> {
        Box::pin(self.draft_message(id))
    }

    fn find_draft<'a>(
        &'a self,
        message: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, Failure>> {
        Box::pin(self.find_draft(message))
    }

    fn drafts_with<'a>(
        &'a self,
        rfc822_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<String>, Failure>> {
        Box::pin(self.drafts_with(rfc822_id))
    }

    fn create_draft<'a>(
        &'a self,
        raw: &'a [u8],
        thread: Option<&'a str>,
        progress: Option<&'a ::gmail::Progress>,
    ) -> BoxFuture<'a, Result<DraftHandle, Failure>> {
        Box::pin(self.create_draft(raw, thread, progress))
    }

    fn update_draft<'a>(
        &'a self,
        id: &'a str,
        raw: &'a [u8],
        thread: Option<&'a str>,
        progress: Option<&'a ::gmail::Progress>,
    ) -> BoxFuture<'a, Result<DraftHandle, Failure>> {
        Box::pin(self.update_draft(id, raw, thread, progress))
    }

    fn send_draft<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Sent, Failure>> {
        Box::pin(self.send_draft(id))
    }

    fn find_sent<'a>(&'a self, rfc822_id: &'a str) -> BoxFuture<'a, Result<Option<Sent>, Failure>> {
        Box::pin(self.find_sent(rfc822_id))
    }

    fn delete_draft<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.delete_draft(id))
    }

    fn refusal(&self, error: &ProviderError) -> SendFailure {
        match error {
            ProviderError::Gmail(error) => drafts::refusal(error),
            ProviderError::Outlook(_) => SendFailure::Other,
        }
    }
}
