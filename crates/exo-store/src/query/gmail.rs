//! Compiler from parsed query trees to Gmail search strings (`q=`).

use super::{Kind, LabelIndex, Node, Token, label_slug};

/// The Gmail search for `node`. `top` marks a split's top level, whose AND isn't parenthesized.
/// `important` is the Important split's definition, which `in:important` and `in:other` stand
/// for in a search.
pub(super) fn gmail_query(
    node: &Node,
    top: bool,
    labels: &LabelIndex,
    me: &[String],
    important: Option<&str>,
) -> String {
    match gmail_part(node, top, labels, me, important) {
        GmailPart::Text(text) => text,
        GmailPart::Empty | GmailPart::True | GmailPart::False => String::new(),
    }
}

/// A part of the Gmail search. Gmail can only add threads that have no notes (threads with notes
/// are stored whole), so a `notes:` term is decided for it: a positive one is false and a negated
/// one true, and the groups around them simplify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GmailPart {
    Text(String),
    /// Nothing to say (an Auto Label, empty text), left out of its group as before notes.
    Empty,
    /// Holds for every thread without notes: no restriction.
    True,
    /// Holds for none: no thread Gmail can add matches.
    False,
}

/// The Gmail search part for `node` ([`gmail_query`]).
pub(super) fn gmail_part(
    node: &Node,
    top: bool,
    labels: &LabelIndex,
    me: &[String],
    important: Option<&str>,
) -> GmailPart {
    let minus = if node.negated() { "-" } else { "" };
    let token = match node {
        Node::And(children, negated) | Node::Or(children, negated) => {
            let all = matches!(node, Node::And(..));
            let parts: Vec<GmailPart> = children
                .iter()
                .map(|child| gmail_part(child, false, labels, me, important))
                .collect();
            let (absorbing, identity) = if all {
                (GmailPart::False, GmailPart::True)
            } else {
                (GmailPart::True, GmailPart::False)
            };
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|part| match part {
                    GmailPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let combined = if parts.contains(&absorbing) {
                absorbing
            } else if texts.is_empty() {
                if parts.contains(&identity) {
                    identity
                } else {
                    GmailPart::Empty
                }
            } else {
                let joined = texts.join(" ");
                return GmailPart::Text(if all && top && !negated {
                    joined
                } else if all {
                    format!("{minus}({joined})")
                } else {
                    format!("{minus}{{{joined}}}")
                });
            };
            return match (combined, negated) {
                (GmailPart::True, true) => GmailPart::False,
                (GmailPart::False, true) => GmailPart::True,
                (combined, _) => combined,
            };
        }
        Node::Leaf(token) => token,
    };
    if token.kind == Kind::Notes {
        return if token.negated {
            GmailPart::True
        } else {
            GmailPart::False
        };
    }
    if token.value.as_deref() == Some("") {
        return GmailPart::Empty;
    }
    let text = gmail_leaf(token, labels, me, important);
    if text.is_empty() {
        GmailPart::Empty
    } else {
        GmailPart::Text(text)
    }
}

/// A leaf's Gmail search, empty for one Gmail can't be asked about.
fn gmail_leaf(token: &Token, labels: &LabelIndex, me: &[String], important: Option<&str>) -> String {
    let minus = if token.negated { "-" } else { "" };
    let value = if token.quoted {
        format!("\"{}\"", token.value())
    } else {
        token.value().to_owned()
    };
    match token.kind {
        Kind::Label if important.is_some() && token.value().eq_ignore_ascii_case("important") => {
            format!("{minus}{}", important.unwrap_or_default())
        }
        Kind::Label if important.is_some() && token.value().eq_ignore_ascii_case("other") => {
            format!(
                "{}{}",
                if token.negated { "" } else { "-" },
                important.unwrap_or_default()
            )
        }
        Kind::Label => {
            let slug = label_slug(token.value());
            match labels.by_slug.get(&slug) {
                Some(label) if label_slug(&label.name) != slug => {
                    format!("{minus}label:{}", label_slug(&label.name))
                }
                _ if token
                    .value()
                    .chars()
                    .any(|c| matches!(c, '!'..='/' | '@' | ';')) =>
                {
                    format!("{minus}label:{slug}")
                }
                _ => format!("{minus}{}:{value}", token.field),
            }
        }
        // Exo has no Auto Labels to name, and notes never leave the device.
        Kind::AutoLabel | Kind::Notes => String::new(),
        Kind::Contact if token.value().eq_ignore_ascii_case("me") && !token.quoted => {
            format!("{minus}{}:{{{}}}", token.field, me.join(" "))
        }
        Kind::Contact if is_dot_me(token.value()) => format!("{minus}{}:*.me", token.field),
        Kind::Contact | Kind::Header => format!("{minus}{}:{value}", token.field),
        Kind::Attachment => format!("{minus}has:attachment"),
        Kind::Shared => format!("{minus}from:\"sharing@superhuman.com\""),
        Kind::Filename => format!("{minus}filename:{value}"),
        Kind::After => format!("{minus}after:{value}"),
        Kind::Category => format!("{minus}category:{}", token.value()),
        Kind::Text => format!("{minus}{}", value.replace('\'', "")),
    }
}

/// Whether the value is `me` after nothing but punctuation (`.me`, `*.me`), ignoring case.
fn is_dot_me(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.strip_suffix("me").is_some_and(|rest| {
        rest.chars()
            .all(|c| !(c.is_ascii_alphanumeric() || c == '_'))
    })
}
