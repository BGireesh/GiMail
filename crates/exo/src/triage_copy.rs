//! Shared triage display copy, label naming and `(Done, Remind, Command)` action-button chrome
//! across single-conversation actions, bulk actions, pickers, list rows and headers.

use egui::{Color32, Rangef, Rect, Sense, Ui, Vec2, pos2, vec2};
use exo_store::{Label, ListId, Mutation, SplitInbox};
use exo_ui::{IconButton, KeyTooltip, Placement};

use crate::inbox::{REMIND_ICON, REMIND_SET_ICON};

pub(crate) const DONE_ICON: &str = "legacy/DEPRECATED_threadActionsDone";
pub(crate) const COMMAND_ICON: &str = "legacy/DEPRECATED_threadActionsCommand";

const TRIAGE_TOOLTIP_PLACEMENT: Placement = Placement::Below {
    gap: 8.0,
    shift: 10.0,
};

/// The name of a label or folder id: a built-in one's own name, else the user's label's.
pub(crate) fn label_name<'a>(id: &str, labels: &'a [Label], outlook: bool) -> &'a str {
    match id {
        "INBOX" => "Inbox",
        "SH_IMPORTANT" => "Important",
        "SH_OTHER" => "Other",
        "SH_ARCHIVED" => "Done",
        "STARRED" if outlook => "Flagged",
        "STARRED" => "Starred",
        "SPAM" if outlook => "Junk Email",
        "SPAM" => "Spam",
        "TRASH" if outlook => "Deleted Items",
        "TRASH" => "Trash",
        id => labels
            .iter()
            .find(|label| label.id == id)
            .map_or("", |label| label.name.as_str()),
    }
}

/// A list's label id as Move and `Y` see it.
pub(crate) fn list_label(list: &ListId) -> String {
    match list {
        ListId::Inbox => "INBOX".into(),
        ListId::Important => "SH_IMPORTANT".into(),
        ListId::Other => "SH_OTHER".into(),
        ListId::Done => "SH_ARCHIVED".into(),
        ListId::Starred => "STARRED".into(),
        ListId::Sent => "SENT".into(),
        ListId::Drafts => "DRAFT".into(),
        ListId::Spam => "SPAM".into(),
        ListId::Trash => "TRASH".into(),
        ListId::Muted => "SH_MUTED".into(),
        ListId::Scheduled => "SH_SCHEDULED".into(),
        ListId::Reminders => "SH_SNOOZED".into(),
        ListId::All => "SH_ALL".into(),
        ListId::Label(id) | ListId::Split(id) => id.clone(),
    }
}

/// What `Y` takes off threads in `list`: the list's own label, or for Exo's own views and Sent,
/// the inbox. Nothing in Drafts or Reminders.
pub(crate) fn removable_label(list: &ListId) -> Option<String> {
    match list {
        ListId::Drafts | ListId::Reminders => None,
        ListId::Starred | ListId::Spam | ListId::Trash | ListId::Label(_) => Some(list_label(list)),
        ListId::Inbox
        | ListId::Important
        | ListId::Other
        | ListId::Done
        | ListId::Sent
        | ListId::Muted
        | ListId::Scheduled
        | ListId::All
        | ListId::Split(_) => Some("INBOX".into()),
    }
}

/// The toast after a move.
pub(crate) fn move_toast(from: Option<&str>, to: &str, labels: &[Label], outlook: bool) -> String {
    move_text(from, to, labels, false, outlook)
}

/// [`move_toast`], or in the present tense for a bulk move (`present`).
pub(crate) fn move_text(
    from: Option<&str>,
    to: &str,
    labels: &[Label],
    present: bool,
    outlook: bool,
) -> String {
    // The inbox, its views and its splits.
    let inbox_like = |id: &str| {
        matches!(id, "INBOX" | "SH_INBOX" | "SH_IMPORTANT" | "SH_OTHER")
            || id.starts_with(exo_store::SPLIT_ID_PREFIX)
    };
    let marking_done = from.is_some_and(|from| inbox_like(from) || to == "SH_ARCHIVED");
    let moving_to_inbox = from.is_some_and(|from| inbox_like(to) || from == "SH_ARCHIVED");
    // Out of Reminders the reminder goes too.
    let (moved, moving) = if from == Some("SH_SNOOZED") {
        ("Reminder removed and moved", "Reminder removed and moving")
    } else {
        ("Moved", "Moving")
    };
    match (
        to == "SPAM",
        marking_done && !moving_to_inbox,
        present,
        outlook,
    ) {
        (true, _, false, true) => "Marked as Junk.".into(),
        (true, _, true, true) => "Marking as Junk.".into(),
        (true, _, false, false) => "Marked as Spam.".into(),
        (true, _, true, false) => "Marking as Spam.".into(),
        (false, true, false, true) => "Moved.".into(),
        (false, true, true, true) => "Moving.".into(),
        (false, true, false, false) => "Moved and Marked Done.".into(),
        (false, true, true, false) => "Moving and Marking Done.".into(),
        (false, false, false, _) => format!("{moved} to {}.", label_name(to, labels, outlook)),
        (false, false, true, _) => format!("{moving} to {}.", label_name(to, labels, outlook)),
    }
}

/// Past- and present-tense copy for simple triage mutations, paired with the bulk action's name.
fn fixed_copy(
    mutation: &Mutation,
    outlook: bool,
) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match mutation {
        Mutation::MarkDone => ("Marked as Done.", "Marking as Done.", "Mark Done"),
        Mutation::MarkNotDone => ("Marked not Done.", "Marking not Done.", "Mark not Done"),
        Mutation::Star if outlook => ("Flagged.", "Flagging.", "Flag"),
        Mutation::Star => ("Starred.", "Starring.", "Star"),
        Mutation::Unstar if outlook => ("Unflagged.", "Unflagging.", "Unflag"),
        Mutation::Unstar => ("Unstarred.", "Unstarring.", "Unstar"),
        Mutation::MarkRead => ("Marked as Read.", "Marking as Read.", "Mark Read"),
        Mutation::MarkUnread => ("Marked as Unread.", "Marking as Unread.", "Mark Unread"),
        Mutation::Trash if outlook => ("Moved to Trash.", "Moving to Trash.", "Delete"),
        Mutation::Trash => ("Moved to Trash.", "Moving to Trash.", "Trash"),
        Mutation::Untrash if outlook => (
            "Moved to Inbox.",
            "Moving to Inbox.",
            "Remove from Deleted Items",
        ),
        Mutation::Untrash => ("Moved to Inbox.", "Moving to Inbox.", "Remove from Trash"),
        Mutation::Spam if outlook => ("Marked as Junk.", "Marking as Junk.", "Mark Junk"),
        Mutation::Spam => ("Marked as Spam.", "Marking as Spam.", "Mark Spam"),
        Mutation::NotSpam if outlook => (
            "Marked as Not Junk.",
            "Marking as Not Junk.",
            "Mark not Junk",
        ),
        Mutation::NotSpam => (
            "Marked as Not Spam.",
            "Marking as Not Spam.",
            "Mark not Spam",
        ),
        Mutation::Mute => ("Muted.", "Muting.", "Mute"),
        Mutation::Unmute => ("Unmuted.", "Unmuting.", "Unmute"),
        _ => return None,
    })
}

/// What a bulk action of `mutation` is called, or `None` for mutations that only run on loaded
/// conversations.
pub(crate) fn bulk_name(mutation: &Mutation, outlook: bool) -> Option<&'static str> {
    if let Some((_, _, name)) = fixed_copy(mutation, outlook) {
        return Some(name);
    }
    match mutation {
        Mutation::Move { .. } if outlook => Some("Move to Folder"),
        Mutation::Move { .. } => Some("Move to Label"),
        Mutation::AddLabel(_) => Some("Add Label"),
        Mutation::RemoveLabel(_) => Some("Remove Label"),
        Mutation::RemoveLabels(_)
        | Mutation::Remind { .. }
        | Mutation::CancelReminder { .. }
        | Mutation::ReturnFromReminder => None,
        _ => None,
    }
}

/// What the toast says after `mutation` on loaded conversations.
pub(crate) fn toast(mutation: &Mutation, labels: &[Label], outlook: bool) -> String {
    if let Some((past, _, _)) = fixed_copy(mutation, outlook) {
        return past.into();
    }
    match mutation {
        Mutation::AddLabel(id) => format!("Added to label {}.", label_name(id, labels, outlook)),
        Mutation::RemoveLabel(id) => {
            format!("Removed from label {}.", label_name(id, labels, outlook))
        }
        Mutation::RemoveLabels(_) => "Removed all labels.".into(),
        Mutation::Move { from, to } => move_toast(from.as_deref(), to, labels, outlook),
        // Remind me's toasts say when, which only the picker knows (`remind::toast`).
        Mutation::CancelReminder { .. } => "Reminder removed.".into(),
        Mutation::Remind { .. } | Mutation::ReturnFromReminder => String::new(),
        _ => String::new(),
    }
}

/// What the toast says when starting a bulk `mutation` from `list`.
pub(crate) fn bulk_toast<'a>(
    mutation: &Mutation,
    list: &ListId,
    labels: &'a [Label],
    splits: impl IntoIterator<Item = &'a SplitInbox>,
    outlook: bool,
) -> String {
    if let Some((_, present, _)) = fixed_copy(mutation, outlook) {
        return present.into();
    }
    match mutation {
        Mutation::AddLabel(id) => format!("Adding to label {}.", label_name(id, labels, outlook)),
        // `Y` names the list it takes threads out of.
        Mutation::RemoveLabel(id) => {
            let name = if removable_label(list).as_ref() == Some(id) {
                crate::lists::name_for(list, labels, splits, outlook)
            } else {
                label_name(id, labels, outlook)
            };
            if name.is_empty() {
                "Removing from label.".into()
            } else {
                format!("Removing from label {name}.")
            }
        }
        Mutation::Move { from, to } => move_text(from.as_deref(), to, labels, true, outlook),
        // Never bulk, because these need loaded conversations.
        Mutation::RemoveLabels(_)
        | Mutation::Remind { .. }
        | Mutation::CancelReminder { .. }
        | Mutation::ReturnFromReminder => String::new(),
        _ => String::new(),
    }
}

/// What the error toast says when `mutation` (or its undo) failed, for those with copy of their
/// own.
pub(crate) fn failure(mutation: &Mutation, undo: bool, outlook: bool) -> Option<&'static str> {
    Some(match (mutation, undo) {
        (Mutation::MarkDone, false) => "Sorry! Failed to mark as Done.",
        (Mutation::MarkNotDone, false) => "Sorry! Failed to mark not Done.",
        (Mutation::Trash, false) => "Sorry! Failed to move to Trash.",
        (Mutation::Untrash, false) => "Sorry! Failed to move to Inbox.",
        (Mutation::Star, false) if outlook => "Sorry! Failed to flag.",
        (Mutation::Star, false) => "Sorry! Failed to star.",
        (Mutation::Unstar, false) if outlook => "Sorry! Failed to unflag.",
        (Mutation::Unstar, false) => "Sorry! Failed to unstar.",
        (Mutation::Star, true) if outlook => "Sorry! Failed to undo flag.",
        (Mutation::Star, true) => "Sorry! Failed to undo star.",
        (Mutation::Unstar, true) if outlook => "Sorry! Failed to undo unflag.",
        (Mutation::Unstar, true) => "Sorry! Failed to undo unstar.",
        (Mutation::Mute, false) => "Sorry! Failed to mute the conversation.",
        (Mutation::Unmute, false) => "Sorry! Failed to Unmute the conversation.",
        (Mutation::Spam | Mutation::NotSpam, false) if outlook => "Sorry! Failed to mark as Junk.",
        (Mutation::Spam | Mutation::NotSpam, false) => "Sorry! Failed to mark as Spam.",
        (Mutation::AddLabel(_), false) => "Sorry! Failed to add label.",
        (Mutation::RemoveLabel(_), false) if outlook => "Sorry! Failed to move to Archive.",
        (Mutation::RemoveLabel(_), false) => "Sorry! Failed to remove from label.",
        (Mutation::AddLabel(_), true) => "Sorry! Failed to undo add label.",
        (Mutation::RemoveLabel(_), true) => "Sorry! Failed to undo remove from label.",
        (Mutation::RemoveLabels(_), false) => "Sorry! Failed to remove labels.",
        (Mutation::RemoveLabels(_), true) => "Sorry! Failed to undo remove labels.",
        (Mutation::Move { .. }, _) => "Sorry! Failed to move conversation.",
        (
            Mutation::MarkRead
            | Mutation::MarkUnread
            | Mutation::Mute
            | Mutation::Unmute
            | Mutation::Spam
            | Mutation::NotSpam,
            true,
        ) => "Sorry! Failed to undo.",
        _ => return None,
    })
}

/// Which triage action button was clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TriageAction {
    Done,
    Remind,
    Command,
}

/// State of the Remind Me button in a triage action cluster.
#[derive(Clone, Copy)]
pub(crate) struct TriageRemind<'a> {
    pub set: bool,
    pub status: Option<&'a str>,
    pub keys: &'a [&'a str],
    pub within: Rangef,
}

/// Descriptor for one button in the `(Done, Remind, Command)` triage cluster.
#[derive(Clone, Copy)]
pub(crate) enum TriageButton<'a> {
    Done { done: bool },
    Remind(TriageRemind<'a>),
    Command,
}

impl TriageButton<'_> {
    /// Paints the button inside `hit` with its glyph at `glyph` (lit when keyboard-focused),
    /// attaches its standard shortcut tooltip, and returns whether it was clicked.
    pub(crate) fn show(self, ui: &Ui, id: egui::Id, hit: Rect, glyph: Rect, lit: bool) -> bool {
        let response = match self {
            Self::Done { done: true } => {
                let response = ui
                    .interact(hit, id.with(DONE_ICON), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                let color = if response.hovered() || lit {
                    Color32::from_rgb(0x5f, 0x74, 0xaa)
                } else {
                    exo_ui::palette(ui.ctx()).actions_done_default
                };
                exo_ui::paint_icon_stroked(ui, DONE_ICON, exo_ui::pixel_snap(glyph), color, 2.0);
                response
            }
            Self::Done { done: false } => {
                IconButton::new(DONE_ICON)
                    .lit(lit)
                    .show(ui, id.with(DONE_ICON), hit, glyph)
            }
            Self::Remind(remind) if remind.set => IconButton::new(REMIND_SET_ICON)
                .lit(lit)
                .colors(
                    Color32::from_rgb(0xe0, 0xbf, 0xe0),
                    Color32::from_rgb(0xcd, 0x7a, 0xcf),
                )
                .show(ui, id.with(REMIND_SET_ICON), hit, glyph),
            Self::Remind(_) => {
                IconButton::new(REMIND_ICON)
                    .lit(lit)
                    .show(ui, id.with(REMIND_ICON), hit, glyph)
            }
            Self::Command => {
                IconButton::new(COMMAND_ICON)
                    .lit(lit)
                    .show(ui, id.with(COMMAND_ICON), hit, glyph)
            }
        };
        let (tooltip, status, within) = match self {
            Self::Done { done: true } => (
                KeyTooltip {
                    text: "Mark not Done",
                    keys: &["shift", "E"],
                },
                None,
                Rangef::EVERYTHING,
            ),
            Self::Done { done: false } => (
                KeyTooltip {
                    text: "Mark Done",
                    keys: &["E"],
                },
                None,
                Rangef::EVERYTHING,
            ),
            Self::Remind(remind) => (
                KeyTooltip {
                    text: "Remind Me",
                    keys: remind.keys,
                },
                remind.status,
                remind.within,
            ),
            Self::Command => (
                KeyTooltip {
                    text: "Exo Command",
                    keys: &["⌘", "K"],
                },
                None,
                Rangef::EVERYTHING,
            ),
        };
        match status {
            _ if lit => {
                tooltip.show_at_focused(ui, &response, TRIAGE_TOOLTIP_PLACEMENT, true, status)
            }
            Some(status) => {
                tooltip.show_at_with_status(ui, &response, TRIAGE_TOOLTIP_PLACEMENT, status, within)
            }
            None => tooltip.show_at(ui, &response, TRIAGE_TOOLTIP_PLACEMENT),
        }
        response.clicked()
    }
}

/// Right-to-left 24px-tall triage action strip shared by inbox rows and the multi-select header.
pub(crate) struct TriageStrip<'a> {
    pub right: f32,
    pub top: f32,
    pub done_width: f32,
    pub done_inset: Vec2,
    pub other_inset: Vec2,
    /// `None` hides Done (for Spam, where conversations cannot be marked Done).
    pub done: Option<bool>,
    /// `None` hides Remind Me (for draft-only conversations).
    pub remind: Option<TriageRemind<'a>>,
}

impl TriageStrip<'_> {
    /// Paints `(Command, Remind, Done)` right-to-left 4px apart and returns the cluster's left
    /// edge alongside any clicked action.
    pub(crate) fn show(self, ui: &Ui, id: egui::Id) -> (f32, Option<TriageAction>) {
        let mut left = self.right;
        let mut clicked = None;
        let items: [(Option<TriageButton<'_>>, TriageAction); 3] = [
            (Some(TriageButton::Command), TriageAction::Command),
            (self.remind.map(TriageButton::Remind), TriageAction::Remind),
            (
                self.done.map(|done| TriageButton::Done { done }),
                TriageAction::Done,
            ),
        ];
        for (button, action) in items {
            let Some(button) = button else { continue };
            let (width, inset, size) = match action {
                TriageAction::Done => (self.done_width, self.done_inset, vec2(16.0, 14.0)),
                TriageAction::Remind | TriageAction::Command => {
                    (24.0, self.other_inset, Vec2::splat(14.0))
                }
            };
            let hit = Rect::from_min_size(pos2(left - width, self.top), vec2(width, 24.0));
            let glyph = Rect::from_min_size(hit.min + inset, size);
            if button.show(ui, id, hit, glyph, false) {
                clicked = Some(action);
            }
            left = hit.left() - 4.0;
        }
        (left + 4.0, clicked)
    }
}

#[cfg(test)]
mod tests {
    use exo_store::LabelKind;

    use super::*;

    fn user_label(id: &str, name: &str) -> Label {
        Label {
            id: id.into(),
            name: name.into(),
            kind: LabelKind::User,
            text_color: None,
            background_color: None,
            hidden: false,
        }
    }

    #[test]
    fn single_and_bulk_triage_copy_stay_aligned() {
        let labels = [user_label("Label_1", "Projects")];
        assert_eq!(
            toast(&Mutation::MarkDone, &labels, false),
            "Marked as Done."
        );
        assert_eq!(
            bulk_toast(&Mutation::MarkDone, &ListId::Inbox, &labels, [], false),
            "Marking as Done."
        );
        assert_eq!(
            toast(&Mutation::AddLabel("Label_1".into()), &labels, false),
            "Added to label Projects."
        );
        assert_eq!(
            bulk_toast(
                &Mutation::AddLabel("Label_1".into()),
                &ListId::Inbox,
                &labels,
                [],
                false
            ),
            "Adding to label Projects."
        );
        assert_eq!(
            bulk_toast(
                &Mutation::RemoveLabel("INBOX".into()),
                &ListId::Done,
                &labels,
                [],
                false
            ),
            "Removing from label Done."
        );
        assert_eq!(
            bulk_name(&Mutation::Untrash, false),
            Some("Remove from Trash")
        );
        assert_eq!(bulk_name(&Mutation::ReturnFromReminder, false), None);
        assert_eq!(
            failure(&Mutation::Unmute, false, false),
            Some("Sorry! Failed to Unmute the conversation.")
        );
    }

    #[test]
    fn outlook_triage_copy_uses_flag_junk_delete_and_folder_terms() {
        let labels = [user_label("Label_1", "Projects")];
        assert_eq!(toast(&Mutation::Star, &labels, true), "Flagged.");
        assert_eq!(toast(&Mutation::Unstar, &labels, true), "Unflagged.");
        assert_eq!(toast(&Mutation::Spam, &labels, true), "Marked as Junk.");
        assert_eq!(
            toast(&Mutation::NotSpam, &labels, true),
            "Marked as Not Junk."
        );
        assert_eq!(
            move_toast(Some("INBOX"), "Label_1", &labels, true),
            "Moved."
        );
        assert_eq!(
            move_toast(Some("Label_1"), "SPAM", &labels, true),
            "Marked as Junk."
        );
        assert_eq!(
            move_toast(Some("SH_ARCHIVED"), "TRASH", &labels, true),
            "Moved to Deleted Items."
        );
        assert_eq!(bulk_name(&Mutation::Star, true), Some("Flag"));
        assert_eq!(bulk_name(&Mutation::Trash, true), Some("Delete"));
        assert_eq!(
            bulk_name(&Mutation::Untrash, true),
            Some("Remove from Deleted Items")
        );
        assert_eq!(
            bulk_name(
                &Mutation::Move {
                    from: None,
                    to: "Label_1".into()
                },
                true
            ),
            Some("Move to Folder")
        );
        assert_eq!(
            failure(&Mutation::Star, false, true),
            Some("Sorry! Failed to flag.")
        );
        assert_eq!(
            failure(&Mutation::Spam, false, true),
            Some("Sorry! Failed to mark as Junk.")
        );
        assert_eq!(
            failure(&Mutation::RemoveLabel("Label_1".into()), false, true),
            Some("Sorry! Failed to move to Archive.")
        );
    }
}
