//! A note's history, drawn in place of its card: its versions newest first, each with when it was
//! saved and what made it, the selected one's text read-only under them, then
//! `Restore this version` (not on the newest) and `Close`.
//!
//! One history or one editor is open at a time. While the history has the keyboard, `↑` and `↓`
//! select a version and `Esc` closes it.

use egui::{CornerRadius, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use exo_editor::{Editor, EditorStyle};
use exo_store::{Note, NoteChange, NoteVersion};
use jiff::Zoned;

use super::{
    CardAction, Control, LABEL, META, Notes, PADDING_X, RADIUS,
    card::{BAR, LABEL_GAP, LINK_GAP, PADDING_Y},
    date,
    files::{chips, chips_height},
    note_editor,
};

/// A version's row: its text 4px in from the row's top and 8px from its left.
const ROW_HEIGHT: f32 = 24.0;
const ROW_INSET_X: f32 = 8.0;
const ROW_INSET_Y: f32 = 4.0;
const ROW_RADIUS: u8 = 4;
/// The gap between the list and the text, and between the text and the buttons.
const GAP: f32 = 10.0;
/// The buttons at the foot.
pub const RESTORE: &str = "Restore this version";
pub const CLOSE: &str = "Close";

/// The open history: the note's versions and the one selected.
pub struct History {
    /// The note's id.
    pub(super) id: String,
    /// Its versions, newest first.
    versions: Vec<NoteVersion>,
    /// The selected version, an index into `versions`.
    selected: usize,
    /// The selected version's text, read-only.
    pub(super) editor: Editor,
    /// The text's left edge on screen when last drawn.
    left: f32,
    /// Whether the history has the keyboard.
    pub(super) focused: bool,
}

impl History {
    /// Note `id`'s history over `versions` (newest first, at least one), the newest selected.
    pub(super) fn new(id: String, versions: Vec<NoteVersion>) -> Self {
        let editor = note_editor(versions.first().map_or("", |version| version.html.as_str()));
        Self {
            id,
            versions,
            selected: 0,
            editor,
            left: 0.0,
            focused: true,
        }
    }

    /// Each version's number and what made it, newest first.
    pub(super) fn list(&self) -> Vec<(u32, NoteChange)> {
        self.versions
            .iter()
            .map(|version| (version.version, version.change))
            .collect()
    }

    /// The selected version.
    pub(super) fn selected(&self) -> Option<&NoteVersion> {
        self.versions.get(self.selected)
    }

    /// The selected version's files, whose images its text shows.
    pub(super) fn files(&self) -> &[exo_store::NoteFile] {
        self.selected()
            .map_or(&[], |version| version.files.as_slice())
    }

    /// Selects version `index` (of `versions`, newest first), if there's one.
    fn select(&mut self, index: usize) {
        if index == self.selected || index >= self.versions.len() {
            return;
        }
        self.selected = index;
        self.editor = note_editor(&self.versions[index].html);
    }

    /// Selects the version `delta` rows down the list (`-1` a newer one), stopping at its ends.
    pub(super) fn step(&mut self, delta: isize) {
        let last = self.versions.len().saturating_sub(1);
        self.select(self.selected.saturating_add_signed(delta).min(last));
    }
}

/// What a version's row says: `23 Nov 3:05 PM · Edited`, told in `now`'s zone.
fn row_text(version: &NoteVersion, now: &Zoned) -> String {
    let time = version
        .saved_at
        .to_zoned(now.time_zone().clone())
        .strftime("%-I:%M %p")
        .to_string();
    let change = match version.change {
        NoteChange::Created => "Created",
        NoteChange::Edited => "Edited",
        NoteChange::Deleted => "Deleted",
        NoteChange::Restored => "Restored",
    };
    format!("{} {time} · {change}", date(version.saved_at, now))
}

impl Notes {
    /// The open history, when it's note `id`'s.
    pub(super) fn history_of(&self, id: &str) -> Option<&History> {
        self.history.as_ref().filter(|history| history.id == id)
    }

    /// How tall the open history is at `width`, in place of its note's card.
    pub(super) fn history_height(&mut self, ui: &Ui, width: f32) -> f32 {
        let Some(history) = &mut self.history else {
            return 0.0;
        };
        let inner = width - BAR - 2.0 * PADDING_X;
        let text = history.editor.height(ui, inner, history.left);
        let files = chips_height(history.files(), inner);
        PADDING_Y
            + LABEL.line_height
            + LABEL_GAP
            + history.versions.len() as f32 * ROW_HEIGHT
            + GAP
            + text
            + files
            + GAP
            + META.line_height
            + PADDING_Y
    }

    /// Draws the open history in `rect`, in place of `note`'s card, of which `clip` shows. A
    /// row clicked is selected. Returns the button clicked: `Restore this version` or `Close`.
    pub(super) fn history_card(
        &mut self,
        ui: &mut Ui,
        note: &Note,
        rect: Rect,
        clip: Rect,
    ) -> Option<CardAction> {
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        let now = self.now.clone();
        self.painted = true;
        match self.rects.iter_mut().find(|(id, _)| *id == note.id) {
            Some((_, painted)) => *painted = rect,
            None => self.rects.push((note.id.clone(), rect)),
        }
        let painter = ui.painter().clone();
        let snapped = exo_ui::pixel_snap(rect);
        painter.rect_filled(
            snapped,
            CornerRadius::same(RADIUS),
            palette.note_background_default,
        );
        painter.rect_filled(
            Rect::from_min_max(snapped.min, pos2(snapped.left() + BAR, snapped.bottom())),
            CornerRadius {
                nw: RADIUS,
                sw: RADIUS,
                ne: 0,
                se: 0,
            },
            palette.note_bar_default,
        );
        let (left, right) = (rect.left() + BAR + PADDING_X, rect.right() - PADDING_X);
        let mut y = rect.top() + PADDING_Y;
        LABEL.paint_line(
            &painter,
            pos2(left, y),
            &format!("{} · History", note.label()),
            palette.note_label_default,
            Some((right - left).max(0.0)),
        );
        y += LABEL.line_height + LABEL_GAP;
        let muted = palette.ink(0.4, 0.4);
        let mut clicked = None;
        let mut controls = Vec::new();

        // The versions, newest first, the selected one marked as a list marks its selection.
        let history = self.history.as_mut()?;
        let mut chosen = None;
        for (index, version) in history.versions.iter().enumerate() {
            let row = Rect::from_min_max(pos2(left, y), pos2(right, y + ROW_HEIGHT));
            let response = ui
                .interact(
                    row.intersect(clip),
                    exo_ui::id(&ctx, ("note-history-row", &note.id, version.version)),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let selected = index == history.selected;
            let ink = if selected {
                painter.rect_filled(
                    exo_ui::pixel_snap(row),
                    CornerRadius::same(ROW_RADIUS),
                    palette.select_fill_default,
                );
                palette.select_content_default
            } else if response.hovered() {
                palette.ink(0.8, 0.8)
            } else {
                palette.ink(0.6, 0.65)
            };
            META.paint_line(
                &painter,
                pos2(row.left() + ROW_INSET_X, row.top() + ROW_INSET_Y),
                &row_text(version, &now),
                ink,
                Some((row.width() - 2.0 * ROW_INSET_X).max(0.0)),
            );
            if response.clicked() {
                chosen = Some(index);
            }
            y += ROW_HEIGHT;
        }
        if let Some(index) = chosen {
            history.select(index);
        }
        y += GAP;

        // `Close`, and `Restore this version` before it on any but the newest, at the foot's right.
        let foot = rect.bottom() - PADDING_Y - META.line_height;
        let mut x = right;
        let buttons: &[(&str, Control)] = if history.selected == 0 {
            &[(CLOSE, Control::CloseHistory)]
        } else {
            &[
                (CLOSE, Control::CloseHistory),
                (RESTORE, Control::RestoreVersion),
            ]
        };
        for (text, control) in buttons {
            let style = if *control == Control::RestoreVersion {
                LABEL
            } else {
                META
            };
            let width = style.width(&ctx, text);
            let button = Rect::from_min_size(pos2(x - width, foot), vec2(width, META.line_height));
            let response = ui
                .interact(
                    button.expand(4.0).intersect(clip),
                    exo_ui::id(&ctx, ("note-history-button", &note.id, *text)),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let ink = match control {
                Control::RestoreVersion => palette.note_label_default,
                _ if response.hovered() => palette.ink(0.8, 0.8),
                _ => muted,
            };
            style.paint_line(&painter, button.min, text, ink, None);
            if response.clicked() {
                clicked = Some(match control {
                    Control::RestoreVersion => {
                        CardAction::Restore(history.selected().map_or(0, |version| version.version))
                    }
                    _ => CardAction::CloseHistory,
                });
            }
            controls.push((control.clone(), button));
            x = button.left() - 2.0 * LINK_GAP;
        }

        // The selected version's files, over the buttons; a click opens one.
        let files = Rect::from_min_max(
            pos2(
                left,
                foot - GAP - chips_height(history.files(), right - left),
            ),
            pos2(right, foot - GAP),
        );
        let chip = chips(ui, history.files(), files, clip, false, &history.id);

        // Its text, which takes no clicks and no keys.
        let text = Rect::from_min_max(pos2(left, y), pos2(right, files.top()));
        history.left = text.left();
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(text)
                .id_salt(("note-history-body", &note.id)),
        );
        child.set_clip_rect(text.intersect(clip).intersect(ui.clip_rect()));
        let shown = history.editor.ui(&mut child, &EditorStyle::default());
        ui.interact(
            text.intersect(clip),
            exo_ui::id(&ctx, ("note-history-body-off", &note.id)),
            Sense::click_and_drag(),
        );
        ui.memory_mut(|memory| memory.surrender_focus(shown.response.id));

        // A press on it gives it the keyboard, and one anywhere else takes it.
        let pressed = ctx.input(|input| {
            input
                .pointer
                .primary_pressed()
                .then(|| input.pointer.press_origin())
                .flatten()
        });
        if let Some(at) = pressed {
            history.focused = rect.contains(at) && clip.contains(at);
        }
        self.controls.extend(controls);
        if let Some(chip) = chip {
            self.chip(chip);
        }
        clicked
    }
}
