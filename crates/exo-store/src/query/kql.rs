//! Compiler from parsed query trees to Keyword Query Language, for Graph's `$search`, and the
//! local check of what KQL can't ask.
//!
//! KQL has words and phrases, the address and subject properties, `hasattachments`,
//! `attachment` (a file's name) and dates on `received`, joined by `AND`, `OR` and `NOT`
//! (learn.microsoft.com/graph/search-query-parameter). It has no labels, read state, stars or
//! folders, and `$search` can't be combined with `$filter`. So those tokens leave the KQL, which
//! then finds more than the query does, and a hit is kept only if the labels it carries don't
//! rule it out ([`may_match`]).

use super::parse::{category_label, parse_date};
use super::{Kind, LabelIndex, Node, Token, label_slug};

/// `node` as KQL: `None` for a node KQL matches everything for, and whether the KQL finds
/// exactly what `node` does (its negation can only be asked when it does).
fn compile(node: &Node, me: &[String], top: bool) -> (Option<String>, bool) {
    let (kql, exact) = match node {
        Node::Leaf(token) => leaf(token, me),
        Node::And(children, _) => {
            let parts: Vec<(Option<String>, bool)> = children
                .iter()
                .map(|child| compile(child, me, false))
                .collect();
            let exact = parts.iter().all(|(_, exact)| *exact);
            let words: Vec<String> = parts.into_iter().filter_map(|(kql, _)| kql).collect();
            (group(words, " AND ", top && !node.negated()), exact)
        }
        Node::Or(children, _) => {
            let parts: Vec<(Option<String>, bool)> = children
                .iter()
                .map(|child| compile(child, me, false))
                .collect();
            // A side KQL can't ask makes the whole OR match everything.
            if parts.iter().any(|(kql, _)| kql.is_none()) {
                return (None, false);
            }
            let exact = parts.iter().all(|(_, exact)| *exact);
            let words: Vec<String> = parts.into_iter().filter_map(|(kql, _)| kql).collect();
            (group(words, " OR ", false), exact)
        }
    };
    if !node.negated() || matches!(node, Node::Leaf(_)) {
        return (kql, exact);
    }
    // NOT of more than the node finds would find too little.
    match kql.filter(|_| exact) {
        Some(kql) => (Some(format!("NOT {kql}")), true),
        None => (None, false),
    }
}

/// `words` joined by `join`, parenthesized unless it's one part or the query's top level.
fn group(words: Vec<String>, join: &str, top: bool) -> Option<String> {
    match words.len() {
        0 => None,
        1 => words.into_iter().next(),
        _ if top => Some(words.join(join)),
        _ => Some(format!("({})", words.join(join))),
    }
}

/// A token as KQL, and whether it's exact. Tokens KQL can't ask are `None`, not exact.
fn leaf(token: &Token, me: &[String]) -> (Option<String>, bool) {
    if token.is_empty_text() {
        return (None, true);
    }
    let not = |kql: String| {
        if token.negated {
            format!("NOT {kql}")
        } else {
            kql
        }
    };
    let value = || quoted(token.value(), token.quoted);
    let kql = match token.kind {
        Kind::Text => match value() {
            Some(value) => value,
            None => return (None, false),
        },
        Kind::Header if token.field == "subject" => match value() {
            Some(value) => format!("subject:{value}"),
            None => return (None, false),
        },
        Kind::Contact => {
            // Gmail's `to:` is any recipient.
            let property = match token.field.as_str() {
                "from" => "from",
                "to" => "recipients",
                "cc" => "cc",
                "bcc" => "bcc",
                _ => return (None, false),
            };
            if token.value().eq_ignore_ascii_case("me") && !token.quoted {
                let mine: Vec<String> = me
                    .iter()
                    .filter_map(|address| {
                        quoted(address, false).map(|value| format!("{property}:{value}"))
                    })
                    .collect();
                match group(mine, " OR ", false) {
                    Some(any) => any,
                    None => return (None, false),
                }
            } else {
                match value() {
                    Some(value) => format!("{property}:{value}"),
                    None => return (None, false),
                }
            }
        }
        Kind::Attachment => "hasattachments:true".to_owned(),
        Kind::Filename => match value() {
            Some(value) => format!("attachment:{value}"),
            None => return (None, false),
        },
        Kind::After => {
            let Some(date) =
                parse_date(token.value()).and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
            else {
                return (None, false);
            };
            // The day as typed: `after:` reads a date in the Mac's zone. `before:` is a negated
            // `after:`.
            let day = date
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d");
            return (
                Some(if token.negated {
                    format!("received<{day}")
                } else {
                    format!("received>={day}")
                }),
                true,
            );
        }
        // Notes are Exo's own: a query needing one is answered locally, and Outlook is asked
        // without them.
        Kind::Label
        | Kind::Category
        | Kind::AutoLabel
        | Kind::Shared
        | Kind::Header
        | Kind::Notes => {
            return (None, false);
        }
    };
    (Some(not(kql)), true)
}

/// `value` as a KQL value: in (escaped) double quotes when typed so, when it matches a KQL
/// operator, or when it contains whitespace or KQL syntax characters. `None` when stripping quotes
/// and backslashes leaves nothing to search for.
pub(super) fn quoted(value: &str, was_quoted: bool) -> Option<String> {
    let clean = value.replace(['"', '\\'], "");
    if clean.is_empty() {
        return None;
    }
    let reserved = matches!(
        clean.to_ascii_uppercase().as_str(),
        "AND" | "OR" | "NOT" | "NEAR" | "ONEAR" | "XRANK"
    );
    let needs_quotes = was_quoted
        || reserved
        || clean.starts_with('-')
        || clean
            .chars()
            .any(|c| !(c.is_alphanumeric() || matches!(c, '@' | '.' | '_' | '-')));
    Some(if needs_quotes {
        format!("\\\"{clean}\\\"")
    } else {
        clean
    })
}

/// Graph's `$search` value for conversations involving `email`, safely quoted as KQL, or `None`
/// when `email` has no searchable characters.
pub(crate) fn participants(email: &str) -> Option<String> {
    let value = quoted(email, false)?;
    Some(format!("\"participants:{value}\""))
}

/// Whether a message carrying `carried` (its label keys and lists) may match `node`. Labels and
/// categories are checked; every other token is what KQL asked, so it may.
pub(super) fn may_match(node: &Node, carried: &[&str], labels: &LabelIndex) -> bool {
    check(node, carried, labels) != Some(false)
}

/// Three-valued: `None` when only Graph's search knows.
fn check(node: &Node, carried: &[&str], labels: &LabelIndex) -> Option<bool> {
    let value = match node {
        Node::And(children, _) => {
            let values: Vec<Option<bool>> = children
                .iter()
                .map(|child| check(child, carried, labels))
                .collect();
            if values.contains(&Some(false)) {
                Some(false)
            } else if values.contains(&None) {
                None
            } else {
                Some(true)
            }
        }
        Node::Or(children, _) => {
            let values: Vec<Option<bool>> = children
                .iter()
                .map(|child| check(child, carried, labels))
                .collect();
            if values.contains(&Some(true)) {
                Some(true)
            } else if values.contains(&None) {
                None
            } else {
                Some(false)
            }
        }
        Node::Leaf(token) if token.is_empty_text() => Some(true),
        Node::Leaf(token) => match token.kind {
            Kind::Label => Some(carries(token.value(), carried, labels)),
            Kind::Category => Some(carries(category_label(token).value(), carried, labels)),
            _ => None,
        },
    };
    if node.negated() {
        value.map(|matched| !matched)
    } else {
        value
    }
}

/// Whether `carried` has the label `value` names.
fn carries(value: &str, carried: &[&str], labels: &LabelIndex) -> bool {
    let wanted = label_slug(labels.id(value));
    carried.iter().any(|label| label_slug(label) == wanted)
}

/// `node`'s `$search` value: the KQL in double quotes, or `None` when KQL can ask nothing of it.
pub(super) fn search(node: &Node, me: &[String]) -> Option<String> {
    let (kql, _) = compile(node, me, true);
    kql.map(|kql| format!("\"{kql}\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchQuery;
    use crate::ids::labels::{INBOX, SH_ARCHIVED, UNREAD};

    fn kql(raw: &str) -> Option<String> {
        SearchQuery::parse(raw).to_kql(&["maya@brightmoor.co".to_owned()])
    }

    #[test]
    fn words_addresses_and_dates_are_kql() {
        assert_eq!(kql("budget").as_deref(), Some("\"budget\""));
        assert_eq!(
            kql("from:ann \"q3 plans\" has:attachment").as_deref(),
            Some(r#""from:ann AND \"q3 plans\" AND hasattachments:true""#)
        );
        assert_eq!(
            kql("to:me").as_deref(),
            Some(r#""recipients:maya@brightmoor.co""#),
            "Gmail's to: is any recipient"
        );
        assert_eq!(
            kql("subject:lunch after:2026/11/01 -from:bob").as_deref(),
            Some(r#""subject:lunch AND received>=2026-11-01 AND NOT from:bob""#)
        );
        assert_eq!(
            kql("from:ann OR from:bob").as_deref(),
            Some(r#""(from:ann OR from:bob)""#)
        );
    }

    #[test]
    fn labels_read_state_and_folders_are_left_to_the_labels() {
        assert_eq!(kql("is:unread in:inbox"), None, "nothing KQL can ask");
        assert_eq!(
            kql("in:inbox budget").as_deref(),
            Some("\"budget\""),
            "the label leaves the KQL"
        );
        assert_eq!(
            kql("budget OR is:starred"),
            None,
            "an OR with a side KQL can't ask finds everything"
        );
        assert_eq!(
            kql("-(budget is:unread)"),
            None,
            "the NOT of more than it finds would find too little"
        );
    }

    #[test]
    fn hits_are_narrowed_by_their_labels() {
        let labels = LabelIndex::new([], [(INBOX, "INBOX"), (UNREAD, "UNREAD")]);
        let query = |raw: &str| SearchQuery::parse(raw);
        let unread_inbox = [INBOX, UNREAD];
        let done = [SH_ARCHIVED];
        assert!(query("is:unread budget").may_match(&unread_inbox, &labels));
        assert!(!query("is:unread budget").may_match(&done, &labels));
        assert!(query("in:done budget").may_match(&done, &labels));
        assert!(!query("-in:inbox").may_match(&unread_inbox, &labels));
        assert!(
            query("budget OR is:starred").may_match(&done, &labels),
            "the word may be what matched"
        );
    }

    #[test]
    fn kql_quotes_operators_special_chars_and_rejects_empty_values() {
        assert_eq!(kql("NEAR").as_deref(), Some(r#""\"NEAR\"""#));
        assert_eq!(kql("ONEAR").as_deref(), Some(r#""\"ONEAR\"""#));
        assert_eq!(
            kql("from:maya+alerts@brightmoor.co").as_deref(),
            Some(r#""from:\"maya+alerts@brightmoor.co\"""#)
        );
        assert_eq!(
            SearchQuery::parse("from:me").to_kql(&["maya+work@brightmoor.co".to_owned()]),
            Some(r#""from:\"maya+work@brightmoor.co\"""#.to_owned())
        );
        assert_eq!(kql("subject:a=b").as_deref(), Some(r#""subject:\"a=b\"""#));
        // Quotes/backslashes-only values drop cleanly instead of emitting invalid `from:` / `NOT `.
        assert_eq!(kql("from:\"\""), None);
        assert_eq!(kql("-from:\"\\\"\""), None);
        assert_eq!(kql("from:\"\" budget").as_deref(), Some("\"budget\""));
        // Participants helper quotes special chars & strips injection quotes cleanly.
        assert_eq!(
            participants("maya+tag@brightmoor.co").as_deref(),
            Some(r#""participants:\"maya+tag@brightmoor.co\"""#)
        );
        assert_eq!(
            participants("a\" OR \"b").as_deref(),
            Some(r#""participants:\"a OR b\"""#)
        );
        assert_eq!(participants("\"\""), None);
    }
}
