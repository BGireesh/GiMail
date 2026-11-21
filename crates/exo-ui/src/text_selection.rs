//! Selecting text Exo paints itself (`user-select: text`), such as a thread's subject and its
//! header's addresses. The screen registers its selectable lines every frame in reading order, and
//! a press and drag selects from one place to another across them. The selection is painted under
//! the glyphs (`::selection`, in the text selection color), and its text goes to ⌘C.

use std::sync::Arc;

use egui::{Galley, Id, Painter, Pos2, Rect, Sense, Ui, pos2, vec2};

/// A place in the registered text: a piece, and a character in it.
type Spot = (usize, usize);

/// One registered run of text, as painted.
struct Piece {
    /// Where its first line box's top left is.
    origin: Pos2,
    galley: Arc<Galley>,
    /// Its line boxes' pitch.
    line_height: f32,
    /// What its text is to its screen (the subject, an address), reported with a selection.
    tag: u32,
    /// What joins it to the piece before when both are copied (` ` for a wrapped line, `\n`).
    join: &'static str,
}

/// What a press and release made of the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Released {
    /// The tags of the pieces the selection starts and ends in.
    pub anchor: u32,
    pub focus: u32,
    /// The press and the release were on the same piece (a click there, in the DOM's terms).
    pub on_one_piece: bool,
}

/// Selectable text's state between frames. Call [`Self::begin`] at the start of each frame,
/// [`Self::add`] for each selectable run as it's painted (before its text, so the selection
/// sits under it), then [`Self::interact`].
#[derive(Default)]
pub struct TextSelection {
    pieces: Vec<Piece>,
    anchor: Option<Spot>,
    focus: Option<Spot>,
    /// The piece the button went down on, while it's down.
    pressed: Option<usize>,
}

impl TextSelection {
    /// Forgets last frame's pieces.
    pub fn begin(&mut self) {
        self.pieces.clear();
    }

    /// Registers `galley`, painted with its first line box's top left at `origin` and its lines
    /// `line_height` apart, and paints the selection's part of it with `painter` in `color`.
    #[expect(
        clippy::too_many_arguments,
        reason = "one run of text and how it's painted"
    )]
    pub fn add(
        &mut self,
        painter: &Painter,
        origin: Pos2,
        galley: Arc<Galley>,
        line_height: f32,
        tag: u32,
        join: &'static str,
        color: egui::Color32,
    ) {
        let index = self.pieces.len();
        let piece = Piece {
            origin,
            galley,
            line_height,
            tag,
            join,
        };
        if let Some((from, to)) = self.range_in(index, &piece) {
            for rect in piece.rects(from, to) {
                painter.rect_filled(rect, 0.0, color);
            }
        }
        self.pieces.push(piece);
    }

    /// The selected characters of piece `index`, if any.
    fn range_in(&self, index: usize, piece: &Piece) -> Option<(usize, usize)> {
        let (start, end) = self.ordered()?;
        if index < start.0 || index > end.0 {
            return None;
        }
        let from = if index == start.0 { start.1 } else { 0 };
        let to = if index == end.0 {
            end.1
        } else {
            piece.galley.text().chars().count()
        };
        (from < to).then_some((from, to))
    }

    fn ordered(&self) -> Option<(Spot, Spot)> {
        let (anchor, focus) = (self.anchor?, self.focus?);
        Some(if anchor <= focus {
            (anchor, focus)
        } else {
            (focus, anchor)
        })
    }

    /// Handles the pointer over this frame's pieces. A press on one starts a selection there (and
    /// returns `Some(true)` for the caller to clear any other), a press elsewhere clears it, and a
    /// drag extends it. On release, returns what was selected, if anything.
    pub fn interact(&mut self, ui: &Ui, id: Id) -> (bool, Option<Released>) {
        let (pressed, released, pointer, origin) = ui.input(|input| {
            (
                input.pointer.primary_pressed(),
                input.pointer.primary_released(),
                input.pointer.interact_pos(),
                input.pointer.press_origin(),
            )
        });
        let mut started = false;
        for (index, piece) in self.pieces.iter().enumerate() {
            let response = ui
                .interact(piece.bounds(), id.with(index), Sense::drag())
                .on_hover_cursor(egui::CursorIcon::Text);
            if pressed && response.hovered() {
                started = true;
                self.pressed = Some(index);
            }
        }
        if pressed {
            match (self.pressed.filter(|_| started), origin) {
                (Some(index), Some(at)) => {
                    let spot = (index, self.pieces[index].char_at(at));
                    (self.anchor, self.focus) = (Some(spot), Some(spot));
                }
                _ => {
                    self.clear();
                    self.pressed = None;
                }
            }
        }
        if let (Some(_), Some(at)) = (self.pressed, pointer) {
            self.focus = Some(self.nearest(at));
        }
        let mut done = None;
        if released && let Some(pressed) = self.pressed.take() {
            let on_one_piece = pointer.is_some_and(|at| {
                self.pieces
                    .get(pressed)
                    .is_some_and(|piece| piece.bounds().contains(at))
            });
            if let Some((start, end)) = self.ordered().filter(|(start, end)| start != end) {
                let tag = |spot: Spot| self.pieces.get(spot.0).map_or(0, |piece| piece.tag);
                done = Some(Released {
                    anchor: tag(start),
                    focus: tag(end),
                    on_one_piece,
                });
            }
        }
        (started, done)
    }

    /// The spot nearest `at`, in the piece under it or else the last piece starting above it.
    fn nearest(&self, at: Pos2) -> Spot {
        let index = self
            .pieces
            .iter()
            .position(|piece| piece.bounds().contains(at))
            .or_else(|| self.pieces.iter().rposition(|piece| piece.origin.y <= at.y))
            .unwrap_or(0);
        (
            index,
            self.pieces.get(index).map_or(0, |piece| piece.char_at(at)),
        )
    }

    /// A right-click at `at`, as a Mac takes it: on the selection, the selection stays; on a word
    /// of the text, that word is selected; elsewhere on the text, nothing is. Off the text it
    /// does nothing.
    pub fn select_word_at(&mut self, at: Pos2) {
        let Some(index) = self
            .pieces
            .iter()
            .position(|piece| piece.bounds().contains(at))
        else {
            return;
        };
        let piece = &self.pieces[index];
        let under = piece.char_under(at);
        if self
            .range_in(index, piece)
            .is_some_and(|(from, to)| from <= under && under < to)
        {
            return;
        }
        let chars: Vec<char> = piece.galley.text().chars().collect();
        let is_word = |at: usize| {
            chars
                .get(at)
                .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '\'' | '’'))
        };
        if !is_word(under) {
            return self.clear();
        }
        let start = (0..under)
            .rev()
            .take_while(|at| is_word(*at))
            .last()
            .unwrap_or(under);
        let end = (under..chars.len())
            .take_while(|at| is_word(*at))
            .last()
            .map_or(under, |last| last + 1);
        (self.anchor, self.focus) = (Some((index, start)), Some((index, end)));
    }

    /// Where the selection starts on screen, on its text's baseline, and that text's size, for
    /// Look Up's popover to line up with it.
    pub fn start(&self) -> Option<(Pos2, f32)> {
        let (start, _) = self.ordered()?;
        let piece = self.pieces.get(start.0)?;
        let galley = &piece.galley;
        let mut before = 0;
        let row = galley
            .rows
            .iter()
            .position(|row| {
                before += row.char_count_including_newline().0;
                start.1 < before
            })
            .unwrap_or(galley.rows.len().saturating_sub(1));
        let glyph = galley.rows.first()?.glyphs.first()?;
        // CSS puts the baseline half the leading below the line box's top, then the ascent.
        let baseline = piece.origin.y
            + row as f32 * piece.line_height
            + (piece.line_height + 2.0 * glyph.font_ascent - glyph.font_height) / 2.0;
        let x = piece.origin.x
            + galley
                .pos_from_cursor(egui::text::CCursor::new(start.1))
                .min
                .x;
        let size = galley.job.sections.first()?.format.font_id.size;
        Some((pos2(x, baseline), size))
    }

    /// Clears the selection.
    pub fn clear(&mut self) {
        (self.anchor, self.focus) = (None, None);
    }

    /// The selected text, with the pieces joined as they read, if anything is selected.
    pub fn text(&self) -> Option<String> {
        let mut text = String::new();
        for (index, piece) in self.pieces.iter().enumerate() {
            let Some((from, to)) = self.range_in(index, piece) else {
                continue;
            };
            if !text.is_empty() {
                text.push_str(piece.join);
            }
            text.extend(piece.galley.text().chars().skip(from).take(to - from));
        }
        (!text.is_empty()).then_some(text)
    }
}

impl Piece {
    /// Its line boxes' extent.
    fn bounds(&self) -> Rect {
        let width = self
            .galley
            .rows
            .iter()
            .map(|row| row.pos.x + row.rect().width())
            .fold(0.0, f32::max);
        Rect::from_min_size(
            self.origin,
            vec2(
                width,
                self.galley.rows.len().max(1) as f32 * self.line_height,
            ),
        )
    }

    /// The character the point `at` falls before, by line box and x.
    fn char_at(&self, at: Pos2) -> usize {
        let rows = &self.galley.rows;
        let row = (((at.y - self.origin.y) / self.line_height).floor().max(0.0) as usize)
            .min(rows.len().saturating_sub(1));
        let before: usize = rows[..row]
            .iter()
            .map(|row| row.char_count_including_newline().0)
            .sum();
        before
            + rows.get(row).map_or(0, |placed| {
                placed.char_at(at.x - self.origin.x - placed.pos.x).0
            })
    }

    /// The character under `at`: the one before the boundary [`Self::char_at`] finds when `at` is
    /// left of that boundary.
    fn char_under(&self, at: Pos2) -> usize {
        let index = self.char_at(at);
        let boundary = self.origin.x
            + self
                .galley
                .pos_from_cursor(egui::text::CCursor::new(index))
                .min
                .x;
        if index > 0 && at.x < boundary {
            index - 1
        } else {
            index
        }
    }

    /// The selection's boxes for characters `from..to`, one per line box they're on.
    fn rects(&self, from: usize, to: usize) -> Vec<Rect> {
        let mut rects = Vec::new();
        let mut start = 0;
        for (index, row) in self.galley.rows.iter().enumerate() {
            let count = row.char_count_excluding_newline().0;
            let (a, b) = (from.max(start), to.min(start + count));
            if a < b {
                let x = |column: usize| {
                    self.origin.x + row.pos.x + row.x_offset(egui::text::CharIndex(column - start))
                };
                let top = self.origin.y + index as f32 * self.line_height;
                rects.push(Rect::from_min_max(
                    pos2(x(a), top),
                    pos2(x(b), top + self.line_height),
                ));
            }
            start += row.char_count_including_newline().0;
        }
        rects
    }
}
