//! The search query language, as the search bar and Split Inbox read it: the parse tree, the
//! Gmail query and the local match.
//!
//! [`SplitQuery`] is a split's definition. It's constrained to the inbox and has the validation
//! the New and Edit dialogs run. Free text never matches a split, because it can't be matched
//! locally, which the router treats as no match.
//! [`SearchQuery`] is what the search bar runs. The local index it's looked up in is in
//! `crate::search`.
//!
//! A few of the parser's and matcher's rules look like slips but are deliberate: a negated
//! group's own exclusion check ignores its negation; `before:` flips `after:`; only some token
//! kinds may define a split.

mod eval;
mod fts;
mod gmail;
mod kql;
mod parse;
mod structure;

use std::collections::HashMap;

pub(crate) use fts::{FTS_ANCHOR, Fts, Recheck, tokenize_email};
pub(crate) use kql::participants as participants_kql;
pub(crate) use structure::{Cond, NoteTerm, Structure};

use self::eval::Matcher;
use self::gmail::{GmailPart, gmail_part, gmail_query};
use self::parse::{
    bulk_refusal, constrain_to_inbox, convert_email_addresses, label_tokens, parse_leaf,
    parse_tree, simplify, split_invalidity,
};
use crate::db::AttachmentRow;
use crate::ids::labels::{
    CATEGORY_PERSONAL, CHAT, DRAFT, INBOX, SENT, SH_ALL, SH_ARCHIVED, SH_IMPORTANT,
    SH_KNOWLEDGE_BASE, SH_MUTED, SH_OTHER, SH_SCHEDULED, SH_SIGNATURES, SH_SNIPPETS, SH_SNOOZED,
    SPAM, STARRED, TRASH, UNREAD,
};
use crate::view::ThreadView;

/// What a token is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Attachment,
    Label,
    Filename,
    Header,
    After,
    Contact,
    Category,
    AutoLabel,
    Shared,
    /// `notes:`, `note:`, `has:notes`: the thread's live notes. Gmail never sees it.
    Notes,
    Text,
}

/// A leaf of the parse tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Token {
    pub(super) kind: Kind,
    /// The operator (`from`, `subject`, `in`, `is`, `label`…), empty for the kinds without one.
    pub(super) field: String,
    /// The value with its double quotes removed, or `None` for `has:attachment`, `is:shared` and
    /// a `notes:` with no value (`has:notes`), which carry none.
    pub(super) value: Option<String>,
    pub(super) quoted: bool,
    pub(super) negated: bool,
    /// The token as typed (`str`), which the "can't split on" alert quotes.
    pub(super) text: String,
}

impl Token {
    pub(super) fn value(&self) -> &str {
        self.value.as_deref().unwrap_or_default()
    }

    /// The empty text token, which `in:inbox` becomes and an empty group simplifies to.
    pub(super) fn empty() -> Self {
        Self {
            kind: Kind::Text,
            field: String::new(),
            value: Some(String::new()),
            quoted: false,
            negated: false,
            text: String::new(),
        }
    }

    pub(super) fn is_empty_text(&self) -> bool {
        self.kind == Kind::Text && self.value.as_deref() == Some("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Node {
    And(Vec<Node>, bool),
    Or(Vec<Node>, bool),
    Leaf(Token),
}

impl Node {
    pub(super) fn negated(&self) -> bool {
        match self {
            Self::And(_, negated) | Self::Or(_, negated) => *negated,
            Self::Leaf(token) => token.negated,
        }
    }

    pub(super) fn negate(&mut self) {
        match self {
            Self::And(_, negated) | Self::Or(_, negated) => *negated = !*negated,
            Self::Leaf(token) => token.negated = !token.negated,
        }
    }
}

/// The Gmail labels a query can name, keyed by label slug ([`label_slug`]). It holds a split's
/// own `labels` first, then the account's, then the built-in lists, Inbox, Starred and the
/// Primary category, each under the slug of its name and the slug of its id. Later entries win.
/// Exo's own lists (`Done`, `Reminders`…) are among the built-in ones, so `important`, `done` and
/// the like resolve to them.
#[derive(Debug, Clone, Default)]
pub struct LabelIndex {
    pub(super) by_slug: HashMap<String, LabelRef>,
}

/// A label a query names, as a split keeps it (`labels`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelRef {
    /// Gmail's id (or an `SH_…` id, for Exo's own lists).
    pub id: String,
    /// Its name.
    pub name: String,
}

/// The lists every account has, Exo's own and Gmail's system labels, by id and name.
const BUILTIN_LABELS: [(&str, &str); 17] = [
    (SH_IMPORTANT, "Important"),
    (SH_OTHER, "Other"),
    (SH_ALL, "All Mail"),
    (SH_ARCHIVED, "Done"),
    (SH_SCHEDULED, "Scheduled"),
    (SH_SNOOZED, "Reminders"),
    (SH_MUTED, "Muted"),
    (SH_SNIPPETS, "Snippets"),
    (SH_KNOWLEDGE_BASE, "Knowledge Base"),
    (SH_SIGNATURES, "Signatures"),
    (SENT, "Sent"),
    (STARRED, "Starred"),
    (UNREAD, "Unread"),
    (DRAFT, "Drafts"),
    (INBOX, "Inbox"),
    (TRASH, "Trash"),
    (SPAM, "Spam"),
];

impl LabelIndex {
    /// The index for a split that carries the `own` labels, in an account whose Gmail labels are
    /// `account` (as `(id, name)`).
    pub fn new<'a>(
        own: impl IntoIterator<Item = LabelRef>,
        account: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        let mut index = Self::default();
        let builtin = BUILTIN_LABELS.into_iter().chain([
            (INBOX, "Inbox"),
            (STARRED, "Starred"),
            (CATEGORY_PERSONAL, "category_personal"),
        ]);
        let labels =
            own.into_iter().chain(
                account
                    .into_iter()
                    .chain(builtin)
                    .map(|(id, name)| LabelRef {
                        id: id.to_owned(),
                        name: name.to_owned(),
                    }),
            );
        for label in labels {
            index.by_slug.insert(label_slug(&label.name), label.clone());
            index.by_slug.insert(label_slug(&label.id), label);
        }
        for (alias, target) in [
            ("important", "sh_important"),
            ("other", "sh_other"),
            ("all", "sh_all"),
            ("archived", "sh_archived"),
            ("done", "sh_archived"),
            ("snoozed", "sh_snoozed"),
            ("reminders", "sh_snoozed"),
            ("muted", "sh_muted"),
            ("snippets", "sh_snippets"),
        ] {
            match index.by_slug.get(target).cloned() {
                Some(label) => index.by_slug.insert(alias.to_owned(), label),
                None => index.by_slug.remove(alias),
            };
        }
        index
    }

    pub(super) fn get(&self, value: &str) -> Option<&LabelRef> {
        self.by_slug.get(&label_slug(value))
    }

    /// The label's id, or the value itself when no label has that name.
    pub(super) fn id<'a>(&'a self, value: &'a str) -> &'a str {
        self.get(value).map_or(value, |label| label.id.as_str())
    }
}

/// A label's slug: quotes dropped, `&`, parentheses and whitespace turned into `-`, and all of it
/// lower-cased.
pub fn label_slug(name: &str) -> String {
    name.chars()
        .filter(|&c| c != '"')
        .map(|c| {
            if matches!(c, '&' | '(' | ')') || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .collect::<String>()
        .to_lowercase()
}

/// Why a query can't define a split: what the alert says, and the offending part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalidity {
    /// The alert's text (its title is `Sorry!`), with lines separated by `\n` and their
    /// indentation included.
    pub text: String,
    /// The tokens or label names at fault.
    pub problem: String,
}

/// A parsed split query.
#[derive(Debug, Clone)]
pub struct SplitQuery {
    raw: String,
    tree: Node,
}

impl SplitQuery {
    /// Parses `raw` as a split's definition. Everything is constrained to `in:inbox`, and a bare
    /// address means `from:` or `to:` that address.
    pub fn parse(raw: &str) -> Self {
        let mut tree = parse_tree(raw);
        if let Node::And(children, _) | Node::Or(children, _) = &mut tree {
            let mut constrained: Vec<Node> = children.drain(..).map(constrain_to_inbox).collect();
            constrained.insert(0, Node::Leaf(parse_leaf("in:inbox", false)));
            *children = constrained;
        }
        convert_email_addresses(&mut tree);
        Self {
            raw: raw.to_owned(),
            tree: simplify(tree),
        }
    }

    /// The query as typed.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Why this query can't define a split, if it can't.
    pub fn invalidity(&self, labels: &LabelIndex) -> Option<Invalidity> {
        split_invalidity(&self.raw, &self.tree, labels)
    }

    /// The labels the query names, which a split keeps with itself.
    pub fn labels_used(&self, labels: &LabelIndex) -> Vec<LabelRef> {
        let mut tokens = Vec::new();
        label_tokens(&self.tree, &mut tokens);
        tokens
            .iter()
            .filter_map(|token| labels.get(token.value()).cloned())
            .collect()
    }

    /// The Gmail search for this split, with drafts and chats excluded and `me` spelled out as
    /// the account's addresses (`me`).
    pub fn to_gmail(&self, labels: &LabelIndex, me: &[String]) -> String {
        let body = gmail_query(&self.tree, true, labels, me, None);
        format!("-in:{DRAFT} -in:{CHAT} {body}").trim().to_owned()
    }

    /// Whether `thread` belongs in a split with this definition. `attachments` holds at least the
    /// thread's attachment rows.
    pub(crate) fn is_match(
        &self,
        thread: &ThreadView,
        attachments: &[AttachmentRow],
        labels: &LabelIndex,
        me: &[String],
    ) -> bool {
        if self.raw.is_empty() {
            return false;
        }
        let matcher = Matcher::split(thread, attachments, labels, me);
        if matcher.excludes(&self.tree) {
            return false;
        }
        matcher
            .find(&self.tree)
            .is_some_and(|found| !found.is_empty())
    }
}

/// A parsed search, as the search bar runs it.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    raw: String,
    tree: Node,
}

/// What a query's last token is, which autocomplete completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastToken {
    /// What it is.
    pub kind: TokenKind,
    /// Its operator, lower-cased (`from`, `in`, `is`, `label`…), or empty for text.
    pub field: String,
    /// Its value as typed after the operator, without quotes, or empty for `has:attachment`.
    pub value: String,
    /// The value was in double quotes.
    pub quoted: bool,
}

/// The kinds of token autocomplete tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// `in:`, `is:`, `label:`, `l:`.
    Label,
    /// `category:`.
    Category,
    /// `from:`, `to:`, `cc:`, `bcc:`, `replyto:`, `deliveredto:`.
    Contact,
    /// `filename:`, `attachment:`.
    Filename,
    /// Free text.
    Text,
    /// Anything else (`subject:`, `after:`, `has:attachment`…).
    Other,
}

impl SearchQuery {
    /// Parses `raw` as the search bar's query.
    pub fn parse(raw: &str) -> Self {
        Self {
            raw: raw.to_owned(),
            tree: simplify(parse_tree(raw)),
        }
    }

    /// The query as typed.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Whether it's nothing but whitespace and punctuation, so nothing to search for.
    pub fn is_empty(&self) -> bool {
        self.raw
            .chars()
            .all(|c| c.is_whitespace() || "-!$%^&*()_+|~=`{}[]:\";'<>?,./\\@#".contains(c))
    }

    /// The Gmail search, with drafts and chats excluded, `me` spelled out as the account's
    /// addresses and `in:important` spelled out as `important`, the Important split's definition.
    pub fn to_gmail(&self, labels: &LabelIndex, me: &[String], important: &str) -> String {
        let body = gmail_query(&self.tree, false, labels, me, Some(important));
        format!("-in:{DRAFT} -in:{CHAT} {body}").trim().to_owned()
    }

    /// The query as KQL for Graph's `$search`, in its double quotes, or `None` when KQL can ask
    /// nothing of it (labels, read state…) and every message is a candidate. What KQL can't ask
    /// is checked with [`may_match`](Self::may_match).
    pub(crate) fn to_kql(&self, me: &[String]) -> Option<String> {
        kql::search(&self.tree, me)
    }

    /// Whether a message carrying `carried` (its label keys and lists) may match: the labels and
    /// categories the query names are checked, and the rest is what [`to_kql`](Self::to_kql)
    /// asked.
    pub(crate) fn may_match(&self, carried: &[&str], labels: &LabelIndex) -> bool {
        kql::may_match(&self.tree, carried, labels)
    }

    /// The words and subject phrases that show in bold in a result's subject and snippet.
    pub fn highlightable_tokens(&self) -> Vec<String> {
        fts::highlightable_tokens(&self.tree)
    }

    /// What local results are checked for again: among the words and subject operators, the
    /// quoted phrases and the words that the index's stemming matches too loosely.
    /// When `all` is true, they are every word there is, and the results' highlights are redone
    /// with them.
    pub(crate) fn rechecks(&self) -> (Vec<Recheck>, bool) {
        fts::rechecks(&self.tree)
    }

    /// The values of address operators and words, which are highlighted in results' names where
    /// they match.
    pub fn highlightable_contact_tokens(&self) -> Vec<String> {
        fts::highlightable_contact_tokens(&self.tree)
    }

    /// The last token, which autocomplete completes, or `None` for an empty query.
    pub fn last_token(&self) -> Option<LastToken> {
        let mut node = &self.tree;
        while let Node::And(children, _) | Node::Or(children, _) = node {
            node = children.last()?;
        }
        let Node::Leaf(token) = node else { return None };
        if token.is_empty_text() {
            return None;
        }
        let kind = match token.kind {
            Kind::Label => TokenKind::Label,
            Kind::Category => TokenKind::Category,
            Kind::Contact => TokenKind::Contact,
            Kind::Filename => TokenKind::Filename,
            Kind::Text => TokenKind::Text,
            Kind::Header
            | Kind::After
            | Kind::Attachment
            | Kind::AutoLabel
            | Kind::Shared
            | Kind::Notes => TokenKind::Other,
        };
        Some(LastToken {
            kind,
            field: token.field.clone(),
            value: token.value().to_owned(),
            quoted: token.quoted,
        })
    }

    /// Why a bulk action can't run on this search's results, in the alert's words. The cause is
    /// a word, which Gmail and Exo match differently, or anything else that Gmail can't search
    /// for the way Exo means it (Exo's own lists, a label that doesn't exist).
    pub fn bulk_refusal(&self, labels: &LabelIndex) -> Option<String> {
        bulk_refusal(&self.tree, labels)
    }

    /// The label ids the query asks for. A result that loses one of them leaves the results.
    pub(crate) fn included_labels(&self, labels: &LabelIndex) -> Vec<String> {
        let mut tokens = Vec::new();
        label_tokens(&self.tree, &mut tokens);
        tokens
            .iter()
            .filter(|token| !token.negated)
            .map(|token| labels.id(token.value()).to_owned())
            .collect()
    }

    /// Whether the query asks for the label with this slug (`in:trash`, `in:sent`).
    pub fn shows_label(&self, slug: &str) -> bool {
        let mut tokens = Vec::new();
        label_tokens(&self.tree, &mut tokens);
        tokens
            .iter()
            .any(|token| !token.negated && token.value().eq_ignore_ascii_case(slug))
    }

    /// Whether Gmail's search endpoint must receive `includeSpamTrash=true` for this query,
    /// matching the local index's inclusion of Trash and Spam (`in:trash`, `in:spam`, `in:all`,
    /// `in:anywhere`).
    pub(crate) fn includes_spam_trash(&self) -> bool {
        ["trash", "spam", "all", "anywhere"]
            .iter()
            .any(|label| self.shows_label(label))
    }

    /// Whether the query has a token that only the thread itself can confirm (a contact), so
    /// index hits are checked with [`is_match`](Self::is_match).
    pub(crate) fn checks_contacts(&self) -> bool {
        fn walk(node: &Node) -> bool {
            match node {
                Node::And(children, _) | Node::Or(children, _) => children.iter().any(walk),
                Node::Leaf(token) => matches!(token.kind, Kind::Contact | Kind::Shared),
            }
        }
        walk(&self.tree)
    }

    /// A quick test that rules `thread` out (a Gmail hit that no longer matches).
    pub(crate) fn excludes(
        &self,
        thread: &ThreadView,
        attachments: &[AttachmentRow],
        labels: &LabelIndex,
        me: &[String],
    ) -> bool {
        Matcher::search(thread, attachments, labels, me).excludes(&self.tree)
    }

    /// Whether `thread` matches, at thread level, or `None` when the query has a token that only
    /// the index can answer (text, headers).
    pub(crate) fn is_match(
        &self,
        thread: &ThreadView,
        attachments: &[AttachmentRow],
        labels: &LabelIndex,
        me: &[String],
    ) -> Option<bool> {
        let matcher = Matcher::search(thread, attachments, labels, me);
        if matcher.excludes(&self.tree) {
            return Some(false);
        }
        matcher.find(&self.tree).map(|found| !found.is_empty())
    }

    /// The local index lookup for this query. It reads every token as if they were all joined by
    /// AND, so an `OR` query only finds threads that match both sides locally, and Gmail's results
    /// add the rest. It leaves out Trash and Spam unless the query names them or `in:all`, and
    /// Snippets unless the query names them.
    pub(crate) fn fts(&self, labels: &LabelIndex, me: &[String]) -> Fts {
        fts::compile(&self.tree, labels, me)
    }

    /// Whether the query has a `notes:` term, which [`structure`](Self::structure) then answers.
    pub fn has_notes_terms(&self) -> bool {
        fts::leaves(&self.tree)
            .iter()
            .any(|token| token.kind == Kind::Notes)
    }

    /// The words and `notes:` values a note can match, as typed: the ones the query looks for,
    /// not those it rules out (under an odd number of negations). A note opened from the results
    /// shows them highlighted.
    pub fn note_words(&self) -> Vec<String> {
        fn walk(node: &Node, negated: bool, out: &mut Vec<String>) {
            let negated = negated != node.negated();
            match node {
                Node::And(children, _) | Node::Or(children, _) => {
                    children.iter().for_each(|child| walk(child, negated, out))
                }
                Node::Leaf(token)
                    if !negated
                        && matches!(token.kind, Kind::Text | Kind::Notes)
                        && !token.value().trim().is_empty() =>
                {
                    let word = token.value().trim().to_owned();
                    if !out.contains(&word) {
                        out.push(word);
                    }
                }
                Node::Leaf(_) => {}
            }
        }
        let mut out = Vec::new();
        walk(&self.tree, false, &mut out);
        out
    }

    /// Whether every match must have a live note: with each `notes:` term decided for a thread
    /// without notes (a positive one false, a negated one true), the query can't hold. Threads
    /// with notes are stored whole, so such a query is answered locally alone, and Gmail, which
    /// can't search notes, isn't asked.
    pub fn requires_a_note(&self) -> bool {
        // The Gmail search comes out false exactly when no thread without notes can match.
        gmail_part(&self.tree, false, &LabelIndex::default(), &[], Some("")) == GmailPart::False
    }

    /// The phrases and stemmed words that every local result must have (or, negated, not have)
    /// as typed: those of words and `subject:` terms the whole query requires.
    pub(crate) fn required_rechecks(&self) -> Vec<Recheck> {
        let required: Vec<&Token> = match &self.tree {
            Node::And(children, false) => children
                .iter()
                .filter_map(|child| match child {
                    Node::Leaf(token) => Some(token),
                    _ => None,
                })
                .collect(),
            Node::Leaf(token) => vec![token],
            _ => Vec::new(),
        };
        required
            .into_iter()
            .filter(|token| {
                token.kind == Kind::Text || (token.kind == Kind::Header && token.field == "subject")
            })
            .filter_map(fts::recheck_of)
            .collect()
    }

    /// The query as a condition on a thread, with its real AND, OR and NOT, for a query with a
    /// `notes:` term: the flattened lookup ([`fts`](Self::fts)) leaves Gmail to find the rest,
    /// and Gmail can't search notes. The default exclusions are ANDed on.
    pub(crate) fn structure(&self, labels: &LabelIndex, me: &[String]) -> Structure {
        structure::compile(&self.tree, labels, me)
    }
}

#[cfg(test)]
mod tests {
    use super::eval::words;
    use super::*;
    use crate::view::tests::{ME, message, thread};

    fn labels() -> LabelIndex {
        LabelIndex::new(
            [],
            [
                ("Label_1", "Work"),
                ("Label_2", "Team Updates"),
                ("CATEGORY_SOCIAL", "CATEGORY_SOCIAL"),
                ("IMPORTANT", "IMPORTANT"),
                ("UNREAD", "UNREAD"),
            ],
        )
    }

    fn gmail(query: &str) -> String {
        SplitQuery::parse(query).to_gmail(&labels(), &[ME.to_owned()])
    }

    fn invalid(query: &str) -> Option<String> {
        SplitQuery::parse(query)
            .invalidity(&labels())
            .map(|invalidity| invalidity.text)
    }

    #[test]
    fn split_definitions_become_gmail_queries() {
        // A Team split, and the default Calendar split.
        assert_eq!(
            gmail("from:brightmoor.co to:me"),
            format!("-in:DRAFT -in:CHAT in:inbox from:\"brightmoor.co\" to:{{{ME}}}")
        );
        assert_eq!(
            gmail("is:starred"),
            "-in:DRAFT -in:CHAT in:inbox is:starred"
        );
        assert_eq!(
            gmail("filename:ics"),
            "-in:DRAFT -in:CHAT in:inbox filename:ics"
        );
        assert_eq!(
            gmail(
                "from:{notifications@calendly.com, calendar-notification@google.com, calendar@superhuman.com} OR filename:ics"
            ),
            // `OR` after a `{…}` group joins that group rather than wrapping it.
            "-in:DRAFT -in:CHAT in:inbox {from:\"notifications@calendly.com\" from:\"calendar-notification@google.com\" from:\"calendar@superhuman.com\" filename:ics}"
        );
        assert_eq!(
            gmail("from:a@x.com OR from:b@y.com"),
            "-in:DRAFT -in:CHAT in:inbox {from:\"a@x.com\" from:\"b@y.com\"}"
        );
        assert_eq!(
            gmail("(from:a OR from:b) to:me"),
            format!("-in:DRAFT -in:CHAT in:inbox {{from:a from:b}} to:{{{ME}}}")
        );
        assert_eq!(
            gmail("label:Label_1 -subject:report"),
            "-in:DRAFT -in:CHAT in:inbox label:work -subject:report"
        );
        assert_eq!(
            gmail("label:\"Team Updates\""),
            "-in:DRAFT -in:CHAT in:inbox label:\"Team Updates\""
        );
        assert_eq!(
            gmail("in:inbox alice@example.com"),
            "-in:DRAFT -in:CHAT in:inbox {from:\"alice@example.com\" to:\"alice@example.com\"}"
        );
        assert_eq!(
            gmail("-alice@example.com"),
            "-in:DRAFT -in:CHAT in:inbox -{from:\"alice@example.com\" to:\"alice@example.com\"}"
        );
        assert_eq!(
            gmail("-from:me"),
            format!("-in:DRAFT -in:CHAT in:inbox -from:{{{ME}}}")
        );
        assert_eq!(gmail(""), "-in:DRAFT -in:CHAT in:inbox");
    }

    #[test]
    fn invalid_definitions_get_an_alert_saying_why() {
        assert_eq!(invalid("from:a@b.com"), None);
        assert_eq!(invalid("subject:\"survey results\""), None);
        assert_eq!(
            invalid("label:Work in:category_social after:2024/01/01 has:attachment list:x"),
            None
        );
        assert_eq!(
            invalid("budget"),
            Some("We can't split your inbox on text searches.\n\n            Please try subject:\"budget\", or from:\"budget\".".into())
        );
        assert_eq!(
            invalid("subject:\"a\"b\""),
            Some("We can't split your inbox on subjects with quotes.\n\n              Please remove this and try again.".into())
        );
        assert_eq!(
            invalid("label:Nope"),
            Some("We can't split your inbox on labels that don't exist.\n\n            Please create label nope first or try splitting on a different label.".into())
        );
        assert_eq!(
            invalid("label:a label:b"),
            Some("We can't split your inbox on labels that don't exist.\n\n            Please create labels first or try splitting on a different label.".into())
        );
        assert_eq!(
            invalid("is:unread"),
            Some("We can't split your inbox on is:unread\n\n            Please remove this and try again.".into())
        );
        assert_eq!(
            invalid("in:done"),
            Some("We can't split your inbox on in:done\n\n            Please remove this and try again.".into())
        );
        assert!(
            invalid("autolabel:x")
                .is_some_and(|text| text.starts_with("We can't split your inbox with Auto Labels"))
        );
    }

    #[test]
    fn splits_match_threads_locally() {
        let me = [ME.to_owned()];
        let labels = labels();
        let matches = |query: &str, view: &ThreadView| {
            SplitQuery::parse(query).is_match(view, &[], &labels, &me)
        };
        let mut from_github = message(
            "m1",
            1_700_000_000_000,
            "notifications@github.com",
            &[ME],
            &["INBOX", "Label_1"],
        );
        from_github.subject = "[repo] Fix the build (#12)".into();
        let view = thread(vec![from_github]);
        assert!(matches("from:github.com", &view));
        assert!(matches("from:notifications@github.com", &view));
        assert!(matches("from:github", &view));
        assert!(!matches("from:hub.com", &view));
        assert!(matches("to:me", &view));
        assert!(matches("label:Work", &view));
        assert!(matches("subject:\"fix the build\"", &view));
        assert!(!matches("subject:fix -label:work", &view));
        assert!(matches("from:nobody OR label:work", &view));
        assert!(!matches("budget", &view), "text can't be matched locally");
        assert!(!matches("", &view), "an empty definition matches nothing");
        assert!(matches("after:2023/01/01", &view));
        assert!(!matches("before:2023/01/01", &view));
        assert!(
            matches("from:github.com -ada@example.com", &view),
            "negated bare address in split excludes that address, keeping others"
        );
        assert!(
            !matches(&format!("from:github.com -{ME}"), &view),
            "negated bare address in split excludes matching recipient"
        );
        let archived = thread(vec![message(
            "m2",
            1,
            "notifications@github.com",
            &[ME],
            &[],
        )]);
        assert!(!matches("from:github.com", &archived), "only inbox threads");
    }

    /// A forwarded lease thread had a Teams invite as a reply a month before its latest
    /// message. The Calendar split's `filename:ics` looks at the latest message, so an old
    /// invite doesn't keep the thread there. Other file names still match anywhere.
    #[test]
    fn a_calendar_split_takes_threads_whose_latest_message_is_an_invite() {
        let (me, labels) = ([ME.to_owned()], labels());
        let file = |message: &str, filename: &str, mime_type: &str| AttachmentRow {
            message_id: message.into(),
            part_id: "1".into(),
            attachment_id: Some(format!("{message}-{filename}")),
            filename: filename.into(),
            mime_type: mime_type.into(),
            size: 1,
            content_id: None,
            inline: false,
        };
        let matches = |query: &str, view: &ThreadView, files: &[AttachmentRow]| {
            SplitQuery::parse(query).is_match(view, files, &labels, &me)
        };
        let calendar = crate::split::CALENDAR_QUERY;
        let invite = || message("m1", 1_000, "ana@x.com", &[ME], &["INBOX"]);
        let reply = || message("m2", 2_000, "ana@x.com", &[ME], &["INBOX"]);
        let files = [
            file("m1", "invite.ics", "text/calendar"),
            file("m1", "lease.pdf", "application/pdf"),
        ];
        assert!(matches(calendar, &thread(vec![invite()]), &files));
        let replied = thread(vec![invite(), reply()]);
        assert!(
            !matches(calendar, &replied, &files),
            "an old invite mid-thread doesn't keep the thread in Calendar"
        );
        assert!(!matches("filename:ics", &replied, &files));
        let reinvited = thread(vec![
            message("m0", 500, "ana@x.com", &[ME], &["INBOX"]),
            invite(),
        ]);
        assert!(
            matches(calendar, &reinvited, &files),
            "a new invite on an old thread puts it there"
        );
        assert!(
            matches("filename:pdf", &replied, &files),
            "other file names still match anywhere in the thread"
        );
    }

    fn fts(query: &str) -> Fts {
        SearchQuery::parse(query).fts(&labels(), &[ME.to_owned()])
    }

    #[test]
    fn searches_become_gmail_queries() {
        let to_gmail = |query: &str| {
            SearchQuery::parse(query).to_gmail(
                &labels(),
                &[ME.to_owned()],
                "((category:personal) OR from:\"via Google\")",
            )
        };
        assert_eq!(to_gmail("budget"), "-in:DRAFT -in:CHAT budget");
        assert_eq!(
            to_gmail("from:ana budget"),
            "-in:DRAFT -in:CHAT (from:ana budget)"
        );
        assert_eq!(
            to_gmail("in:important"),
            "-in:DRAFT -in:CHAT ((category:personal) OR from:\"via Google\")"
        );
        assert_eq!(
            to_gmail("in:other"),
            "-in:DRAFT -in:CHAT -((category:personal) OR from:\"via Google\")"
        );
        assert_eq!(
            to_gmail("to:me is:unread"),
            format!("-in:DRAFT -in:CHAT (to:{{{ME}}} is:unread)")
        );
        assert_eq!(
            to_gmail("label:\"Team Updates\" \"be brilliant\""),
            "-in:DRAFT -in:CHAT (label:\"Team Updates\" \"be brilliant\")"
        );
    }

    #[test]
    fn searches_read_the_index_as_one_and() {
        assert_eq!(
            fts("budget").matches,
            [(
                false,
                "budget NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS".to_owned()
            )]
        );
        assert_eq!(
            fts("in:trash -from:ana").matches[0].1,
            "labels:TRASH NOT from:ana NOT labels:SPAM NOT labels:SH_SNIPPETS"
        );
        assert_eq!(
            fts("in:all").matches[0].1,
            "meta:thread NOT labels:SH_SNIPPETS"
        );
        let me = tokenize_email(ME);
        assert_eq!(
            fts("to:me").matches[0].1,
            format!(
                "(to:{me} OR cc:{me} OR bcc:{me}) NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS"
            )
        );
        assert_eq!(
            fts("from:a OR budget").matches[0].1,
            "from:a AND budget NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS",
            "OR reads as AND locally"
        );
        let phrase = fts("\"be brilliant\" -\"no way\" subject:\"q4 plan\"");
        assert_eq!(
            phrase.matches[0].1,
            "\"be brilliant\" NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS"
        );
        assert_eq!(
            phrase.matches[1..],
            [
                (true, "\"no way\"".to_owned()),
                (false, "subject : \"q4 plan\"".to_owned())
            ]
        );
        assert_eq!(fts("before:2000/01/01").dates.len(), 1);
        assert!(fts("before:2000/01/01").dates[0].0, "before");
        assert_eq!(
            fts("after:nonsense").dates,
            [(true, 0)],
            "a bad date matches nothing"
        );
        // Whatever is typed stays inside one FTS5 string.
        assert_eq!(
            fts("NEAR(a\"").matches[0].1,
            "\"NEAR\" AND a NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS"
        );
        assert_eq!(
            fts("subject:\"x\"\" OR 1\"").matches[1].1,
            "subject : \"x\""
        );
        assert_eq!(
            fts("OR").matches[0].1,
            "meta:thread NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS"
        );
        assert_eq!(
            fts("budget*").matches[0].1,
            "\"budget\"* NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS"
        );
        for kw in ["OR*", "AND*", "NOT*", "NEAR*", "or*"] {
            let head = kw.trim_end_matches('*');
            assert_eq!(
                fts(kw).matches[0].1,
                format!("\"{head}\"* NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS")
            );
            let col_q = format!("label:{kw}");
            assert_eq!(
                fts(&col_q).matches[0].1,
                format!("labels:\"{kw}\" NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS")
            );
        }
        assert!(fts("BEFORE:2000/01/01").dates[0].0, "uppercase BEFORE:");
        assert!(fts("Older:2000/01/01").dates[0].0, "mixed-case Older:");
        assert!(!fts("AFTER:2000/01/01").dates[0].0, "uppercase AFTER:");
        assert_eq!(
            fts("isread:YES hasattachment:TRUE isflagged:YES").matches[0].1,
            "meta:attachment NOT labels:UNREAD AND labels:STARRED NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS"
        );
    }

    #[test]
    fn the_last_token_is_what_autocomplete_completes() {
        let last = |query: &str| {
            SearchQuery::parse(query)
                .last_token()
                .map(|token| (token.kind, token.field, token.value))
        };
        assert_eq!(
            last("fr"),
            Some((TokenKind::Text, String::new(), "fr".into()))
        );
        assert_eq!(
            last("budget from:"),
            Some((TokenKind::Contact, "from".into(), String::new()))
        );
        assert_eq!(
            last("in:sen"),
            Some((TokenKind::Label, "in".into(), "sen".into()))
        );
        assert_eq!(last(""), None);
        assert_eq!(
            last("notes:over"),
            Some((TokenKind::Other, "notes".into(), "over".into()))
        );
        assert!(SearchQuery::parse("  -- ").is_empty());
        assert!(!SearchQuery::parse("a").is_empty());
        assert!(SearchQuery::parse("in:trash").shows_label("trash"));
        assert!(!SearchQuery::parse("-in:trash").shows_label("trash"));
    }

    #[test]
    fn notes_tokens_never_reach_gmail() {
        let to_gmail =
            |query: &str| SearchQuery::parse(query).to_gmail(&labels(), &[ME.to_owned()], "");
        assert_eq!(to_gmail("-notes:x from:a"), "-in:DRAFT -in:CHAT (from:a)");
        assert_eq!(
            to_gmail("-(notes:x budget)"),
            "-in:DRAFT -in:CHAT",
            "a thread without notes always matches"
        );
        assert_eq!(to_gmail("-notes:x OR budget"), "-in:DRAFT -in:CHAT");
        assert_eq!(
            to_gmail("from:a (notes:x OR budget)"),
            "-in:DRAFT -in:CHAT (from:a {budget})"
        );
        assert_eq!(
            to_gmail("-(notes:x OR budget) from:a"),
            "-in:DRAFT -in:CHAT (-{budget} from:a)"
        );
        assert_eq!(to_gmail("has:notes"), "-in:DRAFT -in:CHAT");
        assert_eq!(to_gmail("-notes:\"a b\" -note:"), "-in:DRAFT -in:CHAT");
        assert_eq!(
            gmail("-notes:x from:a"),
            "-in:DRAFT -in:CHAT in:inbox from:a"
        );
    }

    #[test]
    fn notes_tokens_read_the_notes_index() {
        let notes = |query: &str| fts(query).notes;
        assert_eq!(notes("notes:overage"), [(false, "\"overage\"".to_owned())]);
        assert_eq!(
            notes("note:\"keep the cost\""),
            [(false, "\"keep the cost\"".to_owned())]
        );
        assert_eq!(notes("notes:over*"), [(false, "\"over\"*".to_owned())]);
        assert_eq!(notes("notes:"), [(false, String::new())]);
        assert_eq!(notes("has:notes"), [(false, String::new())]);
        assert_eq!(notes("-has:note"), [(true, String::new())]);
        assert_eq!(notes("-notes:x"), [(true, "\"x\"".to_owned())]);
        assert_eq!(
            notes("notes:\""),
            [(false, String::new())],
            "nothing to match"
        );
        assert_eq!(
            notes("-budget"),
            [(true, "budget".to_owned())],
            "a word's notes"
        );
        assert!(SearchQuery::parse("notes:x").has_notes_terms());
        assert!(SearchQuery::parse("-has:notes").has_notes_terms());
        assert!(!SearchQuery::parse("notes").has_notes_terms());
        let words = fts("budget \"a plan\" -other notes:x");
        assert_eq!(words.words, ["budget", "\"a plan\""]);
        assert_eq!(
            words.matches[0].1,
            "budget AND \"a plan\" NOT other NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS",
            "the index's own expression is as it was"
        );
        assert_eq!(
            words.loose.as_deref(),
            Some(
                "(budget OR meta:thread) AND (\"a plan\" OR meta:thread) NOT other NOT labels:TRASH NOT labels:SPAM NOT labels:SH_SNIPPETS"
            )
        );
        assert_eq!(fts("from:a").loose, None);
    }

    #[test]
    fn note_words_are_what_a_note_can_match() {
        let words = |query: &str| SearchQuery::parse(query).note_words();
        assert_eq!(words("notes:slack renewal from:dana"), ["slack", "renewal"]);
        assert_eq!(
            words("notes:\"keep the cost flat\""),
            ["keep the cost flat"]
        );
        assert_eq!(words("has:notes"), Vec::<String>::new());
        assert_eq!(words("budget -notes:x -(overage OR fee)"), ["budget"]);
        assert_eq!(words("-(-renewal)"), ["renewal"]);
    }

    #[test]
    fn a_query_with_notes_keeps_its_structure() {
        let requires = |query: &str| SearchQuery::parse(query).requires_a_note();
        assert!(requires("notes:x"));
        assert!(requires("has:notes budget"));
        assert!(requires("notes:x OR notes:y"));
        assert!(requires("from:a (notes:x OR notes:y)"));
        assert!(!requires("notes:x OR budget"));
        assert!(!requires("-notes:x budget"));
        assert!(!requires("-(notes:x budget)"));
        assert!(!requires("budget"));
        assert!(requires("-(-notes:x OR budget) from:a"));
        assert!(!requires("-notes:x"));

        let structure =
            SearchQuery::parse("-(notes:x budget)").structure(&labels(), &[ME.to_owned()]);
        let Cond::All(parts) = &structure.cond else {
            panic!("{:?}", structure.cond)
        };
        assert_eq!(
            parts[0],
            Cond::Not(Box::new(Cond::All(vec![
                Cond::Note(0),
                Cond::Any(vec![Cond::Mail("budget".into()), Cond::Note(1)]),
            ])))
        );
        assert!(
            structure.positive.is_empty(),
            "nothing is found under a negation"
        );
        // With no mail, it holds for a thread without those notes.
        assert_eq!(structure.cond.eval(Some(false), &|_| false), Some(true));
        assert_eq!(structure.cond.eval(None, &|at| at == 0), None);
        assert_eq!(structure.cond.eval(None, &|_| true), Some(false));
    }

    #[test]
    fn a_split_refuses_notes_tokens() {
        assert_eq!(
            invalid("from:a notes:renewal"),
            Some("We can't split your inbox on notes:renewal\n\n            Please remove this and try again.".into())
        );
        assert!(invalid("has:notes").is_some());
        assert!(
            SearchQuery::parse("notes:x from:ann")
                .bulk_refusal(&labels())
                .is_some()
        );
        let view = thread(vec![message("m1", 1, "a@x.com", &[ME], &["INBOX"])]);
        assert!(!SplitQuery::parse("has:notes").is_match(&view, &[], &labels(), &[ME.to_owned()]));
    }

    #[test]
    fn words_split_at_punctuation_keeping_empty_ends() {
        assert_eq!(words("a.b@c"), ["a", "b", "c"]);
        assert_eq!(words("-a-"), ["", "a", ""]);
        assert_eq!(words("plain"), ["plain"]);
    }

    #[test]
    fn bulk_actions_refuse_words_and_exos_own_lists() {
        let refusal = |query: &str| SearchQuery::parse(query).bulk_refusal(&labels());
        assert_eq!(refusal("from:ann@x.com in:work"), None);
        assert_eq!(
            refusal("is:unread subject:report after:2026/11/01"),
            None,
            "Unread can be acted on in bulk"
        );
        assert_eq!(
            refusal("quarterly report from:ann"),
            Some("We can't complete this action on text searches.\n\nPlease try subject:\"quarterly\" or from:\"quarterly\".".into())
        );
        let generic = Some("We can't complete this action on this set of conversations.\n\nPlease unselect these conversations and try again.".into());
        assert_eq!(refusal("in:done from:ann"), generic);
        assert_eq!(refusal("in:scheduled from:ann"), generic);
        assert_eq!(
            invalid("in:scheduled"),
            Some("We can't split your inbox on in:scheduled\n\n            Please remove this and try again.".into())
        );
        assert_eq!(
            refusal("label:nowhere"),
            generic,
            "a label that doesn't exist"
        );
    }

    #[test]
    fn quoted_to_contact_expands_across_to_cc_bcc_and_spam_trash_flag_tracks_query() {
        let quoted_to = fts("to:\"Ada Lovelace\"");
        assert_eq!(
            quoted_to.matches[1],
            (
                false,
                "(to : \"Ada Lovelace\" OR cc : \"Ada Lovelace\" OR bcc : \"Ada Lovelace\")"
                    .to_owned()
            )
        );
        let neg_to = fts("-to:\"Ada Lovelace\"");
        assert_eq!(
            neg_to.matches[1],
            (
                true,
                "(to : \"Ada Lovelace\" OR cc : \"Ada Lovelace\" OR bcc : \"Ada Lovelace\")"
                    .to_owned()
            )
        );
        // `cc:` stays scoped to `cc`.
        let quoted_cc = fts("cc:\"Ada Lovelace\"");
        assert_eq!(
            quoted_cc.matches[1],
            (false, "cc : \"Ada Lovelace\"".to_owned())
        );

        assert!(!SearchQuery::parse("from:ada").includes_spam_trash());
        assert!(!SearchQuery::parse("-in:trash").includes_spam_trash());
        assert!(SearchQuery::parse("in:trash").includes_spam_trash());
        assert!(SearchQuery::parse("in:spam").includes_spam_trash());
        assert!(SearchQuery::parse("in:all").includes_spam_trash());
        assert!(SearchQuery::parse("in:anywhere").includes_spam_trash());
    }

    #[test]
    fn word_match_matches_word_boundaries_without_regex() {
        use super::eval::word_match;
        assert!(word_match("", "anything"));
        assert!(word_match("fix", "Fix the build"));
        assert!(!word_match("fix", "prefix"));
        assert!(!word_match("bar", "foo_bar"));
        assert!(word_match("fix", "prefix fix"));
        assert!(word_match("a a", "ba a a"));
        assert!(word_match("[repo]", "foo[repo]bar"));
        assert!(word_match("[repo]", " [repo] "));
        assert!(!word_match("[repo]", "([repo])"));
        assert!(word_match("caf", "café"));
        assert!(word_match("a", "a-b"));
        assert!(word_match("b", "a-b"));
    }
}
