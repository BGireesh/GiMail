//! Every thread of a select-all's list or search that Gmail finds (`threads.list`, through
//! every page), for bulk actions.

use std::collections::HashSet;

use gmail::ListQuery;
use jiff::Timestamp;

use super::Gmail;
use super::select::selection;
use crate::bulk::{ATTEMPTS, CONCURRENCY, Scope, label_of};
use crate::error::Result;
use crate::ids::labels::{CHAT, DRAFT, INBOX, SENT, SPAM, TRASH, UNREAD};
use crate::ids::{Filter, ListId, ThreadId};
use crate::provider::{Dated, Find};
use crate::quota::Lane;
use crate::split::{Me, important_definition};

impl Gmail {
    /// Every thread Gmail lists for `query`, through every page.
    pub(crate) async fn find_all(&self, mut query: ListQuery) -> Result<Vec<ThreadId>> {
        let mut found = Vec::new();
        query.max_results = Some(500);
        loop {
            let page = self
                .api
                .call(Lane::Sync, ATTEMPTS, |gmail| gmail.threads(&query))
                .await?;
            found.extend(
                page.threads
                    .into_iter()
                    .filter_map(|thread| thread.id)
                    .map(ThreadId::new),
            );
            match page.next_page_token {
                Some(token) => query.page_token = Some(token),
                None => return Ok(found),
            }
        }
    }

    /// The threads `find` asks for.
    pub(super) async fn find(&self, find: Find<'_>) -> Result<Vec<ThreadId>> {
        match find.dated {
            Dated::AtOrBefore { label } => self.find_before(&find, label).await,
            Dated::After => self.find_after(&find).await,
        }
    }

    /// Every thread of the cutoff's list that Gmail finds at or before the cutoff's date,
    /// carrying `label` if one is given.
    async fn find_before(&self, find: &Find<'_>, label: Option<&str>) -> Result<Vec<ThreadId>> {
        let cutoff = find.cutoff;
        let mut query = scope_query(find)?;
        query.q = Some(before_query(
            query.q.as_deref(),
            cutoff.filter,
            cutoff.before,
        )?);
        if let Some(label) = label {
            query.label_ids.push(label.to_owned());
            query.include_spam_trash |= [SPAM, TRASH].contains(&label);
        }
        self.find_all(query).await
    }

    /// Every thread of the cutoff's list that Gmail finds with mail since the cutoff's date.
    /// Gmail's `after:` has 1-second resolution, so threads whose newest mail falls in the
    /// cutoff's own second are checked by millisecond `internalDate`.
    async fn find_after(&self, find: &Find<'_>) -> Result<Vec<ThreadId>> {
        let cutoff = find.cutoff;
        let base = scope_query(find)?;
        let sec = cutoff.before.as_second();
        let next_sec = Timestamp::from_second(sec.saturating_add(1)).unwrap_or(Timestamp::MAX);
        let mut strictly_after = base.clone();
        strictly_after.q = Some(after_query(base.q.as_deref(), next_sec));
        let mut newer: HashSet<ThreadId> =
            self.find_all(strictly_after).await?.into_iter().collect();

        let before_ms = cutoff.before.as_millisecond();
        let next_ms = next_sec.as_millisecond();
        let required_labels = base.label_ids.clone();
        let include_spam_trash = base.include_spam_trash;
        let mut boundary = base;
        boundary.q = Some(format!(
            "{} before:{}",
            after_query(boundary.q.as_deref(), cutoff.before),
            sec.saturating_add(1)
        ));
        let candidates: Vec<ThreadId> = self
            .find_all(boundary)
            .await?
            .into_iter()
            .filter(|thread| !newer.contains(thread))
            .collect();
        let drafts_count = matches!(cutoff.scope, Scope::List(ListId::Drafts));
        let sent_scope = matches!(cutoff.scope, Scope::List(ListId::Sent));
        for round in candidates.chunks(CONCURRENCY) {
            let fetched = futures::future::join_all(round.iter().map(|thread| {
                let id = thread.as_str().to_owned();
                async move {
                    let result = self
                        .api
                        .call(Lane::Sync, ATTEMPTS, |gmail| {
                            gmail.thread(&id, gmail::Format::Minimal, &[])
                        })
                        .await;
                    (thread.clone(), result)
                }
            }))
            .await;
            for (thread, result) in fetched {
                match result {
                    Ok(full) => {
                        let has_newer = full.messages.iter().any(|message| {
                            let date = message.internal_date.unwrap_or_default();
                            if date <= before_ms || date >= next_ms {
                                return false;
                            }
                            let has = |label: &str| message.label_ids.iter().any(|l| l == label);
                            let draft = !drafts_count && has(DRAFT);
                            let chat = has(CHAT);
                            let spam_or_trash = !include_spam_trash && (has(SPAM) || has(TRASH));
                            let own_reply = !sent_scope && has(SENT) && !has(INBOX);
                            !draft
                                && !chat
                                && !spam_or_trash
                                && !own_reply
                                && required_labels.iter().all(|label| has(label))
                        });
                        if has_newer {
                            newer.insert(thread);
                        }
                    }
                    Err(failure) if failure.is_not_found() => {}
                    Err(failure) => return Err(failure.into()),
                }
            }
        }
        Ok(newer.into_iter().collect())
    }
}

/// How Gmail is asked for the scope's threads: the list's own query (the one backfill uses), or
/// the search's Gmail form.
fn scope_query(find: &Find<'_>) -> Result<ListQuery> {
    let router = find.router;
    Ok(match &find.cutoff.scope {
        Scope::List(list) => selection(list, router)?,
        Scope::Search(raw) => {
            let me = Me {
                addresses: &router.me.addresses,
                name: router.me.name.as_deref(),
            };
            let parsed = crate::query::SearchQuery::parse(raw);
            let q = parsed.to_gmail(
                find.labels,
                &router.me.addresses,
                &important_definition(&me),
            );
            ListQuery {
                q: Some(q),
                include_spam_trash: parsed.includes_spam_trash(),
                ..ListQuery::default()
            }
        }
    })
}

/// `{query} before:{t}` and the filter's operator, with the list's own query in parentheses. An
/// `OR` in it (a split's query is the user's) then groups inside them and can't reach past the
/// cutoff. `before:` takes seconds and excludes the second it names, so using the second after
/// the cutoff's keeps threads dated within the cutoff's second.
fn before_query(query: Option<&str>, filter: Option<Filter>, before: Timestamp) -> Result<String> {
    let filter = match filter.map(label_of).transpose()? {
        None => None,
        Some(UNREAD) => Some("is:unread"),
        Some(_) => Some("is:starred"),
    };
    let seconds = before.as_second() + 1;
    let mut parts: Vec<String> = query
        .filter(|query| !query.trim().is_empty())
        .map(|query| format!("({query})"))
        .into_iter()
        .collect();
    parts.extend(filter.map(str::to_owned));
    parts.push(format!("before:{seconds}"));
    Ok(parts.join(" "))
}

/// `{query} after:{t}`, which finds what the list has dated at or after the cutoff's second,
/// whatever the filter.
fn after_query(query: Option<&str>, before: Timestamp) -> String {
    let seconds = before.as_second();
    match query.filter(|query| !query.trim().is_empty()) {
        Some(query) => format!("({query}) after:{seconds}"),
        None => format!("after:{seconds}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn the_cutoff_and_filter_go_outside_the_lists_query() {
        let at: Timestamp = "2026-11-10T14:00:00.250Z".parse().unwrap();
        let seconds = at.as_second() + 1;
        // A split's own `OR` stays inside its parentheses, so it can't reach past the cutoff.
        assert_eq!(
            before_query(
                Some("-in:DRAFT in:inbox {from:\"a.com\"} OR subject:x"),
                None,
                at
            )
            .unwrap(),
            format!("(-in:DRAFT in:inbox {{from:\"a.com\"}} OR subject:x) before:{seconds}")
        );
        assert_eq!(
            before_query(Some("-in:INBOX"), Some(Filter::Unread), at).unwrap(),
            format!("(-in:INBOX) is:unread before:{seconds}")
        );
        // A label list is asked for by id, with nothing in `q`.
        assert_eq!(
            before_query(None, Some(Filter::Starred), at).unwrap(),
            format!("is:starred before:{seconds}")
        );
        assert!(matches!(
            before_query(Some("in:inbox"), Some(Filter::Important), at),
            Err(Error::Unsearchable(_))
        ));
        assert!(matches!(
            before_query(Some("in:inbox"), Some(Filter::NoReply), at),
            Err(Error::Unsearchable(_))
        ));
        let after_sec = at.as_second();
        assert_eq!(
            after_query(Some("-in:INBOX OR x"), at),
            format!("(-in:INBOX OR x) after:{after_sec}")
        );
        assert_eq!(after_query(None, at), format!("after:{after_sec}"));
    }
}
