//! What Graph is asked to list for a list: a folder's messages, or the mailbox's with a filter,
//! newest first, grouped into conversations. Lists Graph can't ask for (Important, Other, the
//! splits, Done) list what holds them (the Inbox, every folder) and the store routes their
//! threads, as it does everything it stores.

use std::collections::{HashMap, HashSet};

use outlook::{Call, Client, Message, Page};
use url::Url;

use super::Outlook;
use super::mailbox::{Mailbox, Role, SNOOZED};
use super::parse::{LABELS, expand, listed};
use crate::bulk::{Scope, label_of};
use crate::error::Result;
use crate::ids::labels::{DRAFT, INBOX, SENT, SPAM, TRASH};
use crate::ids::{ListId, ThreadId};
use crate::provider::{Dated, Find, Listed, Listing, Selection};
use crate::quota::Lane;
use crate::sync::ATTEMPTS;

/// What a listing reads of each message.
const LISTED: &str =
    "id,conversationId,parentFolderId,lastModifiedDateTime,receivedDateTime,isDraft";
/// How many messages a contact's search reads per conversation it lists.
const PER_CONTACT_THREAD: u32 = 5;

/// Where a listing's messages come from.
enum Source {
    /// A folder, by id.
    Folder(String),
    /// Every folder, narrowed by a `$filter` when there's one.
    All(Option<String>),
    /// A KQL search across folders.
    Search(String),
    /// Nothing Graph has.
    Nothing,
}

/// Which of the messages listed count: those Exo reads, and for Done those outside the Inbox,
/// Junk Email and Deleted Items.
#[derive(Clone, Copy)]
enum Keep {
    Read,
    Done,
    /// Not drafts, for a contact's conversations.
    NoDrafts,
}

impl Outlook {
    /// A page of the conversations `listing` asks for.
    pub(super) async fn list(&self, listing: Listing<'_>, lane: Lane) -> Result<Listed> {
        let mailbox = self.mailbox().await?;
        let (source, keep) = match listing.selection {
            Selection::List(list) => selection(list, &mailbox),
            Selection::Contact(email) => (
                crate::query::participants_kql(email).map_or(Source::Nothing, Source::Search),
                Keep::NoDrafts,
            ),
        };
        let page = listing
            .page
            .as_deref()
            .and_then(|link| Url::parse(link).ok());
        let before = listing.before;
        let max = listing.max;
        let top = match source {
            Source::Search(_) => max.saturating_mul(PER_CONTACT_THREAD),
            _ => max,
        };
        let messages: Page<Message> = match (&source, &page) {
            (Source::Nothing, _) => Page::default(),
            (_, Some(link)) => {
                self.graph
                    .call(lane, ATTEMPTS, |graph| graph.follow::<Message>(link))
                    .await?
            }
            (source, None) => {
                self.graph
                    .call(lane, ATTEMPTS, |graph| first(graph, source, before, top))
                    .await?
            }
        };
        let mut threads: Vec<(String, u64)> = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for message in &messages.value {
            let folder = message.parent_folder_id.as_deref().unwrap_or_default();
            let received = message
                .received_date_time
                .map_or(0, jiff::Timestamp::as_millisecond);
            let kept = mailbox.is_read(folder)
                && kept(keep, &mailbox, folder)
                && (!matches!(keep, Keep::NoDrafts)
                    || message.is_draft != Some(true)
                        && before.is_none_or(|before| received < before));
            let Some(thread) = message.conversation_id.clone().filter(|_| kept) else {
                continue;
            };
            let version = listed(message.last_modified_date_time);
            match by_id.get(&thread).copied() {
                Some(index) => threads[index].1 = threads[index].1.max(version),
                None => {
                    by_id.insert(thread.clone(), threads.len());
                    threads.push((thread, version));
                }
            }
        }
        let next = match source {
            // A contact's conversations are its first page alone.
            Source::Search(_) => {
                threads.truncate(max as usize);
                None
            }
            _ => messages.next_link.map(String::from),
        };
        Ok(Listed { threads, next })
    }
}

/// Messages a page of a select-all's finding reads.
const FIND_PAGE: u32 = 500;

impl Outlook {
    /// Every conversation of a select-all's list that `find` asks for: those with a message
    /// dated at or before the cutoff (carrying the label, if one is given, and passing the
    /// filter), or those with mail since it. Graph's dates are to the second, as the store's
    /// Outlook dates are.
    pub(super) async fn find(&self, find: Find<'_>) -> Result<Vec<ThreadId>> {
        let cutoff = find.cutoff;
        let list = match &cutoff.scope {
            Scope::List(list) => list,
            Scope::Search(raw) => {
                let query = crate::query::SearchQuery::parse(raw);
                return self
                    .find_search(&query, find.labels, find.router, cutoff.before, find.dated)
                    .await;
            }
        };
        let wanted = cutoff.filter.map(label_of).transpose()?;
        let mailbox = self.mailbox().await?;
        let (source, keep) = selection(list, &mailbox);
        if matches!(source, Source::Nothing) {
            return Ok(Vec::new());
        }
        let at = cutoff.before.to_string();
        let dated = match find.dated {
            Dated::AtOrBefore { .. } => format!("receivedDateTime le {at}"),
            Dated::After => format!("receivedDateTime gt {at}"),
        };
        let markers = expand(false);
        let mut found: Vec<ThreadId> = Vec::new();
        let mut seen: HashSet<ThreadId> = HashSet::new();
        let mut page: Option<Url> = None;
        loop {
            let messages: Page<Message> = match &page {
                Some(link) => {
                    self.graph
                        .call(Lane::Sync, ATTEMPTS, |graph| graph.follow::<Message>(link))
                        .await?
                }
                None => {
                    self.graph
                        .call(Lane::Sync, ATTEMPTS, |graph| {
                            dated_in(graph, &source, &dated)
                                .select(LABELS)
                                .expand(&markers)
                                .top(FIND_PAGE)
                        })
                        .await?
                }
            };
            let mailbox = self.mailbox_for(messages.value.iter()).await?;
            for message in &messages.value {
                let folder = message.parent_folder_id.as_deref().unwrap_or_default();
                if !mailbox.is_read(folder) || !kept(keep, &mailbox, folder) {
                    continue;
                }
                let labels = mailbox.labels(message, folder);
                let has = |label: &str| labels.iter().any(|carried| carried == label);
                let counts = match find.dated {
                    Dated::AtOrBefore { label } => wanted.is_none_or(has) && label.is_none_or(has),
                    // What's new since isn't a draft, nor the user's own reply outside Sent.
                    Dated::After => {
                        !has(DRAFT) && (*list == ListId::Sent || !has(SENT) || has(INBOX))
                    }
                };
                if let Some(thread) = message.conversation_id.as_deref().filter(|_| counts) {
                    let thread = ThreadId::new(thread);
                    if seen.insert(thread.clone()) {
                        found.push(thread);
                    }
                }
            }
            match messages.next_link {
                Some(next) => page = Some(next),
                None => return Ok(found),
            }
        }
    }
}

/// Whether a message in `folder` counts for a list that keeps `keep`.
fn kept(keep: Keep, mailbox: &Mailbox, folder: &str) -> bool {
    match keep {
        Keep::Read | Keep::NoDrafts => true,
        Keep::Done => {
            let labels = mailbox.folder_labels(folder);
            ![INBOX, SPAM, TRASH]
                .iter()
                .any(|role| labels.contains(role))
        }
    }
}

/// `source`'s messages dated as `dated` (a `$filter` on `receivedDateTime`) says.
fn dated_in<'a>(graph: &'a Client, source: &Source, dated: &str) -> Call<'a, Page<Message>> {
    let mail = graph.mail();
    match source {
        Source::Folder(id) => mail.folder_messages(id).filter(dated),
        Source::All(Some(filter)) => mail.messages().filter(&format!("{dated} and {filter}")),
        Source::All(None) | Source::Search(_) | Source::Nothing => mail.messages().filter(dated),
    }
}

/// Where `list` lists from, and which of it counts.
fn selection(list: &ListId, mailbox: &Mailbox) -> (Source, Keep) {
    let folder = |role: Role| {
        mailbox
            .folder(role)
            .map_or(Source::Nothing, |id| Source::Folder(id.to_owned()))
    };
    match list {
        ListId::All => (Source::All(None), Keep::Read),
        ListId::Inbox | ListId::Important | ListId::Other | ListId::Split(_) => {
            (folder(Role::Inbox), Keep::Read)
        }
        ListId::Done => (Source::All(None), Keep::Done),
        ListId::Starred => (
            Source::All(Some("flag/flagStatus eq 'flagged'".into())),
            Keep::Read,
        ),
        ListId::Sent => (folder(Role::Sent), Keep::Read),
        ListId::Drafts => (folder(Role::Drafts), Keep::Read),
        ListId::Spam => (folder(Role::Junk), Keep::Read),
        ListId::Trash => (folder(Role::Deleted), Keep::Read),
        // Outlook has no mute.
        ListId::Muted => (Source::Nothing, Keep::Read),
        // Every reminder is local (`db::coverage`), so this only finds what carries the marker.
        ListId::Reminders => (
            Source::All(Some(outlook::OpenTypeExtension::filter(SNOOZED))),
            Keep::Read,
        ),
        ListId::Scheduled => (Source::Nothing, Keep::Read),
        ListId::Label(key) => match mailbox.folder_for(key) {
            Some(id) if mailbox.is_folder_key(key) => (Source::Folder(id.to_owned()), Keep::Read),
            _ => match mailbox.category_name(key) {
                Some(name) => (
                    Source::All(Some(format!(
                        "categories/any(c:c eq {})",
                        outlook::odata::quote(name)
                    ))),
                    Keep::Read,
                ),
                None => (Source::Nothing, Keep::Read),
            },
        },
    }
}

/// The first page of `source`, `top` messages at most, newest first, received before `before`
/// (epoch milliseconds) when given.
fn first<'a>(
    graph: &'a Client,
    source: &Source,
    before: Option<i64>,
    top: u32,
) -> Call<'a, Page<Message>> {
    let mail = graph.mail();
    // Outlook orders by a property only when the filter starts with it.
    let received = match before.and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok()) {
        Some(before) => format!("receivedDateTime lt {before}"),
        None => "receivedDateTime ge 1900-01-01T00:00:00Z".to_owned(),
    };
    let call = match source {
        Source::Search(query) => return mail.messages().search(query).select(LISTED).top(top),
        Source::Folder(id) => mail.folder_messages(id).filter(&received),
        Source::All(None) | Source::Nothing => mail.messages().filter(&received),
        Source::All(Some(filter)) => mail.messages().filter(&format!("{received} and {filter}")),
    };
    call.order_by("receivedDateTime desc")
        .select(LISTED)
        .top(top)
}
