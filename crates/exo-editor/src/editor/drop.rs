//! Dragging the selection within the body, as Chromium's `MoveSelectionCommand` moves it, after
//! an undo step. As on a Mac, a move deletes smartly (a space beside the text goes with it)
//! and a selection made by words inserts smartly (spaces put between it and the words it lands
//! next to). ⌥ copies instead.
//!
//! Text dropped from outside the editor (another app, another editor) goes in as a paste of it
//! would, and ends up selected. A selection moved out to a field or another editor in the window
//! leaves this editor as a move's would.

use super::Editor;
use crate::model::{Document, Pos, Selection};

impl Editor {
    /// Drops the selection at `at` (in the document as it is now) in one undo step, moving it
    /// there, or copying it when `copy`. The dropped text is selected after
    /// (`kSelectReplacement`). `smart` is whether the selection was made by words. A drop on the
    /// selection itself does nothing.
    pub(crate) fn drop_selection(&mut self, at: Pos, copy: bool, smart: bool) {
        let (start, end) = (self.selection.start(), self.selection.end());
        if self.selection.is_collapsed() || (start <= at && at <= end) {
            return;
        }
        let mut fragment = self.doc.fragment(start, end);
        // Part of one line goes as text, out of the line's containers.
        if let [block] = fragment.as_mut_slice() {
            block.path.clear();
        }
        let mut fragment = Document::new(fragment);
        super::fix_cursors(&mut fragment);
        self.undo.save(&self.doc, self.selection);
        let (from, to) = if copy {
            (start, end)
        } else {
            self.smart_delete_range(start, end)
        };
        let (dropped, selected_end) = if copy {
            self.place_dropped(at, fragment, smart).0
        } else if at < from {
            let (selected, end) = self.place_dropped(at, fragment, smart);
            let follow = |pos: Pos| follow_insertion(pos, at, end);
            self.doc.delete(follow(from), follow(to));
            selected
        } else {
            let before = self.doc.blocks.len();
            self.doc.delete(from, to);
            let at = follow_deletion(at, from, to, before, self.doc.blocks.len());
            self.place_dropped(at, fragment, smart).0
        };
        self.selection = Selection::new(dropped, selected_end);
        self.changed();
    }

    /// Puts text dropped from outside the editor in at `at`, in one undo step, and selects it. The
    /// `html` goes in cleaned as a paste's is, or else the plain `text` goes in as plain text is
    /// pasted. The source's styles don't come along.
    pub(crate) fn drop_text(&mut self, at: Pos, html: Option<&str>, text: &str) {
        self.collapse_to(self.doc.clamp(at));
        let start = self.caret();
        match html {
            Some(markup) => {
                let fragment = self.pasted_html(markup);
                self.insert_fragment(fragment, false);
            }
            None => self.insert_fragment(super::plain_fragment(text), true),
        }
        self.selection = Selection::new(start, self.caret());
        self.changed();
    }

    /// Takes the selection out, as a move out of the editor does (`DeleteSelectionAfterDragging`,
    /// a smart delete), in one undo step.
    pub(crate) fn delete_dragged(&mut self) {
        let (start, end) = (self.selection.start(), self.selection.end());
        if self.selection.is_collapsed() {
            return;
        }
        let (from, to) = self.smart_delete_range(start, end);
        self.undo.save(&self.doc, self.selection);
        let pos = self.doc.delete(from, to);
        self.collapse_to(pos);
        self.changed();
    }

    /// `start..end` with the space Chromium's smart delete takes with it (`DeleteSelectionCommand`
    /// with `smart_delete_`): none if the range starts or ends with whitespace itself, else the
    /// space before the range, else the one after.
    fn smart_delete_range(&self, start: Pos, end: Pos) -> (Pos, Pos) {
        let char_at = |pos: Pos| self.doc.blocks[pos.block].text().chars().nth(pos.offset);
        let space = |ch: Option<char>| ch.is_some_and(|ch| ch == ' ' || ch == '\u{A0}');
        let first = char_at(start);
        let last = end
            .offset
            .checked_sub(1)
            .and_then(|offset| char_at(Pos::new(end.block, offset)));
        if space(first) || space(last) {
            return (start, end);
        }
        if let Some(before) = start.offset.checked_sub(1)
            && char_at(Pos::new(start.block, before)) == Some(' ')
        {
            return (Pos::new(start.block, before), end);
        }
        if space(char_at(end)) {
            return (start, Pos::new(end.block, end.offset + 1));
        }
        (start, end)
    }

    /// Puts `fragment` at `at`, with smart insertion's spaces when `smart`
    /// (`ReplaceSelectionCommand::kSmartReplace`). Returns where the text starts and ends (not
    /// counting its spaces), and the position after everything it put in.
    fn place_dropped(&mut self, at: Pos, mut fragment: Document, smart: bool) -> ((Pos, Pos), Pos) {
        let mut dropped = at;
        let mut trailing = false;
        if smart
            && let (Some(first), Some(last)) = (fragment.blocks.first(), fragment.blocks.last())
        {
            let text = self.doc.blocks[at.block].text();
            let before = at
                .offset
                .checked_sub(1)
                .and_then(|offset| text.chars().nth(offset));
            let after = text.chars().nth(at.offset);
            let leading = before.is_some_and(|ch| !smart_exempt(ch, true))
                && first
                    .text()
                    .chars()
                    .next()
                    .is_some_and(|ch| !smart_exempt(ch, false));
            trailing = after.is_some_and(|ch| !smart_exempt(ch, false))
                && last
                    .text()
                    .chars()
                    .last()
                    .is_some_and(|ch| !smart_exempt(ch, true));
            if leading {
                self.doc
                    .insert_text(at, " ", self.doc.blocks[at.block].marks_at(at.offset));
                dropped.offset += 1;
            }
            if trailing && let Some(last) = fragment.blocks.last_mut() {
                let marks = last.marks_at(last.len());
                last.insert(last.len(), vec![crate::model::Run::text(" ", marks)]);
            }
        }
        self.collapse_to(dropped);
        self.place_fragment(fragment);
        // The space put after the dropped text isn't part of it.
        let end = self.caret();
        (
            (
                dropped,
                if trailing {
                    Pos::new(end.block, end.offset - 1)
                } else {
                    end
                },
            ),
            end,
        )
    }
}

/// Where `pos`, which isn't in `start..end`, ends up once that range is deleted. The document had
/// `blocks` blocks before the deletion and has `after` now.
fn follow_deletion(pos: Pos, start: Pos, end: Pos, blocks: usize, after: usize) -> Pos {
    if pos <= start {
        pos
    } else if pos.block == end.block {
        Pos::new(start.block, start.offset + (pos.offset - end.offset))
    } else {
        Pos::new(pos.block + after - blocks, pos.offset)
    }
}

/// Where `pos`, which is after `at`, ends up once text put in at `at` ends at `end`. The rest of
/// `at`'s line follows the text, and the lines after move down by the lines the text added.
fn follow_insertion(pos: Pos, at: Pos, end: Pos) -> Pos {
    if pos.block == at.block {
        Pos::new(end.block, end.offset + (pos.offset - at.offset))
    } else {
        Pos::new(pos.block + (end.block - at.block), pos.offset)
    }
}

/// Characters smart insertion puts no space beside (`IsCharacterSmartReplaceExemptConservative`):
/// whitespace; after the text, any punctuation; and these before or after it.
fn smart_exempt(ch: char, previous: bool) -> bool {
    if ch.is_whitespace() || (!previous && ch.is_ascii_punctuation()) {
        return true;
    }
    let set = if previous {
        "([\"'#$/-`{"
    } else {
        ")].,;:?'!\"%*-/}"
    };
    set.contains(ch)
}
