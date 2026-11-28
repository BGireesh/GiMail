//! A query with a `notes:` term as a condition on a thread, with its real AND, OR and NOT
//! ([`super::SearchQuery::structure`]). The flattened lookup (`fts::compile`) leaves Gmail to
//! find the rest, and Gmail can't search notes.

use regex::Regex;

use super::fts::{
    Fts, default_exclusions, fts_token, fts5_prefix_or_phrase, fts5_value, full_table_match,
    is_anywhere, recheck_of,
};
use super::{Kind, LabelIndex, Node, Token};

/// How deeply groups may nest in a query that [`super::SearchQuery::structure`] reads.
const MAX_DEPTH: usize = 50;

/// How deeply `node`'s groups nest.
fn depth(node: &Node) -> usize {
    match node {
        Node::Leaf(_) => 0,
        Node::And(children, _) | Node::Or(children, _) => {
            1 + children.iter().map(depth).max().unwrap_or_default()
        }
    }
}

/// A condition on a thread, as [`super::SearchQuery::structure`] builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cond {
    True,
    All(Vec<Cond>),
    Any(Vec<Cond>),
    Not(Box<Cond>),
    /// The thread's row in the mail's index matches the FTS5 expression.
    Mail(String),
    /// `(before, epoch ms)`: the thread's date is before it, or else at or after it.
    Date(bool, i64),
    /// The thread has a live note in [`Structure::notes`] at this index.
    Note(usize),
}

impl Cond {
    /// The condition's value, where `mail` is the value of every mail and date condition
    /// (`None` when they can't be told) and `note` says whether the thread has a note in a term.
    /// `None` when it can't be told.
    pub(crate) fn eval(&self, mail: Option<bool>, note: &dyn Fn(usize) -> bool) -> Option<bool> {
        match self {
            Self::True => Some(true),
            Self::Mail(_) | Self::Date(..) => mail,
            Self::Note(at) => Some(note(*at)),
            Self::Not(inner) => inner.eval(mail, note).map(|value| !value),
            Self::All(children) => {
                let values: Vec<Option<bool>> = children
                    .iter()
                    .map(|child| child.eval(mail, note))
                    .collect();
                if values.contains(&Some(false)) {
                    Some(false)
                } else if values.iter().all(Option::is_some) {
                    Some(true)
                } else {
                    None
                }
            }
            Self::Any(children) => {
                let values: Vec<Option<bool>> = children
                    .iter()
                    .map(|child| child.eval(mail, note))
                    .collect();
                if values.contains(&Some(true)) {
                    Some(true)
                } else if values.iter().all(Option::is_some) {
                    Some(false)
                } else {
                    None
                }
            }
        }
    }
}

/// A lookup in the notes' index: the live notes matching `expression` (any live note when it's
/// empty), and of those, the ones whose text has `exact` as typed, when it's set.
#[derive(Debug, Clone)]
pub(crate) struct NoteTerm {
    pub expression: String,
    pub exact: Option<Regex>,
}

/// A query as a condition on a thread ([`super::SearchQuery::structure`]).
#[derive(Debug, Clone)]
pub(crate) struct Structure {
    pub cond: Cond,
    /// The lookups in the notes' index that [`Cond::Note`] names.
    pub notes: Vec<NoteTerm>,
    /// The note lookups a match can be found with (not under a negation), whose notes a result
    /// lists.
    pub positive: Vec<usize>,
    /// The mail index's expressions not under a negation, which highlight a result's subject and
    /// excerpt.
    pub highlight: Vec<String>,
}

/// `tree` as a condition on a thread, with the default exclusions ANDed on.
pub(super) fn compile(tree: &Node, labels: &LabelIndex, me: &[String]) -> Structure {
    let mut structure = Structure {
        cond: Cond::True,
        notes: Vec::new(),
        positive: Vec::new(),
        highlight: Vec::new(),
    };
    // A query nested deeper than this matches nothing, rather than making SQL deeper than
    // SQLite parses.
    if depth(tree) > MAX_DEPTH {
        structure.cond = Cond::Any(Vec::new());
        return structure;
    }
    let cond = structure.node(tree, false, labels, me);
    let mut all = vec![cond];
    for token in default_exclusions(tree) {
        all.push(structure.leaf(&token, false, labels, me));
    }
    structure.cond = Cond::All(all);
    structure
}

impl Structure {
    fn node(&mut self, node: &Node, flipped: bool, labels: &LabelIndex, me: &[String]) -> Cond {
        let (children, negated, all) = match node {
            Node::Leaf(token) => return self.leaf(token, flipped, labels, me),
            Node::And(children, negated) => (children, *negated, true),
            Node::Or(children, negated) => (children, *negated, false),
        };
        let conds = children
            .iter()
            .map(|child| self.node(child, flipped != negated, labels, me))
            .collect();
        let cond = if all {
            Cond::All(conds)
        } else {
            Cond::Any(conds)
        };
        if negated {
            Cond::Not(Box::new(cond))
        } else {
            cond
        }
    }

    /// `flipped` says the token sits under a negated group.
    fn leaf(&mut self, token: &Token, flipped: bool, labels: &LabelIndex, me: &[String]) -> Cond {
        if token.is_empty_text() || is_anywhere(token) {
            return Cond::True;
        }
        let positive = flipped == token.negated;
        let cond = match token.kind {
            // `before:` reads as a negated `after:`, so its negation is its direction.
            Kind::After => {
                let mut fts = Fts::default();
                fts_token(token, labels, me, &mut Vec::new(), &mut fts);
                return fts
                    .dates
                    .first()
                    .map_or(Cond::True, |(before, date)| Cond::Date(*before, *date));
            }
            Kind::Notes => {
                let expression = token
                    .value
                    .as_deref()
                    .map(fts5_prefix_or_phrase)
                    .unwrap_or_default();
                let at = self.note_term(expression, token);
                if positive {
                    self.positive.push(at);
                }
                Cond::Note(at)
            }
            Kind::Text => {
                let expression = if token.quoted {
                    fts5_prefix_or_phrase(token.value())
                } else {
                    fts5_value(token.value())
                };
                let at = self.note_term(expression.clone(), token);
                if positive {
                    self.positive.push(at);
                    self.highlight.push(expression.clone());
                }
                Cond::Any(vec![Cond::Mail(expression), Cond::Note(at)])
            }
            _ => {
                let unnegated = Token {
                    negated: false,
                    ..token.clone()
                };
                let mut terms = Vec::new();
                let mut fts = Fts::default();
                fts_token(&unnegated, labels, me, &mut terms, &mut fts);
                let mut parts: Vec<String> = Vec::new();
                if !terms.is_empty() {
                    parts.push(full_table_match(terms));
                }
                parts.extend(fts.matches.into_iter().map(|(_, expression)| expression));
                // A token with nothing to look up (an Auto Label) limits nothing.
                if parts.is_empty() {
                    return Cond::True;
                }
                if positive {
                    self.highlight.extend(parts.iter().cloned());
                }
                Cond::All(parts.into_iter().map(Cond::Mail).collect())
            }
        };
        if token.negated {
            Cond::Not(Box::new(cond))
        } else {
            cond
        }
    }

    /// The index of the notes' lookup for `expression`, checked as `token` is typed.
    fn note_term(&mut self, expression: String, token: &Token) -> usize {
        let exact = recheck_of(token).map(|recheck| recheck.regex);
        let same = |term: &NoteTerm| {
            term.expression == expression
                && term.exact.as_ref().map(Regex::as_str) == exact.as_ref().map(Regex::as_str)
        };
        if let Some(at) = self.notes.iter().position(same) {
            return at;
        }
        self.notes.push(NoteTerm { expression, exact });
        self.notes.len() - 1
    }
}
