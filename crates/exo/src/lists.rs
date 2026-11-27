//! The lists Exo shows (its views over Gmail labels), what they're called, and how `g` sequences
//! and the palette's `go to` rows reach them.

use exo_store::{Label, LabelKind, ListId, SplitInbox};

use crate::mailbox::{EXO_LABEL_PREFIX, FOREIGN_STATE_LABEL_PREFIX};

/// The name a view's header and the window title show. `splits` are the account's, using Outlook
/// folder wording (`Flagged`, `Junk Email`, `Deleted Items`) when `outlook` is set.
pub fn name_for<'a>(
    list: &ListId,
    labels: &'a [Label],
    splits: impl IntoIterator<Item = &'a SplitInbox>,
    outlook: bool,
) -> &'a str {
    match list {
        ListId::Inbox => "Inbox",
        ListId::Important => "Important",
        ListId::Other => "Other",
        ListId::Done => "Done",
        ListId::Starred if outlook => "Flagged",
        ListId::Starred => "Starred",
        ListId::Sent => "Sent",
        ListId::Drafts => "Drafts",
        ListId::Spam if outlook => "Junk Email",
        ListId::Spam => "Spam",
        ListId::Trash if outlook => "Deleted Items",
        ListId::Trash => "Trash",
        ListId::Muted => "Muted",
        ListId::Scheduled => "Scheduled",
        ListId::Reminders => "Reminders",
        ListId::All => "All Mail",
        ListId::Label(id) => labels
            .iter()
            .find(|label| label.id == *id)
            .map_or("", |label| label.name.as_str()),
        ListId::Split(id) => splits
            .into_iter()
            .find(|split| split.id == *id)
            .map_or("", SplitInbox::name),
    }
}

/// Whether `list` is one of the inbox views, which have the tab bar with counts and Add Split,
/// and Inbox Zero when empty.
pub fn is_inbox(list: &ListId) -> bool {
    crate::splits::is_inbox_view(list)
}

/// A system view's `go to` row and `g` sequence.
pub struct GoTo {
    /// The view.
    pub list: ListId,
    /// The row's id (the label's slug).
    pub id: &'static str,
    /// The view's name (`go to {name}`).
    pub name: &'static str,
    /// Extra `go to {alias}` names that find the row (`Spam` for `Junk Email`).
    pub aliases: &'static [&'static str],
    /// The word that boosts the row when typed.
    pub operative_word: &'static str,
    /// The sequences that go there.
    pub keys: &'static [&'static str],
    /// The keycaps the row shows.
    pub key_code: &'static [&'static str],
    /// The row this one sorts after when both match (`go to trash` after `Trash`).
    pub follow: Option<&'static str>,
}

const fn go_to(
    list: ListId,
    id: &'static str,
    name: &'static str,
    keys: &'static [&'static str],
    key_code: &'static [&'static str],
    follow: Option<&'static str>,
) -> GoTo {
    GoTo {
        list,
        id,
        name,
        aliases: &[],
        operative_word: name,
        keys,
        key_code,
        follow,
    }
}

/// The system views before the user's labels, in the palette's order: Important and Other
/// unless the inbox is unified, All Mail spliced in after them, then provider labels in list
/// order. `g i` and `g o` aren't bound here, because they go home and to Other whatever the rows
/// are.
pub fn system_go_tos(unified: bool) -> Vec<GoTo> {
    system_go_tos_for(unified, false)
}

/// [`system_go_tos`], with Outlook folder names, aliases (`Spam`/`Trash`), and no `g s` binding
/// when `outlook` is set.
pub fn system_go_tos_for(unified: bool, outlook: bool) -> Vec<GoTo> {
    let important_other = [
        go_to(
            ListId::Important,
            "important",
            "Important",
            &[],
            &["G", "then", "I"],
            Some("MOVE_TO_IMPORTANT"),
        ),
        go_to(
            ListId::Other,
            "other",
            "Other",
            &[],
            &["G", "then", "O"],
            Some("MOVE_TO_OTHER"),
        ),
    ];
    let important_other = if unified {
        Vec::new()
    } else {
        important_other.into()
    };
    let trash = if outlook {
        GoTo {
            aliases: &["Trash"],
            ..go_to(
                ListId::Trash,
                "trash",
                "Deleted Items",
                &["g #"],
                &["G", "then", "#"],
                Some("TRASH"),
            )
        }
    } else {
        go_to(
            ListId::Trash,
            "trash",
            "Trash",
            &["g #"],
            &["G", "then", "#"],
            Some("TRASH"),
        )
    };
    let spam = if outlook {
        GoTo {
            aliases: &["Spam"],
            ..go_to(
                ListId::Spam,
                "spam",
                "Junk Email",
                &["g !"],
                &["G", "then", "!"],
                Some("MARK_SPAM"),
            )
        }
    } else {
        go_to(
            ListId::Spam,
            "spam",
            "Spam",
            &["g !"],
            &["G", "then", "!"],
            Some("MARK_SPAM"),
        )
    };
    let starred = if outlook {
        GoTo {
            aliases: &["Starred"],
            ..go_to(ListId::Starred, "flagged", "Flagged", &[], &[], None)
        }
    } else {
        go_to(
            ListId::Starred,
            "starred",
            "Starred",
            &["g s"],
            &["G", "then", "S"],
            None,
        )
    };
    important_other
        .into_iter()
        .chain([
            go_to(
                ListId::All,
                "all",
                "All Mail",
                &["g a"],
                &["G", "then", "A"],
                None,
            ),
            GoTo {
                // `go to Sent` boosts on a lowercase `sent`.
                operative_word: "sent",
                ..go_to(
                    ListId::Sent,
                    "sent",
                    "Sent",
                    &["g t"],
                    &["G", "then", "T"],
                    None,
                )
            },
            go_to(
                ListId::Inbox,
                "inbox",
                "Inbox",
                &[],
                &["G", "then", "I"],
                None,
            ),
            trash,
            go_to(
                ListId::Drafts,
                "draft",
                "Drafts",
                &["g d"],
                &["G", "then", "D"],
                None,
            ),
            spam,
            starred,
        ])
        .collect()
}

/// The system views listed after the user's labels, in the palette's order. Scheduled has no
/// `g` key.
pub fn late_go_tos() -> [GoTo; 4] {
    [
        go_to(
            ListId::Done,
            "done",
            "Done",
            &["g e"],
            &["G", "then", "E"],
            Some("MARK_DONE"),
        ),
        go_to(ListId::Scheduled, "scheduled", "Scheduled", &[], &[], None),
        go_to(
            ListId::Reminders,
            "reminders",
            "Reminders",
            &["g h"],
            &["G", "then", "H"],
            None,
        ),
        go_to(
            ListId::Muted,
            "muted",
            "Muted",
            &["g m", "g shift+m"],
            &["G", "then", "M"],
            None,
        ),
    ]
}

/// [`late_go_tos`], omitting `Muted` on Outlook accounts.
pub fn late_go_tos_for(outlook: bool) -> Vec<GoTo> {
    late_go_tos()
        .into_iter()
        .filter(|item| !(outlook && item.list == ListId::Muted))
        .collect()
}

/// The user's labels, which show as chips and can be gone to or added. That's every user label
/// but the `[Superhuman]/…` and Exo's own state labels, in Gmail's order. Hiding a label from
/// Gmail's label list doesn't hide it here.
pub fn user_labels(labels: &[Label]) -> impl Iterator<Item = &Label> {
    labels.iter().filter(|label| {
        label.kind == LabelKind::User
            && !label.name.starts_with(FOREIGN_STATE_LABEL_PREFIX)
            && !label.name.starts_with(EXO_LABEL_PREFIX)
    })
}

/// The user's labels as the palette lists them to go to, move to or add, and the folder drawer
/// lists them. That's [`user_labels`] without Auto Labels (`[News]`), which show only as chips.
pub fn listed_labels(labels: &[Label]) -> impl Iterator<Item = &Label> {
    listed_labels_for(labels, false)
}

/// [`listed_labels`], also hiding `Deleted Items/…` subfolders on Outlook accounts.
pub fn listed_labels_for(labels: &[Label], outlook: bool) -> impl Iterator<Item = &Label> {
    user_labels(labels).filter(move |label| {
        !crate::mailbox::is_auto_label(&label.name)
            && !(outlook && label.name.starts_with("Deleted Items/"))
    })
}

/// A label's slug, which is its `go to` row's id: the name lowercased, with whitespace, `&`, `(`
/// and `)` as `-`.
pub fn slug(name: &str) -> String {
    name.chars()
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
