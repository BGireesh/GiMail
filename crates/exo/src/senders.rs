//! What triage does to everything from a sender or domain. A provider rule carries the choice to
//! future mail (a Gmail filter, or an Outlook Inbox rule / Focused Inbox override), and the mail
//! already there is relabelled. Both are undone together.

use std::collections::HashSet;
use std::sync::LazyLock;

use exo_store::{SenderRule, SenderRuleUndo, Store, ThreadId};
use regex::Regex;

/// An address made only of characters that mean nothing to Gmail's search or filter criteria.
/// The address comes from someone else's From header, and goes into a filter and a `from:"…"`
/// search. A quote, space, brace or leading `-` in it could widen either to mail that isn't
/// theirs (`a@b.com" OR in:inbox`), so only addresses like this can be blocked or moved.
static PLAIN_ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[a-z0-9_][a-z0-9._%+-]*@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$").expect("valid regex")
});

/// Whether `email` can be blocked or moved (see [`PLAIN_ADDRESS`]). Its domain then can be too.
pub(crate) fn is_plain_address(email: &str) -> bool {
    PLAIN_ADDRESS.is_match(email)
}

/// Labels a move to Important takes off.
const NOT_IMPORTANT: [&str; 4] = [
    "CATEGORY_PROMOTIONS",
    "CATEGORY_UPDATES",
    "CATEGORY_SOCIAL",
    "CATEGORY_FORUMS",
];

/// A job on everything from `from`, an address or a domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SenderJob {
    /// Block them. Their mail goes to Trash from now on, and so does the mail already there,
    /// except `thread`, which moves on its own.
    Block {
        /// The address or domain.
        from: String,
        /// The conversation it was blocked from.
        thread: String,
    },
    /// Their mail goes to Other (or Important) from now on, and so do their inbox conversations
    /// now, except `thread`, which moves on its own.
    Importance {
        /// The address or domain.
        from: String,
        /// Other rather than Important.
        other: bool,
        /// The conversation it was asked from.
        thread: String,
    },
}

/// What a job changed, to undo it.
#[derive(Debug)]
pub struct Done {
    /// The provider rule created or updated, to reverse on undo.
    rule: SenderRuleUndo,
    /// Relabels to undo: threads, the labels added, the labels removed.
    relabels: Vec<(Vec<ThreadId>, Vec<String>, Vec<String>)>,
}

/// Runs `job`.
///
/// # Errors
/// When the provider can't be reached or refuses the rule. What was done by then stays done.
pub async fn run(store: &Store, job: &SenderJob) -> exo_store::Result<Done> {
    let strings = |labels: &[&str]| {
        labels
            .iter()
            .map(|label| (*label).to_owned())
            .collect::<Vec<_>>()
    };
    match job {
        SenderJob::Block { from, thread } => {
            let rule = store.set_sender_rule(from, SenderRule::Block).await?;
            let query = format!("from:\"{from}\"");
            let all: Vec<ThreadId> = store
                .find_threads(&query)
                .await?
                .into_iter()
                .filter(|found| found.as_str() != thread)
                .collect();
            let mut relabels = Vec::new();
            if !all.is_empty() {
                let in_inbox: HashSet<ThreadId> = store
                    .find_threads(&format!("{query} in:inbox"))
                    .await?
                    .into_iter()
                    .filter(|found| found.as_str() != thread)
                    .collect();
                let (add, remove) = (strings(&["TRASH"]), strings(&["INBOX", "SPAM"]));
                let trashed = store.relabel(&all, &add, &remove).await?;
                // Undo must remove Trash and restore Inbox in one relabel for threads that came
                // from the inbox; on single-folder providers (Outlook), adding Inbox while Trash is
                // still set is a no-op and removing Trash afterward would leave them in Archive.
                let (trashed_inbox, trashed_other): (Vec<_>, Vec<_>) =
                    trashed.into_iter().partition(|id| in_inbox.contains(id));
                if !trashed_other.is_empty() {
                    relabels.push((trashed_other, add.clone(), Vec::new()));
                }
                if !trashed_inbox.is_empty() {
                    relabels.push((trashed_inbox, add, strings(&["INBOX"])));
                }
            }
            Ok(Done { rule, relabels })
        }
        SenderJob::Importance {
            from,
            other,
            thread,
        } => {
            let (sender_rule, category) = if *other {
                (SenderRule::Other, "CATEGORY_SOCIAL")
            } else {
                (SenderRule::Important, "CATEGORY_PERSONAL")
            };
            let rule = store.set_sender_rule(from, sender_rule).await?;
            let threads: Vec<ThreadId> = store
                .find_threads(&format!("in:inbox from:\"{from}\""))
                .await?
                .into_iter()
                .filter(|found| found.as_str() != thread)
                .collect();
            let mut relabels = Vec::new();
            if !threads.is_empty() {
                let (add, remove) = if *other {
                    (strings(&[category]), strings(&["CATEGORY_PERSONAL"]))
                } else {
                    (strings(&[category]), strings(&NOT_IMPORTANT))
                };
                let moved = store.relabel(&threads, &add, &remove).await?;
                if !moved.is_empty() {
                    relabels.push((moved, add, remove));
                }
            }
            Ok(Done { rule, relabels })
        }
    }
}

/// Undoes what `done` changed: the rule is reversed, and the relabelled conversations get their
/// labels back, in reverse.
///
/// # Errors
/// When the provider can't be reached.
pub async fn undo(store: &Store, done: Done) -> exo_store::Result<()> {
    store.undo_sender_rule(&done.rule).await?;
    for (threads, added, removed) in done.relabels.into_iter().rev() {
        store.relabel(&threads, &removed, &added).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_addresses_can_be_blocked() {
        for plain in [
            "ann@x.com",
            "a.b+news@mail.example.co.uk",
            "_x-1@sub-1.example.org",
        ] {
            assert!(is_plain_address(plain), "{plain}");
        }
        for hostile in [
            "a@b.com\" OR in:inbox OR \"x@y.com",
            "x@evil.com OR gmail.com",
            "-x@y.com",
            "{a b}@x.com",
            "a@b.com in:inbox",
            "a@-b.com",
            "a@b",
            "",
        ] {
            assert!(!is_plain_address(hostile), "{hostile}");
        }
    }
}
