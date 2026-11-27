//! The seam between the store and the account's mail provider. The store reads and writes mail
//! in its own terms, and the provider turns those into its own calls. Gmail's are in [`gmail`].
//!
//! The terms are the store's rows ([`Parsed`], [`Fetched`]), threads and messages by the
//! provider's ids, and label keys: the roles in [`crate::ids::labels`] (`INBOX`, `UNREAD`, …),
//! spelled as Gmail spells its system labels, and a key the provider gives each of the account's
//! own labels.

pub(crate) mod gmail;
pub(crate) mod outlook;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use futures::future::BoxFuture;
use jiff::Timestamp;
use toasty::Executor;
use tokio::sync::watch;

use crate::api::Failure;
use crate::bulk::{ListCutoff, SenderRule, SenderRuleUndo};
use crate::connection::{Connection, Outage};
use crate::contacts::{PhotoMetadata, ProviderContacts};
use crate::db::{AttachmentRow, BodyRow, CursorRow, MessageRow};
use crate::drafts::SendFailure;
use crate::error::{ProviderError, Result};
use crate::ids::{ListId, ThreadId};
use crate::model::{Alias, Label};
use crate::mutation::Step;
use crate::out_of_office::AutoReply;
use crate::query::{LabelIndex, SearchQuery};
use crate::quota::Lane;
use crate::view::Router;

/// An account's mail provider. Every mail call the store makes goes through one.
pub(crate) trait Provider: Send + Sync {
    /// The connection to the provider as calls have found it, as the offline bar shows it.
    fn connection(&self) -> watch::Receiver<Connection>;

    /// Something other than a call to the provider found `outage`.
    fn down(&self, outage: Outage);

    /// Another API on the same account answered (`mail == false`), clearing network/auth outages.
    fn reached(&self, mail: bool);

    /// Makes one small call, to see whether the provider answers.
    fn check(&self) -> BoxFuture<'_, Result<(), Failure>>;

    /// The cursors at the mailbox as it is now, which sync reads changes from.
    fn start(&self) -> BoxFuture<'_, Result<Cursors>>;

    /// The account's labels, with Exo's markers made first where the provider keeps them as
    /// labels.
    fn labels(&self) -> BoxFuture<'_, Result<Labels>>;

    /// Who the account is.
    fn account(&self) -> BoxFuture<'_, Result<Account>>;

    /// What changed since `since`, or `None` when the provider can no longer say, and the cache
    /// is rebuilt.
    fn changes<'a>(&'a self, since: &'a Cursors) -> BoxFuture<'a, Result<Option<Changes>>>;

    /// A page of the threads `listing` asks for, newest first, in `lane`.
    fn list<'a>(&'a self, listing: Listing<'a>, lane: Lane) -> BoxFuture<'a, Result<Listed>>;

    /// Thread `id` for its rows, in `lane`, trying up to `attempts` times: every message's row,
    /// and in full those that may hold files or an invite, unless they're among `stored`. `None`
    /// when the thread is gone.
    fn rows<'a>(
        &'a self,
        id: &'a str,
        stored: &'a HashSet<String>,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Fetched>, Failure>>;

    /// Thread `id` with every message in full. `None` when the thread is gone.
    fn thread<'a>(
        &'a self,
        id: &'a str,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Fetched>, Failure>>;

    /// Message `id` in full, or its row alone unless `full`. `None` when it's gone.
    fn message<'a>(
        &'a self,
        id: &'a str,
        full: bool,
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Option<Parsed>, Failure>>;

    /// The `missing` messages of `thread` in full, fetched the cheapest way. Nothing when the
    /// thread is gone.
    fn bodies<'a>(
        &'a self,
        thread: &'a str,
        missing: &'a [String],
        lane: Lane,
        attempts: u32,
    ) -> BoxFuture<'a, Result<Vec<Parsed>, Failure>>;

    /// The bytes of message `message`'s part `part`, which was stored with the provider's
    /// `attachment` id, reporting to `progress` how much has arrived.
    fn part<'a>(
        &'a self,
        message: &'a str,
        attachment: Option<&'a str>,
        part: &'a str,
        lane: Lane,
        progress: Option<::gmail::Progress>,
    ) -> BoxFuture<'a, Result<Vec<u8>>>;

    /// A page of the messages the provider's search finds for `searching`, newest first.
    fn search<'a>(&'a self, searching: Searching<'a>, lane: Lane) -> BoxFuture<'a, Result<Found>>;

    /// Every thread of a select-all's list or search that the provider finds as `find` asks.
    fn find<'a>(&'a self, find: Find<'a>) -> BoxFuture<'a, Result<Vec<ThreadId>>>;

    /// Every thread the provider finds for `query`, newest first.
    fn find_threads<'a>(
        &'a self,
        query: &'a str,
        labels: &'a LabelIndex,
        router: &'a Router,
    ) -> BoxFuture<'a, Result<Vec<ThreadId>>>;

    /// Creates or updates the provider's sender rule for `from` (block, Important, or Other).
    fn set_sender_rule<'a>(
        &'a self,
        from: &'a str,
        rule: SenderRule,
    ) -> BoxFuture<'a, Result<SenderRuleUndo>>;

    /// Reverses a previous [`set_sender_rule`](Self::set_sender_rule).
    fn undo_sender_rule<'a>(&'a self, undo: &'a SenderRuleUndo) -> BoxFuture<'a, Result<()>>;

    /// Deletes all provider rules blocking `from`. Returns how many were deleted.
    fn delete_block_rules<'a>(&'a self, from: &'a str) -> BoxFuture<'a, Result<usize>>;

    /// Thread `id`'s raw provider JSON for the original-message source viewer.
    fn thread_source<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<serde_json::Value>>;

    /// Reads the account's automatic-reply / out-of-office configuration.
    fn auto_reply(&self) -> BoxFuture<'_, Result<AutoReply>>;

    /// Updates the account's automatic-reply / out-of-office configuration and returns what the
    /// provider kept.
    fn update_auto_reply<'a>(&'a self, reply: &'a AutoReply) -> BoxFuture<'a, Result<AutoReply>>;

    /// Syncs saved contacts, organization directory, and other/relevant contacts from the
    /// provider, returning `(contacts, complete)`.
    fn sync_contacts<'a>(
        &'a self,
        account: &'a str,
        kept: Option<&'a ProviderContacts>,
        listed: Timestamp,
    ) -> BoxFuture<'a, Result<(ProviderContacts, bool)>>;

    /// The signed-in user's profile photo metadata, when available.
    fn my_photo_metadata(&self) -> BoxFuture<'_, Result<Option<PhotoMetadata>>>;

    /// Fetches contact photo bytes for a provider photo locator, or `Ok(None)` when unavailable.
    fn contact_photo<'a>(&'a self, locator: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>>;

    /// Makes the outbox's `step` on thread `thread`, in one try. A change to a thread, message
    /// or label that no longer exists has nothing left to do, so it succeeds. A refresh is read
    /// with [`rows`](Self::rows) instead.
    fn apply<'a>(&'a self, thread: &'a str, step: &'a Step) -> BoxFuture<'a, Result<(), Failure>>;

    /// Adds `add` to and removes `remove` from messages `ids`, at once (a bulk relabel).
    fn relabel_messages<'a>(
        &'a self,
        ids: &'a [String],
        add: &'a [String],
        remove: &'a [String],
    ) -> BoxFuture<'a, Result<(), Failure>>;

    /// Adds `add` to and removes `remove` from every message of thread `id` (a bulk relabel).
    fn relabel_thread<'a>(
        &'a self,
        id: &'a str,
        add: &'a [String],
        remove: &'a [String],
    ) -> BoxFuture<'a, Result<(), Failure>>;

    /// Thread `id`'s messages as the provider has them now, each by id with its label keys.
    /// `None` when the thread is gone.
    fn thread_labels<'a>(&'a self, id: &'a str)
    -> BoxFuture<'a, Result<Option<Labelled>, Failure>>;

    /// The message draft `id` holds now. `None` when the draft is gone.
    fn draft_message<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<String>, Failure>>;

    /// The draft that holds message `message`, if any.
    fn find_draft<'a>(&'a self, message: &'a str)
    -> BoxFuture<'a, Result<Option<String>, Failure>>;

    /// The drafts whose message has Message-ID `rfc822_id`.
    fn drafts_with<'a>(&'a self, rfc822_id: &'a str)
    -> BoxFuture<'a, Result<Vec<String>, Failure>>;

    /// A new draft holding the RFC 822 message `raw`, in `thread` when there's one, reporting
    /// its upload to `progress`.
    fn create_draft<'a>(
        &'a self,
        raw: &'a [u8],
        thread: Option<&'a str>,
        progress: Option<&'a ::gmail::Progress>,
    ) -> BoxFuture<'a, Result<DraftHandle, Failure>>;

    /// Draft `id` holding `raw` now, as [`create_draft`](Self::create_draft) has it.
    fn update_draft<'a>(
        &'a self,
        id: &'a str,
        raw: &'a [u8],
        thread: Option<&'a str>,
        progress: Option<&'a ::gmail::Progress>,
    ) -> BoxFuture<'a, Result<DraftHandle, Failure>>;

    /// Sends draft `id`.
    fn send_draft<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Sent, Failure>>;

    /// The sent message with Message-ID `rfc822_id`, if there is one.
    fn find_sent<'a>(&'a self, rfc822_id: &'a str) -> BoxFuture<'a, Result<Option<Sent>, Failure>>;

    /// Deletes draft `id`.
    fn delete_draft<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), Failure>>;

    /// Why the provider refused to send a draft, from its answer.
    fn refusal(&self, error: &ProviderError) -> SendFailure;
}

/// A message flattened into its rows.
pub(crate) struct Parsed {
    pub message: MessageRow,
    pub attachments: Vec<AttachmentRow>,
    /// `None` for a message fetched for its row alone.
    pub body: Option<BodyRow>,
}

/// A thread as the provider has it now: its messages, each with its body when it was fetched in
/// full and as its row alone otherwise.
pub(crate) struct Fetched {
    pub id: String,
    /// The thread's version, which only moves forward. A copy older than the stored one is never
    /// written.
    pub version: u64,
    pub messages: Vec<Parsed>,
}

/// Where sync has read the provider's changes up to: a token for each of the provider's scopes,
/// which only the provider reads. Gmail has one, the mailbox. A round's cursors are stored
/// together, once everything it read is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Cursors(BTreeMap<String, String>);

impl Cursors {
    /// The cursors stored.
    pub async fn load(exec: &mut dyn Executor) -> Result<Self> {
        Ok(CursorRow::all()
            .exec(exec)
            .await?
            .into_iter()
            .map(|row| (row.scope, row.token))
            .collect())
    }

    /// Stores these cursors in place of those stored.
    pub async fn save(&self, exec: &mut dyn Executor) -> Result<()> {
        CursorRow::all().delete().exec(exec).await?;
        if self.0.is_empty() {
            return Ok(());
        }
        let mut rows = CursorRow::create_many();
        for (scope, token) in &self.0 {
            rows = rows.with_item(|row| row.scope(scope).token(token));
        }
        rows.exec(exec).await?;
        Ok(())
    }

    /// No scope has a cursor yet.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `scope`'s token.
    pub fn get(&self, scope: &str) -> Option<&str> {
        self.0.get(scope).map(String::as_str)
    }
}

impl FromIterator<(String, String)> for Cursors {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(cursors: I) -> Self {
        Self(cursors.into_iter().collect())
    }
}

/// What changed since the cursors sync stored.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    /// Each thread's changes.
    pub threads: BTreeMap<String, ThreadChanges>,
    /// Every label key the changes name, so that labels made elsewhere are read first.
    pub labels: BTreeSet<String>,
    /// The account's labels themselves changed (a folder made, renamed, moved or deleted), so
    /// they're read again whatever the changes name.
    pub relabelled: bool,
    /// Messages gone from the mailbox, by id alone, when the provider doesn't say whose thread
    /// they were in (Graph's deltas don't). The store finds their threads.
    pub removed: BTreeSet<String>,
    /// The cursors to store once the changes are.
    pub next: Cursors,
}

/// One thread's changes.
#[derive(Debug, Default)]
pub(crate) struct ThreadChanges {
    pub added: BTreeSet<String>,
    pub deleted: BTreeSet<String>,
    /// Each message's labels after its newest change, with that change's version.
    pub labels: BTreeMap<String, (u64, Vec<String>)>,
    /// The newest version touching the thread.
    pub latest: u64,
    /// The changes to its messages, counted for [`SyncProgress`](crate::SyncProgress).
    pub changes: usize,
}

/// The account's labels, and which of them are Exo's markers.
pub(crate) struct Labels {
    pub labels: Vec<Label>,
    pub markers: Markers,
}

/// The keys of the labels that mark Exo's own states, once the provider has them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Markers {
    /// Muted.
    pub muted: Option<String>,
    /// A reminder is set.
    pub snoozed: Option<String>,
    /// A reminder brought the thread back and it hasn't been read since.
    pub reminder_unread: Option<String>,
}

/// Who the account is.
pub(crate) struct Account {
    /// The addresses that are "me", lower-cased.
    pub addresses: Vec<String>,
    /// The account's name.
    pub name: Option<String>,
    /// The addresses compose can send from.
    pub aliases: Vec<Alias>,
}

/// What [`Provider::list`] lists.
pub(crate) enum Selection<'a> {
    /// A list's threads.
    List(&'a ListId),
    /// The conversations with this address, drafts left out.
    Contact(&'a str),
}

/// A page of a [`Selection`].
pub(crate) struct Listing<'a> {
    pub selection: Selection<'a>,
    /// Who the user is and their splits, which the inbox lists are listed by.
    pub router: &'a Router,
    /// Where the previous page left off.
    pub page: Option<String>,
    /// Only threads with mail before this time (epoch milliseconds).
    pub before: Option<i64>,
    /// The most threads on the page.
    pub max: u32,
}

/// A page of threads, by id and version, and where the next one starts.
pub(crate) struct Listed {
    pub threads: Vec<(String, u64)>,
    pub next: Option<String>,
}

/// A page of a search for [`Provider::search`].
pub(crate) struct Searching<'a> {
    pub query: &'a SearchQuery,
    /// The labels the query can name.
    pub labels: &'a LabelIndex,
    /// Who the user is, whom `me` and the Important filter name.
    pub router: &'a Router,
    /// Where the previous page left off.
    pub page: Option<&'a str>,
    /// The most messages on the page.
    pub size: u32,
}

/// The messages a page of a search found, by thread and message id, and where the next page
/// starts.
pub(crate) struct Found {
    pub hits: Vec<(String, String)>,
    pub next: Option<String>,
}

/// What [`Provider::find`] finds: the threads of a select-all's list or search.
pub(crate) struct Find<'a> {
    pub cutoff: &'a ListCutoff,
    /// Which of them, by date.
    pub dated: Dated<'a>,
    /// Who the user is and their splits, which the inbox lists are listed by.
    pub router: &'a Router,
    /// The labels a search can name.
    pub labels: &'a LabelIndex,
}

/// Which of a select-all's threads [`Provider::find`] finds.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Dated<'a> {
    /// Those dated at or before the cutoff, carrying `label` if one is given.
    AtOrBefore { label: Option<&'a str> },
    /// Those with mail since the cutoff.
    After,
}

/// A thread's messages, each by id with its label keys.
pub(crate) type Labelled = Vec<(String, Vec<String>)>;

/// A draft the provider wrote: its id, and its message's and thread's.
pub(crate) struct DraftHandle {
    pub id: Option<String>,
    pub message: Option<String>,
    pub thread: Option<String>,
}

/// The message a send made, and its thread.
pub(crate) struct Sent {
    pub message: Option<String>,
    pub thread: Option<String>,
}
