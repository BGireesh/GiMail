//! Searching Outlook: Exo's query as KQL for Graph's `$search`, newest sent first, with each hit
//! kept only if its labels don't rule it out, since KQL can't ask for labels, read state, stars
//! or folders.

use std::collections::{BTreeSet, HashSet};

use outlook::{Message, Page};
use url::Url;

use super::Outlook;
use super::mailbox::Mailbox;
use super::parse::{ROW, expand, parse};
use crate::error::Result;
use crate::ids::ThreadId;
use crate::ids::labels::{SPAM, TRASH};
use crate::provider::{Dated, Found, Searching};
use crate::query::{LabelIndex, SearchQuery};
use crate::quota::Lane;
use crate::sync::ATTEMPTS;
use crate::view::{Router, ThreadView};

/// Messages a page of a select-all's search reads.
const FIND_PAGE: u32 = 250;

impl Outlook {
    /// A page of the messages `searching` finds, newest first.
    pub(super) async fn search(&self, searching: Searching<'_>, lane: Lane) -> Result<Found> {
        let link = searching.page.and_then(|link| Url::parse(link).ok());
        let page = self
            .searched(
                searching.query,
                searching.router,
                link,
                searching.size,
                lane,
            )
            .await?;
        let mailbox = self.mailbox_for(page.value.iter()).await?;
        let hits = page
            .value
            .iter()
            .filter_map(|message| {
                let thread = self.hit(
                    message,
                    &mailbox,
                    searching.query,
                    searching.labels,
                    searching.router,
                )?;
                Some((thread, message.id.clone()?))
            })
            .collect();
        Ok(Found {
            hits,
            next: page.next_link.map(String::from),
        })
    }

    /// Every conversation `query` finds across every page, newest first.
    pub(super) async fn find_threads(
        &self,
        query: &SearchQuery,
        labels: &LabelIndex,
        router: &Router,
    ) -> Result<Vec<ThreadId>> {
        let mut found: Vec<ThreadId> = Vec::new();
        let mut seen: HashSet<ThreadId> = HashSet::new();
        let mut link = None;
        loop {
            let page = self
                .searched(query, router, link.take(), FIND_PAGE, Lane::Sync)
                .await?;
            let mailbox = self.mailbox_for(page.value.iter()).await?;
            for message in &page.value {
                if let Some(thread) = self.hit(message, &mailbox, query, labels, router) {
                    let thread = ThreadId::new(thread);
                    if seen.insert(thread.clone()) {
                        found.push(thread);
                    }
                }
            }
            match page.next_link {
                Some(next) => link = Some(next),
                None => return Ok(found),
            }
        }
    }

    /// Every conversation a select-all's search `query` finds `dated` so against the cutoff
    /// `before`.
    pub(super) async fn find_search(
        &self,
        query: &SearchQuery,
        labels: &LabelIndex,
        router: &Router,
        before: jiff::Timestamp,
        dated: Dated<'_>,
    ) -> Result<Vec<ThreadId>> {
        let before = before.as_millisecond();
        let mut found: Vec<ThreadId> = Vec::new();
        let mut seen: HashSet<ThreadId> = HashSet::new();
        let mut link = None;
        loop {
            let page = self
                .searched(query, router, link.take(), FIND_PAGE, Lane::Sync)
                .await?;
            let mailbox = self.mailbox_for(page.value.iter()).await?;
            for message in &page.value {
                let received = message
                    .received_date_time
                    .map_or(0, jiff::Timestamp::as_millisecond);
                let in_time = match dated {
                    Dated::AtOrBefore { .. } => received <= before,
                    Dated::After => received > before,
                };
                let carries = |label: Option<&str>| {
                    label.is_none_or(|label| {
                        mailbox
                            .labels(message, "")
                            .iter()
                            .any(|carried| carried == label)
                    })
                };
                let labelled = match dated {
                    Dated::AtOrBefore { label } => carries(label),
                    Dated::After => true,
                };
                if !in_time || !labelled {
                    continue;
                }
                if let Some(thread) = self.hit(message, &mailbox, query, labels, router) {
                    let thread = ThreadId::new(thread);
                    if seen.insert(thread.clone()) {
                        found.push(thread);
                    }
                }
            }
            match page.next_link {
                Some(next) => link = Some(next),
                None => return Ok(found),
            }
        }
    }

    /// A page of what Graph finds for `query` (from `link`, the last page's next), `top` at
    /// most. A query KQL can ask nothing of lists every message, newest first.
    async fn searched(
        &self,
        query: &SearchQuery,
        router: &Router,
        link: Option<Url>,
        top: u32,
        lane: Lane,
    ) -> Result<Page<Message>> {
        let kql = query.to_kql(&router.me.addresses);
        let markers = expand(false);
        Ok(self
            .graph
            .call(lane, ATTEMPTS, |graph| match (&link, &kql) {
                (Some(link), _) => graph.follow::<Message>(link),
                (None, Some(kql)) => graph
                    .mail()
                    .messages()
                    .search(kql)
                    .select(ROW)
                    .expand(&markers)
                    .top(top),
                (None, None) => graph
                    .mail()
                    .messages()
                    .filter("receivedDateTime ge 1900-01-01T00:00:00Z")
                    .order_by("receivedDateTime desc")
                    .select(ROW)
                    .expand(&markers)
                    .top(top),
            })
            .await?)
    }

    /// `message`'s conversation, if it's a hit: not a draft, in a folder Exo reads, out of Junk
    /// Email and Deleted Items unless the query asks for them, and carrying the labels the query
    /// names, its lists among them.
    fn hit(
        &self,
        message: &Message,
        mailbox: &Mailbox,
        query: &SearchQuery,
        labels: &LabelIndex,
        router: &Router,
    ) -> Option<String> {
        let folder = message.parent_folder_id.as_deref()?;
        if message.is_draft == Some(true) || !mailbox.is_read(folder) {
            return None;
        }
        let parsed = parse(message, false, mailbox, folder, 0)?;
        let row = parsed.message;
        let carried_spam = row
            .label_ids
            .iter()
            .any(|label| label == SPAM || label == TRASH);
        if carried_spam && !query.includes_spam_trash() {
            return None;
        }
        let thread = row.thread_id.clone();
        let keys = row.label_ids.clone();
        let view = ThreadView::new(thread.clone(), vec![row], &[], &BTreeSet::new());
        let lists = router.list_ids(&view, &[]);
        let carried: Vec<&str> = keys.iter().map(String::as_str).chain(lists).collect();
        query.may_match(&carried, labels).then_some(thread)
    }
}
