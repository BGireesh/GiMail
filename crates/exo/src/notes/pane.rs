//! The right pane's Notes tab, beside its Contact tab while a thread is open.
//!
//! With notes in the thread, the tab is a timeline: one compact entry per note between small
//! markers for the emails, in time order, and clicking a note's entry scrolls the thread to it.
//! With notes in the pane only, it shows the notes as full cards in time order, and the editor
//! opens there. `Share notes by email` sits at the foot of both.

use egui::{CornerRadius, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use exo_store::{Note, NotesPlacement, ThreadDetail};
use exo_ui::{Family, Font, TextStyle};
use jiff::Timestamp;

use super::{
    ADD_HEIGHT, CardAction, EditorAction, LABEL, META, Notes, Target, add_thread_note, date,
    first_name,
};

/// The tab row's height, at the pane's top.
pub const TABS_HEIGHT: f32 = 40.0;
/// The tabs' labels: 13px semibold, 18px apart, with the shown one underlined 4px under its text.
const TAB: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 13.0, 16.0);
const TAB_GAP: f32 = 18.0;
const TAB_TOP: f32 = 14.0;
const UNDERLINE: f32 = 2.0;
/// The right pane's footer, which the pane leaves room for.
const FOOTER_HEIGHT: f32 = 62.0;
/// The space above the list, and between the list and the share button.
const LIST_TOP: f32 = 12.0;
const LIST_GAP: f32 = 14.0;
/// The gap between cards.
const CARD_GAP: f32 = 8.0;
/// The share button: 32px, with its icon 12px and 6px before its label.
const SHARE_HEIGHT: f32 = 32.0;
const SHARE_ICON: f32 = 12.0;
const SHARE_ICON_GAP: f32 = 6.0;
const SHARE: &str = "Share notes by email";
/// The timeline's line is 5px in from the list's left, its dots 8px across on it, and the
/// entries' text 15px past it. Entries are 14px apart.
const LINE_INSET: f32 = 5.0;
const DOT: f32 = 8.0;
const DOT_TOP: f32 = 4.0;
const ENTRY_INSET: f32 = 15.0;
const ENTRY_GAP: f32 = 14.0;
/// An email's envelope, 12px, 5px before its text.
const ENVELOPE: f32 = 12.0;
const ENVELOPE_GAP: f32 = 5.0;
/// A note entry's excerpt: at most two lines.
const EXCERPT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 16.0);
const EXCERPT_LINES: usize = 2;
const ELLIPSIS: &str = "…";

/// One entry in the timeline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    /// A note, by id.
    Note(String),
    /// An email, by message id.
    Email(String),
}

/// What a click in the notes pane asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneAction {
    /// A note's timeline entry: scroll the thread to note `id` and flash it.
    Reveal(String),
    /// An email's timeline entry: scroll the thread to that message.
    RevealMessage(String),
    /// `+ Add thread note`: a new note.
    Start(Target),
    /// A card's link, on note `id`.
    Card(String, CardAction),
    /// The editor's `Save`.
    Save,
    /// `Share notes by email`.
    Share,
    /// A deleted note's `Restore`, from the pane's deleted notes.
    RestoreDeleted(String),
}

/// The thread's notes and emails in time order: each email by its date, each note by when it was
/// written, an email before a note written in the same instant, and notes written in the same
/// instant by number.
pub fn timeline(detail: &ThreadDetail, notes: &[Note]) -> Vec<Item> {
    let mut items: Vec<(Timestamp, u8, Option<u32>, Item)> = detail
        .messages
        .iter()
        .filter(|message| !message.is_draft())
        .map(|message| (message.date, 0, None, Item::Email(message.id.clone())))
        .chain(
            notes
                .iter()
                .map(|note| (note.created_at, 1, note.number, Item::Note(note.id.clone()))),
        )
        .collect();
    items.sort_by_key(|(at, kind, number, _)| (*at, *kind, *number));
    items.into_iter().map(|(_, _, _, item)| item).collect()
}

/// Draws the tab row in `row`: `Contact` and `Notes · {count}`, with the Notes tab shown when
/// `notes_tab`. Returns whether the Notes tab was clicked (`true`) or the Contact tab (`false`).
pub fn tabs(ui: &Ui, row: Rect, rem: f32, notes_tab: bool, count: usize) -> Option<bool> {
    let ctx = ui.ctx();
    let palette = exo_ui::palette(ctx);
    let painter = ui.painter_at(row);
    let notes = format!("Notes · {count}");
    let mut x = row.left() + 2.0 * rem;
    let mut clicked = None;
    for (label, is_notes) in [("Contact", false), (notes.as_str(), true)] {
        let width = TAB.width(ctx, label);
        let text = Rect::from_min_size(pos2(x, row.top() + TAB_TOP), vec2(width, TAB.line_height));
        let response = ui
            .interact(
                text.expand2(vec2(TAB_GAP / 2.0, TAB_TOP)).intersect(row),
                exo_ui::id(ctx, ("notes-pane-tab", is_notes)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let shown = is_notes == notes_tab;
        let ink = if shown || response.hovered() {
            palette.text_base_default
        } else {
            palette.ink(0.45, 0.5)
        };
        TAB.paint_line(&painter, text.min, label, ink, None);
        if shown {
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(text.left(), text.bottom() + 4.0),
                    vec2(width, UNDERLINE),
                ),
                0.0,
                palette.note_bar_default,
            );
        }
        if response.clicked() && !shown {
            clicked = Some(is_notes);
        }
        x = text.right() + TAB_GAP;
    }
    clicked
}

/// Draws the Notes tab in `pane` (the right pane under its tab row) for the open thread
/// `detail`, and returns what was clicked in it.
pub fn show(
    ui: &mut Ui,
    pane: Rect,
    rem: f32,
    notes: &mut Notes,
    detail: Option<&ThreadDetail>,
) -> Option<PaneAction> {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let (left, right) = (pane.left() + 2.0 * rem, pane.right() - 2.0 * rem);
    let bottom = pane.bottom() - FOOTER_HEIGHT;
    let mut action = None;

    // `Share notes by email`, at the foot: it looks disabled until sharing arrives.
    let share = Rect::from_min_max(pos2(left, bottom - SHARE_HEIGHT), pos2(right, bottom));
    let response = ui
        .interact(share, exo_ui::id(&ctx, "notes-share"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if response.hovered() {
        palette.ink(0.08, 0.12)
    } else {
        palette.ink(0.05, 0.08)
    };
    let painter = ui.painter_at(pane);
    painter.rect_filled(exo_ui::pixel_snap(share), CornerRadius::same(5), fill);
    let muted = palette.ink(0.4, 0.4);
    let label = META.width(&ctx, SHARE);
    let start = share.center().x - (SHARE_ICON + SHARE_ICON_GAP + label) / 2.0;
    exo_ui::paint_icon(
        ui,
        "legacy/DEPRECATED_share",
        exo_ui::pixel_snap(Rect::from_min_size(
            pos2(start, share.center().y - SHARE_ICON / 2.0),
            vec2(SHARE_ICON, SHARE_ICON),
        )),
        muted,
    );
    META.paint_line(
        &painter,
        pos2(
            start + SHARE_ICON + SHARE_ICON_GAP,
            share.center().y - META.line_height / 2.0,
        ),
        SHARE,
        muted,
        None,
    );
    if response.clicked() {
        action = Some(PaneAction::Share);
    }

    // What's above the button scrolls, clipped to its box.
    let list = Rect::from_min_max(
        pos2(left, pane.top() + LIST_TOP),
        pos2(right, share.top() - LIST_GAP),
    );
    if list.height() <= 0.0 {
        return action;
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(list).id_salt("notes-pane-list"));
    child.set_clip_rect(list.intersect(ui.clip_rect()));
    let all = notes.all();
    let mut ordered: Vec<&Note> = all.notes.iter().collect();
    ordered.sort_by_key(|note| (note.created_at, note.number));
    let top = list.top() - notes.pane_scroll;
    let height = match notes.placement() {
        NotesPlacement::Thread => {
            let items = detail.map_or_else(Vec::new, |detail| timeline(detail, &all.notes));
            let (height, clicked) = notes.paint_timeline(&child, list, top, detail, &items);
            action = action.or(clicked);
            height
        }
        NotesPlacement::Pane => {
            let (height, clicked) = notes.paint_cards(&mut child, list, top, &ordered);
            action = action.or(clicked);
            height
        }
    };

    // The wheel scrolls the list while the pointer is over it.
    let max = (height - list.height()).max(0.0);
    if ui.rect_contains_pointer(list) {
        let delta = ui.input(|input| input.smooth_scroll_delta.y);
        notes.pane_scroll -= delta;
    }
    notes.pane_scroll = notes.pane_scroll.clamp(0.0, max);
    action
}

impl Notes {
    /// Paints the timeline's `items` from `top` down in `list`, and returns how tall it is and
    /// what was clicked.
    fn paint_timeline(
        &mut self,
        ui: &Ui,
        list: Rect,
        top: f32,
        detail: Option<&ThreadDetail>,
        items: &[Item],
    ) -> (f32, Option<PaneAction>) {
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        let painter = ui.painter().clone();
        let all = self.all();
        let line_x = list.left() + LINE_INSET;
        let text_left = line_x + ENTRY_INSET;
        let text_width = (list.right() - text_left).max(0.0);
        let mut clicked = None;
        let mut y = top;
        self.timeline.clear();
        let mut line = None::<(f32, f32)>;
        for item in items {
            let (title, title_ink, excerpt, dot) = match item {
                Item::Note(id) => {
                    let Some(note) = all.notes.iter().find(|note| note.id == *id) else {
                        continue;
                    };
                    let title = format!("{} · {}", note.label(), date(note.created_at, &self.now));
                    let excerpt = excerpt(&ctx, &note.text, text_width);
                    (
                        title,
                        palette.note_label_default,
                        excerpt,
                        palette.note_bar_default,
                    )
                }
                Item::Email(id) => {
                    let Some(message) =
                        detail.and_then(|detail| detail.messages.iter().find(|m| m.id == *id))
                    else {
                        continue;
                    };
                    let from = message.from.as_ref().map_or("", |from| {
                        first_name(from.name.as_deref().unwrap_or(&from.email))
                    });
                    let title = format!("{from} · {}", date(message.date, &self.now));
                    (
                        title,
                        palette.ink(0.45, 0.5),
                        Vec::new(),
                        palette.ink(0.3, 0.35),
                    )
                }
            };
            let height = LABEL.line_height + excerpt.len() as f32 * EXCERPT.line_height;
            let rect = Rect::from_min_max(pos2(list.left(), y), pos2(list.right(), y + height));
            let visible = rect.intersect(list);
            let response = ui
                .interact(
                    visible,
                    exo_ui::id(&ctx, ("notes-timeline", item)),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            painter.circle_filled(pos2(line_x, y + DOT_TOP + DOT / 2.0), DOT / 2.0, dot);
            line = Some((line.map_or(y, |(first, _)| first), y));
            let mut x = text_left;
            if matches!(item, Item::Email(_)) {
                exo_ui::paint_icon(
                    ui,
                    "legacy/DEPRECATED_email",
                    exo_ui::pixel_snap(Rect::from_min_size(
                        pos2(x, y + (LABEL.line_height - ENVELOPE) / 2.0),
                        vec2(ENVELOPE, ENVELOPE),
                    )),
                    title_ink,
                );
                x += ENVELOPE + ENVELOPE_GAP;
            }
            let style = if matches!(item, Item::Note(_)) {
                LABEL
            } else {
                META
            };
            let ink = if response.hovered() {
                palette.text_base_default
            } else {
                title_ink
            };
            style.paint_line(
                &painter,
                pos2(x, y),
                &title,
                ink,
                Some((list.right() - x).max(0.0)),
            );
            for (k, text) in excerpt.iter().enumerate() {
                EXCERPT.paint_line(
                    &painter,
                    pos2(
                        text_left,
                        y + LABEL.line_height + k as f32 * EXCERPT.line_height,
                    ),
                    text,
                    palette.ink(0.75, 0.8),
                    None,
                );
            }
            if response.clicked() {
                clicked = Some(match item {
                    Item::Note(id) => PaneAction::Reveal(id.clone()),
                    Item::Email(id) => PaneAction::RevealMessage(id.clone()),
                });
            }
            self.timeline.push((item.clone(), rect));
            y += height + ENTRY_GAP;
        }
        // The line runs from the first dot to the last, behind them.
        if let Some((first, last)) = line {
            painter.vline(
                line_x,
                (first + DOT_TOP + DOT / 2.0)..=(last + DOT_TOP + DOT / 2.0),
                egui::Stroke::new(1.0, palette.border_base_default),
            );
        }
        ((y - top - ENTRY_GAP).max(0.0), clicked)
    }

    /// Paints the notes' cards (`ordered`, in time order) from `top` down in `list`, with the
    /// editor where its note is or after the notes, else `+ Add thread note`. Returns how tall
    /// they are and what was clicked.
    fn paint_cards(
        &mut self,
        ui: &mut Ui,
        list: Rect,
        top: f32,
        ordered: &[&Note],
    ) -> (f32, Option<PaneAction>) {
        let width = list.width();
        let edited = self
            .editing
            .as_ref()
            .map(|open| (open.id.clone(), open.note_id.is_some()));
        let mut clicked = None;
        let mut y = top;
        let mut editor_at = None;
        for note in ordered {
            if edited
                .as_ref()
                .is_some_and(|(id, existing)| *existing && *id == note.id)
            {
                let height = self.editor_height(ui, width);
                editor_at = Some((y, height));
                y += height + CARD_GAP;
                continue;
            }
            let height = self.card_height(ui, note, width);
            let rect = Rect::from_min_size(pos2(list.left(), y), vec2(width, height));
            if rect.intersects(list)
                && let Some(card) = self.card(ui, note, rect, list)
            {
                clicked = Some(PaneAction::Card(note.id.clone(), card));
            }
            y += height + CARD_GAP;
        }
        match &edited {
            // A new note's editor, or one whose note has gone, follows the notes.
            Some(_) if editor_at.is_none() => {
                let height = self.editor_height(ui, width);
                editor_at = Some((y, height));
                y += height + CARD_GAP;
            }
            Some(_) => {}
            None => {
                let rect = Rect::from_min_size(pos2(list.left(), y), vec2(width, ADD_HEIGHT));
                if add_thread_note(ui, rect, list) {
                    clicked = Some(PaneAction::Start(Target::Thread));
                }
                y += ADD_HEIGHT + CARD_GAP;
            }
        }
        // The editor is drawn wherever it is, so it keeps the keyboard out of view.
        if let Some((at, height)) = editor_at {
            let rect = Rect::from_min_size(pos2(list.left(), at), vec2(width, height));
            if self.take_reveal() {
                self.reveal_in_pane(at - top, height, list.height());
            }
            if self.editor(ui, rect) == Some(EditorAction::Save) {
                clicked = Some(PaneAction::Save);
            }
        }
        ((y - top - CARD_GAP).max(0.0), clicked)
    }

    /// Scrolls the pane's list so what's `at` px down it and `height` tall shows in a list
    /// `view` tall.
    fn reveal_in_pane(&mut self, at: f32, height: f32, view: f32) {
        if at < self.pane_scroll {
            self.pane_scroll = at;
        } else if at + height > self.pane_scroll + view {
            self.pane_scroll = (at + height - view).min(at);
        }
    }
}

/// A note's text as its timeline entry shows it: at most two lines `width` wide, the second cut
/// with an ellipsis when there's more.
fn excerpt(ctx: &egui::Context, text: &str, width: f32) -> Vec<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return Vec::new();
    }
    let mut lines = EXCERPT.wrap(ctx, &flat, width);
    if lines.len() > EXCERPT_LINES {
        lines.truncate(EXCERPT_LINES);
        let last = &mut lines[EXCERPT_LINES - 1];
        while !last.is_empty() && EXCERPT.width(ctx, &format!("{last}{ELLIPSIS}")) > width {
            last.pop();
        }
        *last = format!("{}{ELLIPSIS}", last.trim_end());
    }
    lines
}
