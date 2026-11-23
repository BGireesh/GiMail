//! The editor a note is written in, where the note is or will be: an amber-edged card with what
//! it's for, the text, and `Save`.

use egui::{CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};
use exo_editor::EditorStyle;

use super::{
    Editing, LABEL, META, Notes, PADDING_X, RADIUS,
    files::{chips, chips_height},
};

/// The padding above the label, and under the text and the foot.
const PADDING_Y: f32 = 8.0;
/// The text is at least this tall.
const MIN_HEIGHT: f32 = 70.0;
const PLACEHOLDER: &str = "Write a note…";
/// The foot's `Save`, and the key beside it.
const SAVE: &str = "Save";
const SAVE_KEY: &str = "⌘↩";
const SAVE_GAP: f32 = 6.0;

/// What a click in the editor's card asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorAction {
    /// `Save`: save the note and close the editor.
    Save,
}

impl Notes {
    /// What the open editor is for: `New thread note`, `New note on Dana's email`,
    /// `Editing Note 2`.
    fn title(&self, open: &Editing) -> String {
        if let Some(note) = open
            .note_id
            .as_ref()
            .and_then(|id| self.notes.notes.iter().find(|note| note.id == *id))
        {
            return format!("Editing {}", note.label());
        }
        match open
            .message_id
            .as_ref()
            .map(|message| self.senders.get(message))
        {
            None => "New thread note".to_owned(),
            Some(Some((_, true))) => "New note on my reply".to_owned(),
            Some(Some((name, false))) => format!("New note on {name}'s email"),
            Some(None) => "New note".to_owned(),
        }
    }

    /// How tall the open editor's card is at `width` (0 with none open).
    pub fn editor_height(&mut self, ui: &Ui, width: f32) -> f32 {
        let Some(open) = &mut self.editing else {
            return 0.0;
        };
        let inner = width - 2.0 * PADDING_X;
        let text = open.editor.height(ui, inner, open.left).max(MIN_HEIGHT);
        PADDING_Y
            + LABEL.line_height
            + PADDING_Y
            + text
            + chips_height(&open.files, inner)
            + PADDING_Y
            + META.line_height
            + PADDING_Y
    }

    /// Draws the open editor's card in `rect` and handles its typing. Returns what was clicked
    /// in it.
    pub fn editor(&mut self, ui: &mut Ui, rect: Rect) -> Option<EditorAction> {
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        let title = self.title(self.editing.as_ref()?);
        let active = self.active;
        self.editor_rect = Some(rect);
        self.painted = true;
        let open = self.editing.as_mut()?;
        let painter = ui.painter().clone();
        let snapped = exo_ui::pixel_snap(rect);
        painter.rect_filled(
            snapped,
            CornerRadius::same(RADIUS),
            palette.note_background_default,
        );
        painter.rect_stroke(
            snapped,
            CornerRadius::same(RADIUS),
            Stroke::new(1.0, palette.note_bar_default),
            StrokeKind::Inside,
        );
        let (left, right) = (rect.left() + PADDING_X, rect.right() - PADDING_X);
        LABEL.paint_line(
            &painter,
            pos2(left, rect.top() + PADDING_Y),
            &title,
            palette.note_label_default,
            Some(right - left),
        );

        // `Save ⌘↩` at the foot's right.
        let foot = rect.bottom() - PADDING_Y - META.line_height;
        let key_width = META.width(&ctx, SAVE_KEY);
        let save_width = LABEL.width(&ctx, SAVE);
        let save = Rect::from_min_size(
            pos2(right - key_width - SAVE_GAP - save_width, foot),
            vec2(save_width + SAVE_GAP + key_width, META.line_height),
        );
        let response = ui
            .interact(
                save.expand(4.0).intersect(ui.clip_rect()),
                exo_ui::id(&ctx, "note-editor-save"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        LABEL.paint_line(&painter, save.min, SAVE, palette.note_label_default, None);
        META.paint_line(
            &painter,
            pos2(save.right() - key_width, foot),
            SAVE_KEY,
            palette.ink(0.4, 0.4),
            None,
        );
        let saved = response.clicked();

        // The listed files' cards over the foot, each with its remove cross.
        let files = Rect::from_min_max(
            pos2(
                left,
                foot - PADDING_Y - chips_height(&open.files, right - left),
            ),
            pos2(right, foot - PADDING_Y),
        );
        let chip = chips(ui, &open.files, files, ui.clip_rect(), true, &open.id);

        let text = Rect::from_min_max(
            pos2(left, rect.top() + PADDING_Y + LABEL.line_height + PADDING_Y),
            pos2(right, files.top()),
        );
        open.left = text.left();
        let mut child = ui.new_child(UiBuilder::new().max_rect(text).id_salt("note-editor"));
        child.set_clip_rect(text.intersect(ui.clip_rect()));
        let edited = open.editor.ui(
            &mut child,
            &EditorStyle {
                placeholder: Some(PLACEHOLDER.to_owned()),
                min_height: text.height(),
            },
        );
        // No trigger opens a dropdown here (snippets and mentions are compose's, and the emoji
        // picker isn't wired to notes yet), so what typing arms is dismissed at once, leaving
        // none for Escape to dismiss before it closes the editor.
        if open.editor.trigger().is_some() {
            open.editor.dismiss_trigger();
        }
        if edited.changed {
            open.changed = Some(ctx.input(|input| input.time));
        }
        if open.changed.is_some() {
            // The draft is kept a second on.
            ctx.request_repaint_after_secs(super::AUTOSAVE_AFTER as f32);
        }
        // A press on the card gives the editor the keyboard, and one anywhere else takes it
        // (the editor's own toolbar floats outside the card, and keeps it).
        let pressed = ctx.input(|input| {
            input
                .pointer
                .primary_pressed()
                .then(|| input.pointer.press_origin())
                .flatten()
        });
        if let Some(at) = pressed {
            open.focused = rect.contains(at) && ui.clip_rect().contains(at);
        }
        if edited.toolbar.is_some() {
            open.focused = true;
        }
        let wanted = active && open.focused;
        let has = ui.memory(|memory| memory.has_focus(edited.response.id));
        if wanted && !has {
            ui.memory_mut(|memory| memory.request_focus(edited.response.id));
        } else if !wanted && has {
            ui.memory_mut(|memory| memory.surrender_focus(edited.response.id));
        }
        if let Some(chip) = chip {
            self.chip(chip);
        }
        saved.then_some(EditorAction::Save)
    }
}
