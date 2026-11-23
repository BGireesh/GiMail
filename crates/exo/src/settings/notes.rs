//! The Notes dialog has two radio buttons, where thread notes show, then a line about the other
//! choice. Choosing one applies it at once, and there's no Save. It's Exo's own.

use egui::{Color32, Context, CursorIcon, Rect, Sense, Ui, pos2, vec2};
use exo_store::NotesPlacement;
use exo_ui::{Dialog, DialogResponse, Family, Font, TextStyle, paint_radio};

use super::Action;

/// The Theme dialog's layout: 12px/20px lines from `padding-top: 20px`, 3px down, and a footer
/// line under `padding: 17px 0 2px`.
const PADDING_TOP: f32 = 20.0;
const NUDGE: f32 = 3.0;
const LINE: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const FOOTER_PADDING: [f32; 2] = [17.0, 2.0];
const BODY_HEIGHT: f32 = PADDING_TOP + 2.0 * 20.0 + FOOTER_PADDING[0] + 20.0 + FOOTER_PADDING[1];
/// The 12px radio, 3px below its line's top and 7px before its label.
const RADIO: f32 = 12.0;
const RADIO_TOP: f32 = 3.0;
const RADIO_GAP: f32 = 7.0;
/// The dialog's card is Snow's in either theme: the labels `rgba(0, 0, 0, .85)` and the footer
/// `rgba(0, 0, 0, .6)`, as the Theme dialog's.
const LABEL_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);
const FOOTER_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 153);
const CHOICES: [(&str, NotesPlacement); 2] = [
    ("In thread", NotesPlacement::Thread),
    ("In pane only", NotesPlacement::Pane),
];
const FOOTER: &str = "In pane only, the thread shows how many notes there are.";

/// Draws the dialog over `area` with `current` checked. A click on the other choice sets it.
pub fn dialog(
    ctx: &Context,
    area: Rect,
    open: bool,
    current: NotesPlacement,
) -> DialogResponse<Option<Action>> {
    Dialog::new("notes", "Notes", BODY_HEIGHT)
        .show(ctx, area, open, |ui, body| picker(ui, body, current))
}

fn picker(ui: &mut Ui, body: Rect, current: NotesPlacement) -> Option<Action> {
    let painter = ui.painter().clone();
    let top = body.top() + NUDGE + PADDING_TOP;
    let mut chosen = None;
    for (i, (label, placement)) in CHOICES.into_iter().enumerate() {
        let line_top = top + i as f32 * LINE.line_height;
        paint_radio(
            &painter,
            pos2(body.left(), line_top + RADIO_TOP),
            placement == current,
        );
        let text_left = body.left() + RADIO + RADIO_GAP;
        LINE.paint_line(&painter, pos2(text_left, line_top), label, LABEL_INK, None);
        // The radio and its label take the click, with a pointer cursor.
        let hit = Rect::from_min_size(
            pos2(body.left(), line_top),
            vec2(
                RADIO + RADIO_GAP + LINE.width(ui.ctx(), label),
                LINE.line_height,
            ),
        );
        let response = ui
            .interact(hit, ui.id().with(("notes-choice", i)), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() && placement != current {
            chosen = Some(Action::SetNotesPlacement(placement));
        }
    }
    let footer_top = top + 2.0 * LINE.line_height + NUDGE + FOOTER_PADDING[0];
    LINE.paint_line(
        &painter,
        pos2(body.left(), footer_top),
        FOOTER,
        FOOTER_INK,
        None,
    );
    chosen
}
