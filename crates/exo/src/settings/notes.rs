//! The Notes dialog has two radio buttons, where thread notes show, then a line about the other
//! choice. Under them, `Keep a Markdown copy` turns the notes folder on or off
//! ([`crate::notes::markdown`]), and the folder's path shows with `Choose…`, which picks another.
//! Every choice applies at once, and there's no Save. It's Exo's own.

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
/// The Markdown copy's checkbox line and its folder's line, a footer's padding below the footer.
const MARKDOWN_GAP: f32 = 17.0;
const BODY_HEIGHT: f32 = PADDING_TOP
    + 2.0 * 20.0
    + FOOTER_PADDING[0]
    + 20.0
    + MARKDOWN_GAP
    + 2.0 * 20.0
    + FOOTER_PADDING[1];
/// The checkbox, 3px below its line's top, as the radios.
const CHECKBOX: f32 = 15.0;
/// The gap between the folder's path and `Choose…`.
const CHOOSE_GAP: f32 = 8.0;
/// `Choose…`, in the dialogs' link blue.
const LINK_INK: Color32 = Color32::from_rgb(0x54, 0xac, 0xdc);
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
const MARKDOWN: &str = "Keep a Markdown copy";
const CHOOSE: &str = "Choose\u{2026}";

/// The notes folder as the dialog shows it: the folder kept, or the one turning the copy on keeps.
pub struct Folder<'a> {
    /// The folder kept, or `None` while no copy is kept.
    pub kept: Option<&'a str>,
    /// The folder turning the copy on keeps.
    pub default: &'a str,
}

/// Draws the dialog over `area` with `current` checked and the notes `folder`. A click on the
/// other choice sets it, the checkbox turns the copy on or off, and `Choose…` picks a folder.
pub fn dialog(
    ctx: &Context,
    area: Rect,
    open: bool,
    current: NotesPlacement,
    folder: &Folder<'_>,
) -> DialogResponse<Option<Action>> {
    Dialog::new("notes", "Notes", BODY_HEIGHT).show(ctx, area, open, |ui, body| {
        let placement = picker(ui, body, current);
        let copy = markdown(ui, body, folder);
        placement.or(copy)
    })
}

/// The Markdown copy's lines: the checkbox, then the folder and `Choose…`.
fn markdown(ui: &mut Ui, body: Rect, folder: &Folder<'_>) -> Option<Action> {
    let painter = ui.painter().clone();
    let top = body.top()
        + NUDGE
        + PADDING_TOP
        + 2.0 * LINE.line_height
        + NUDGE
        + FOOTER_PADDING[0]
        + LINE.line_height
        + MARKDOWN_GAP;
    let mut chosen = None;
    exo_ui::paint_checkbox(
        &painter,
        pos2(body.left(), top + RADIO_TOP),
        folder.kept.is_some(),
        exo_ui::palette(ui.ctx()),
    );
    let text_left = body.left() + CHECKBOX + RADIO_GAP;
    LINE.paint_line(&painter, pos2(text_left, top), MARKDOWN, LABEL_INK, None);
    let hit = Rect::from_min_size(
        pos2(body.left(), top),
        vec2(
            CHECKBOX + RADIO_GAP + LINE.width(ui.ctx(), MARKDOWN),
            LINE.line_height,
        ),
    );
    if ui
        .interact(hit, ui.id().with("notes-markdown"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
    {
        chosen = Some(Action::SetNotesFolder(match folder.kept {
            Some(_) => None,
            None => Some(folder.default.to_owned()),
        }));
    }
    // The folder, its start elided to leave room for `Choose…`.
    let line_top = top + LINE.line_height;
    let choose_width = LINE.width(ui.ctx(), CHOOSE);
    let room = body.width() - (text_left - body.left()) - CHOOSE_GAP - choose_width;
    let path = shown_path(folder.kept.unwrap_or(folder.default));
    let path = elide_start(&path, |text| LINE.width(ui.ctx(), text) <= room);
    let width = LINE.paint_line(&painter, pos2(text_left, line_top), &path, FOOTER_INK, None);
    let choose_left = text_left + width + CHOOSE_GAP;
    LINE.paint_line(
        &painter,
        pos2(choose_left, line_top),
        CHOOSE,
        LINK_INK,
        None,
    );
    let hit = Rect::from_min_size(
        pos2(choose_left, line_top),
        vec2(choose_width, LINE.line_height),
    );
    if ui
        .interact(hit, ui.id().with("notes-choose"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
    {
        chosen = Some(Action::ChooseNotesFolder);
    }
    chosen
}

/// `folder` with the home folder shown as `~`.
fn shown_path(folder: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match folder.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => folder.to_owned(),
    }
}

/// `text`, or as much of its end as `fits` with `…` before it.
fn elide_start(text: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(text) {
        return text.to_owned();
    }
    let mut starts = text.char_indices().map(|(at, _)| at).skip(1);
    starts
        .find_map(|at| {
            let elided = format!("\u{2026}{}", &text[at..]);
            fits(&elided).then_some(elided)
        })
        .unwrap_or_else(|| "\u{2026}".to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_folder_loses_its_start() {
        assert_eq!(elide_start("/a/b", |_| true), "/a/b");
        assert_eq!(
            elide_start("/Users/maya/Documents/Email Notes", |text| text
                .chars()
                .count()
                <= 12),
            "\u{2026}Email Notes"
        );
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(shown_path(&format!("{home}/Notes")), "~/Notes");
        }
        assert_eq!(shown_path("/tmp/Notes"), "/tmp/Notes");
    }
}
