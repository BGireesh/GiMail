//! What Gmail is asked to list (`threads.list`) and search for (`messages.list`), in its search
//! syntax.

use gmail::ListQuery;

use super::Gmail;
use crate::error::{Error, Result};
use crate::ids::ListId;
use crate::provider::{Found, Listed, Listing, Searching, Selection};
use crate::quota::Lane;
use crate::split::{Me, gmail_search, important_definition};
use crate::sync::ATTEMPTS;
use crate::view::Router;

impl Gmail {
    /// A page of `threads.list` for `listing`.
    pub(super) async fn list(&self, listing: Listing<'_>, lane: Lane) -> Result<Listed> {
        let mut query = match listing.selection {
            Selection::List(list) => selection(list, listing.router)?,
            Selection::Contact(email) => ListQuery {
                q: Some(format!(
                    "(from:{email} OR to:{email} OR cc:{email} OR bcc:{email}) AND -in:DRAFT"
                )),
                ..ListQuery::default()
            },
        };
        query.page_token = listing.page;
        query.max_results = Some(listing.max);
        if let Some(before) = listing.before {
            query.q = Some(listed_before(query.q.as_deref(), before));
        }
        let page = self
            .api
            .call(lane, ATTEMPTS, |gmail| gmail.threads(&query))
            .await?;
        Ok(Listed {
            threads: page
                .threads
                .iter()
                .filter_map(|thread| {
                    Some((thread.id.clone()?, thread.history_id.unwrap_or_default()))
                })
                .collect(),
            next: page.next_page_token,
        })
    }

    /// A page of `messages.list` for the query's Gmail form.
    pub(super) async fn search(&self, searching: Searching<'_>, lane: Lane) -> Result<Found> {
        let me = &searching.router.me;
        let important = important_definition(&Me {
            addresses: &me.addresses,
            name: me.name.as_deref(),
        });
        let query = ListQuery {
            q: Some(
                searching
                    .query
                    .to_gmail(searching.labels, &me.addresses, &important),
            ),
            include_spam_trash: searching.query.includes_spam_trash(),
            max_results: Some(searching.size),
            page_token: searching.page.map(str::to_owned),
            ..ListQuery::default()
        };
        let page = self
            .api
            .call(lane, ATTEMPTS, |gmail| gmail.messages(&query))
            .await?;
        Ok(Found {
            hits: page
                .messages
                .iter()
                .filter_map(|message| Some((message.thread_id.clone()?, message.id.clone()?)))
                .collect(),
            next: page.next_page_token,
        })
    }
}

/// `q` narrowed to threads with mail before `before` (epoch milliseconds). Gmail's `before:`
/// takes seconds, so the time is rounded up to leave out nothing before it.
fn listed_before(q: Option<&str>, before: i64) -> String {
    let seconds = before.saturating_add(999).div_euclid(1000);
    match q {
        Some(q) => format!("({q}) before:{seconds}"),
        None => format!("before:{seconds}"),
    }
}

/// What Gmail is asked to list for `list`.
pub(crate) fn selection(list: &ListId, router: &Router) -> Result<ListQuery> {
    let me = &router.me;
    let query = |q: String| ListQuery {
        q: Some(q),
        ..ListQuery::default()
    };
    let labels = |id: &str| ListQuery {
        label_ids: vec![id.to_owned()],
        ..ListQuery::default()
    };
    Ok(match list {
        ListId::All => ListQuery {
            include_spam_trash: true,
            ..ListQuery::default()
        },
        ListId::Inbox | ListId::Important | ListId::Other | ListId::Split(_) => {
            let me = Me {
                addresses: &me.addresses,
                name: me.name.as_deref(),
            };
            query(gmail_search(list.key(), &router.splits.defined, &me))
        }
        ListId::Done => query("-in:INBOX".into()),
        ListId::Muted => labels(
            me.muted_label
                .as_deref()
                .ok_or(Error::NotSynced("the mute label"))?,
        ),
        // Every reminder is local (`db::coverage`), so this only fetches what the label has.
        ListId::Reminders => labels(
            me.snoozed_label
                .as_deref()
                .ok_or(Error::NotSynced("the reminder labels"))?,
        ),
        other => labels(other.key()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_past_evicted_mail_lists_what_came_before_it() {
        assert_eq!(
            listed_before(None, 1_700_000_000_001),
            "before:1700000001",
            "up to the second after"
        );
        assert_eq!(
            listed_before(Some("in:inbox OR label:vip"), 1_700_000_000_000),
            "(in:inbox OR label:vip) before:1700000000"
        );
    }
}
