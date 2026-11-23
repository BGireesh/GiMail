//! A note as the thread shows it: an amber card with a bar on its left, the note's label and
//! what it's on, and its text, read-only. Pointing at it shows its `Edit` and `Delete`.

use egui::{CornerRadius, Rect, Sense, Shape, Stroke, Ui, UiBuilder, pos2, vec2};
use exo_editor::EditorStyle;
use exo_store::Note;
use exo_ui::{Family, Font, TextStyle};

use super::{
    Body, LABEL, META, Notes, PADDING_X, RADIUS, date,
    files::{chips, chips_height},
    marks, note_editor,
};

/// The bar on a card's left.
const BAR: f32 = 3.0;
/// The padding above the label and under the text.
const PADDING_Y: f32 = 9.0;
/// The gap between the label's line and the text.
const LABEL_GAP: f32 = 4.0;
/// The gap between the links on the label's line, and between them and what's before.
const LINK_GAP: f32 = 12.0;
/// The `+ Add thread note` row's height.
pub const ADD_HEIGHT: f32 = 32.0;
const ADD: &str = "+ Add thread note";

/// What a click on a card asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAction {
    /// Open the note in the editor.
    Edit,
    /// Delete the note.
    Delete,
    /// Show the note's versions.
    History,
}

impl Notes {
    /// The read-only text of `note`, as its current version has it.
    fn body(&mut self, note: &Note) -> &mut Body {
        let body = self.bodies.entry(note.id.clone()).or_insert_with(|| Body {
            version: note.version,
            editor: note_editor(&note.html),
            left: 0.0,
            marked: (Vec::new(), egui::Color32::TRANSPARENT),
        });
        if body.version != note.version {
            body.version = note.version;
            body.editor = note_editor(&note.html);
            body.marked = (Vec::new(), egui::Color32::TRANSPARENT);
        }
        body
    }

    /// How tall `note`'s card is at `width`: its text, and the cards of its listed files.
    pub fn card_height(&mut self, ui: &Ui, note: &Note, width: f32) -> f32 {
        let inner = width - BAR - 2.0 * PADDING_X;
        let body = self.body(note);
        let text = body.editor.height(ui, inner, body.left);
        PADDING_Y
            + LABEL.line_height
            + LABEL_GAP
            + text
            + chips_height(&note.files, inner)
            + PADDING_Y
    }

    /// Draws `note`'s card in `rect`, of which `clip` shows, and returns what was clicked on it.
    pub fn card(&mut self, ui: &mut Ui, note: &Note, rect: Rect, clip: Rect) -> Option<CardAction> {
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        match self.rects.iter_mut().find(|(id, _)| *id == note.id) {
            Some((_, painted)) => *painted = rect,
            None => self.rects.push((note.id.clone(), rect)),
        }
        let painter = ui.painter().clone();
        let snapped = exo_ui::pixel_snap(rect);
        // A note the thread just scrolled to fades from the flash to its own background.
        let background = match self.flash_progress(&note.id) {
            Some(progress) => {
                ctx.request_repaint();
                exo_ui::motion::mix(
                    palette.note_flash_default,
                    palette.note_background_default,
                    progress,
                )
            }
            None => palette.note_background_default,
        };
        painter.rect_filled(snapped, CornerRadius::same(RADIUS), background);
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
        let line = rect.top() + PADDING_Y;
        let muted = palette.ink(0.4, 0.4);

        // `Edit` and `Delete` at the right of the label's line, while the pointer is on the card.
        let mut clicked = None;
        let mut meta_right = right;
        if ui.rect_contains_pointer(rect.intersect(clip)) {
            for (text, action) in [("Delete", CardAction::Delete), ("Edit", CardAction::Edit)] {
                let width = META.width(&ctx, text);
                let link = Rect::from_min_size(
                    pos2(meta_right - width, line),
                    vec2(width, META.line_height),
                );
                let response = ui
                    .interact(
                        link.expand2(vec2(LINK_GAP / 2.0, 4.0)).intersect(clip),
                        exo_ui::id(&ctx, ("note-link", &note.id, text)),
                        Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                let ink = if response.hovered() {
                    palette.ink(0.8, 0.8)
                } else {
                    muted
                };
                META.paint_line(&painter, link.min, text, ink, None);
                if response.clicked() {
                    clicked = Some(action);
                }
                meta_right = link.left() - LINK_GAP;
            }
        }

        let label = LABEL.paint_line(
            &painter,
            pos2(left, line),
            &note.label(),
            palette.note_label_default,
            Some((meta_right - left).max(0.0)),
        );
        let meta = format!(
            " · {} · {}",
            self.where_text(note),
            date(note.updated_at, &self.now)
        );
        META.paint_line(
            &painter,
            pos2(left + label, line),
            &meta,
            muted,
            Some((meta_right - left - label).max(0.0)),
        );

        // The listed files' cards at the foot: a click opens one.
        let files = Rect::from_min_max(
            pos2(
                left,
                rect.bottom() - PADDING_Y - chips_height(&note.files, right - left),
            ),
            pos2(right, rect.bottom() - PADDING_Y),
        );
        if let Some(chip) = chips(ui, &note.files, files, clip, false, &note.id) {
            self.chip(chip);
        }

        // The text, which takes no clicks and no keys.
        let text = Rect::from_min_max(
            pos2(left, line + LABEL.line_height + LABEL_GAP),
            pos2(right, files.top()),
        );
        // A search's words, marked where the text has them.
        let terms = self.highlight.clone();
        let body = self.body(note);
        body.left = text.left();
        let color = palette.note_mark_default;
        if body.marked.0 != terms || body.marked.1 != color {
            let found = marks(body.editor.document(), &terms);
            body.editor.set_marks(found, color);
            body.marked = (terms, color);
        }
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(text)
                .id_salt(("note-body", &note.id)),
        );
        child.set_clip_rect(text.intersect(clip).intersect(ui.clip_rect()));
        let shown = body.editor.ui(&mut child, &EditorStyle::default());
        ui.interact(
            text.intersect(clip),
            exo_ui::id(&ctx, ("note-body-off", &note.id)),
            Sense::click_and_drag(),
        );
        ui.memory_mut(|memory| memory.surrender_focus(shown.response.id));
        clicked
    }
}

/// The `+ Add thread note` row in `rect`, of which `clip` shows. Returns whether it was clicked.
pub fn add_thread_note(ui: &Ui, rect: Rect, clip: Rect) -> bool {
    let ctx = ui.ctx();
    let palette = exo_ui::palette(ctx);
    let response = ui
        .interact(
            rect.intersect(clip),
            exo_ui::id(ctx, "add-thread-note"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let (ink, border) = if response.hovered() {
        (palette.ink(0.8, 0.8), palette.note_bar_default)
    } else {
        (palette.ink(0.4, 0.4), palette.ink(0.2, 0.2))
    };
    // A `1px dashed` border: Chromium's 3px dashes and gaps.
    let outline = exo_ui::pixel_snap(rect).shrink(0.5);
    let corners = [
        outline.left_top(),
        outline.right_top(),
        outline.right_bottom(),
        outline.left_bottom(),
        outline.left_top(),
    ];
    ui.painter().extend(Shape::dashed_line(
        &corners,
        Stroke::new(1.0, border),
        3.0,
        3.0,
    ));
    META.paint_line(
        ui.painter(),
        pos2(
            rect.left() + PADDING_X,
            rect.center().y - META.line_height / 2.0,
        ),
        ADD,
        ink,
        None,
    );
    response.clicked()
}

/// The count chip a thread shows on its subject and on a message's sender while its notes are in
/// the pane only: `1 note`, `3 notes`, 11px in a 16px pill with 8px at its sides.
const CHIP: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 11.0, 16.0);
const CHIP_PADDING: f32 = 8.0;
const CHIP_RADIUS: u8 = 8;

/// The count chip's text for `count` notes.
fn chip_text(count: usize) -> String {
    if count == 1 {
        "1 note".to_owned()
    } else {
        format!("{count} notes")
    }
}

/// How wide the count chip for `count` notes is.
pub fn count_chip_width(ctx: &egui::Context, count: usize) -> f32 {
    CHIP.width(ctx, &chip_text(count)) + 2.0 * CHIP_PADDING
}

/// Draws the count chip for `count` notes with its left edge at `left`, centred on `middle`, and
/// returns whether it was clicked. `id` (the message's, or the thread's for its subject's) tells
/// it apart from the thread's other chips.
pub fn count_chip(ui: &Ui, left: f32, middle: f32, count: usize, id: &str) -> bool {
    let ctx = ui.ctx();
    let palette = exo_ui::palette(ctx);
    let text = chip_text(count);
    let rect = Rect::from_min_size(
        pos2(left, middle - CHIP.line_height / 2.0),
        vec2(count_chip_width(ctx, count), CHIP.line_height),
    );
    let response = ui
        .interact(
            rect.intersect(ui.clip_rect()),
            exo_ui::id(ctx, ("notes-count-chip", id)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let snapped = exo_ui::pixel_snap(rect);
    ui.painter().rect_filled(
        snapped,
        CornerRadius::same(CHIP_RADIUS),
        palette.note_background_default,
    );
    if response.hovered() {
        ui.painter().rect_stroke(
            snapped,
            CornerRadius::same(CHIP_RADIUS),
            Stroke::new(1.0, palette.note_bar_default),
            egui::StrokeKind::Inside,
        );
    }
    CHIP.paint_line(
        ui.painter(),
        pos2(rect.left() + CHIP_PADDING, rect.top()),
        &text,
        palette.note_label_default,
        None,
    );
    response.clicked()
}
