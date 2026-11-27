//! Outlook behind the seam: Microsoft Graph's mail, through [`Graph`], read as the store's rows.
//! A conversation is a thread, and a message's folder, read state, flag, categories, Focused
//! Inbox verdict and Exo's markers are its label keys ([`mailbox`]).

mod account;
mod contacts;
mod drafts;
mod fetch;
pub(crate) mod graph;
mod history;
mod mailbox;
mod parse;
mod rules;
mod search;
mod select;
mod settings;
mod triage;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use jiff::Timestamp;
use tokio::sync::watch;

use self::graph::Graph;
use self::mailbox::Mailbox;
use self::parse::Versions;
use super::{
    Account, Changes, Cursors, DraftHandle, Fetched, Find, Found, Labelled, Labels, Listed,
    Listing, Parsed, Provider, Searching, Sent,
};
use crate::api::Failure;
use crate::bulk::{SenderRule, SenderRuleUndo};
use crate::connection::{Connection, Debounce, Outage};
use crate::contacts::{PhotoMetadata, ProviderContacts};
use crate::drafts::SendFailure;
use crate::error::{ProviderError, Result};
use crate::ids::ThreadId;
use crate::mutation::Step;
use crate::out_of_office::AutoReply;
use crate::query::{LabelIndex, SearchQuery};
use crate::quota::{Lane, Quota};
use crate::view::Router;

/// An Outlook account.
pub(crate) struct Outlook {
    graph: Graph,
    /// HTTP client for pre-authenticated upload-session PUTs with streamed progress.
    http: reqwest::Client,
    /// The mailbox's folders and categories, once read.
    mailbox: RwLock<Option<Arc<Mailbox>>>,
    versions: Versions,
}

impl Outlook {
    /// Graph's calls through `client`, paced under `quota` (`None` paces nothing), for a store
    /// in `dir` whose outages end as `debounce` says.
    pub fn new(
        client: outlook::Client,
        base: Duration,
        quota: Option<Quota>,
        debounce: Debounce,
        dir: &Path,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5 * 60))
            .build()
            .expect("the TLS backend initializes");
        Self {
            graph: Graph::new(client, base, quota, debounce, dir),
            http,
            mailbox: RwLock::new(None),
            versions: Versions::default(),
        }
    }

    /// The mailbox as last read, reading it first if it hasn't been yet.
    async fn mailbox(&self) -> Result<Arc<Mailbox>, Failure> {
        let known = self
            .mailbox
            .read()
            .expect("the mailbox lock is never poisoned")
            .clone();
        match known {
            Some(mailbox) => Ok(mailbox),
            None => Ok(self.load().await?.0),
        }
    }

    /// The mailbox to read `messages` with: as last read, or with the master category list read
    /// again when they carry a category the list may have gained since.
    async fn mailbox_for<'a>(
        &self,
        messages: impl Iterator<Item = &'a outlook::Message> + Clone,
    ) -> Result<Arc<Mailbox>, Failure> {
        let mailbox = self.mailbox().await?;
        if !mailbox.misses_categories(messages.clone()) {
            return Ok(mailbox);
        }
        let categories = self
            .graph
            .call(Lane::Sync, crate::sync::ATTEMPTS, |graph| {
                graph.settings().categories().all()
            })
            .await?
            .value;
        Ok(self.keep(mailbox.with_categories(&categories, messages)))
    }

    /// Keeps `mailbox` as the mailbox now.
    fn keep(&self, mailbox: Mailbox) -> Arc<Mailbox> {
        let mailbox = Arc::new(mailbox);
        *self
            .mailbox
            .write()
            .expect("the mailbox lock is never poisoned") = Some(mailbox.clone());
        mailbox
    }
}

impl Provider for Outlook {
    fn connection(&self) -> watch::Receiver<Connection> {
        self.graph.connection()
    }

    fn down(&self, outage: Outage) {
        self.graph.down(outage);
    }

    fn reached(&self, mail: bool) {
        self.graph.reached(mail);
    }

    fn check(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(self.check())
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
        Box::pin(self.rows(id, stored, lane, attempts))
    }

    fn thread<'a>(
        &'a self,
        id: &'a str,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Fetched>, Failure>> {
        Box::pin(self.thread(id, lane, attempts))
    }

    fn message<'a>(
        &'a self,
        id: &'a str,
        full: bool,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Parsed>, Failure>> {
        Box::pin(self.message(id, full, lane, attempts))
    }

    fn bodies<'a>(
        &'a self,
        thread: &'a str,
        missing: &'a [String],
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Vec<Parsed>, Failure>> {
        Box::pin(self.bodies(thread, missing, lane, attempts))
    }

    fn part<'a>(
        &'a self,
        message: &'a str,
        attachment: Option<&'a str>,
        part: &'a str,
        lane: Lane,
        _progress: Option<::gmail::Progress>,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(self.part(message, attachment, part, lane))
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
        labels: &'a LabelIndex,
        router: &'a Router,
    ) -> BoxFuture<'a, Result<Vec<ThreadId>>> {
        Box::pin(async move {
            let parsed = SearchQuery::parse(query);
            self.find_threads(&parsed, labels, router).await
        })
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
        Box::pin(self.thread_source(id))
    }

    fn auto_reply(&self) -> BoxFuture<'_, Result<AutoReply>> {
        Box::pin(self.auto_reply())
    }

    fn update_auto_reply<'a>(&'a self, reply: &'a AutoReply) -> BoxFuture<'a, Result<AutoReply>> {
        Box::pin(self.update_auto_reply(reply))
    }

    fn sync_contacts<'a>(
        &'a self,
        _account: &'a str,
        kept: Option<&'a ProviderContacts>,
        listed: Timestamp,
    ) -> BoxFuture<'a, Result<(ProviderContacts, bool)>> {
        Box::pin(self.sync_contacts(kept, listed))
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
        _thread: Option<&'a str>,
        progress: Option<&'a ::gmail::Progress>,
    ) -> BoxFuture<'a, Result<DraftHandle, Failure>> {
        Box::pin(self.create_draft(raw, progress))
    }

    fn update_draft<'a>(
        &'a self,
        id: &'a str,
        raw: &'a [u8],
        _thread: Option<&'a str>,
        progress: Option<&'a ::gmail::Progress>,
    ) -> BoxFuture<'a, Result<DraftHandle, Failure>> {
        Box::pin(self.update_draft(id, raw, progress))
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
        drafts::refusal(error)
    }
}
