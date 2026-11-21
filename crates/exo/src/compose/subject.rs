//! The `:` emoji and `;` snippet pickers in a new message's subject. The subject looks for the
//! body's triggers in its text up to the caret and shows the body's dropdowns for them. Each
//! dropdown hangs from the subject's line, with its top left corner at the bottom of the line and
//! as far in as the text before the trigger.

use egui::{
    Event, Id, Rect, Ui,
    text::{CCursor, CCursorRange},
    text_edit::TextEditState,
};
use exo_editor::{Pos, Trigger, TriggerKind};
use exo_ui::TextStyle;

use super::draft::Open;

/// The subject's assistant between frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    /// The trigger at the subject's caret. Its anchor is the trigger character's left edge on the
    /// line.
    pub trigger: Option<Trigger>,
    /// A trigger character was typed since a row was last picked (`editing`). The `;` picker
    /// needs it.
    armed: bool,
    /// Escape closed the dropdown, so it stays closed until a trigger character is typed again.
    dismissed: bool,
    /// Where the caret goes once a picked row has replaced the trigger's text.
    caret: Option<usize>,
}

/// Whether this frame's keys type a trigger character. Call it before the field takes the keys.
pub(super) fn typed_trigger(ui: &Ui) -> bool {
    ui.input(|input| input.events.iter().any(|event| matches!(event, Event::Text(text) if text.chars().any(exo_editor::is_trigger_char))))
}

/// Finds the trigger at the subject's caret, after the subject drew with the keyboard. The subject
/// is field `id`, with its text on `line` in `style`, and `typed` says a trigger character was
/// typed this frame.
pub(super) fn update(ui: &Ui, open: &mut Open, id: Id, line: Rect, style: &TextStyle, typed: bool) {
    let ctx = ui.ctx();
    let state = &mut open.subject_assistant;
    if let Some(caret) = state.caret.take() {
        exo_ui::focus_field(ctx, id, CCursorRange::one(CCursor::new(caret)));
    }
    if typed {
        state.armed = true;
        state.dismissed = false;
    }
    let caret = TextEditState::load(ctx, id)
        .and_then(|edit| edit.cursor.char_range())
        .filter(|range| range.is_empty())
        .map(|range| usize::from(range.primary.index));
    let subject = &open.record.subject;
    state.trigger = caret.filter(|_| !state.dismissed).and_then(|caret| {
        let before: String = subject.chars().take(caret).collect();
        let (kind, start, query) = exo_editor::find_trigger(&before)?;
        // Emoji show as typed, but a snippet's `;` has to have been typed. A mention shows
        // nothing. The subject finds `+`, `@` and `-`, but unlike the body it never shows their
        // contacts.
        let shown = match kind {
            TriggerKind::Emoji => true,
            TriggerKind::Snippet => state.armed,
            TriggerKind::Contact(_) => false,
        };
        let x = exo_ui::field_x(ctx, subject, line, style, start);
        shown.then(|| Trigger {
            kind,
            query,
            start: Pos::new(0, start),
            end: Pos::new(0, caret),
            anchor: Rect::from_x_y_ranges(x..=x, line.y_range()),
        })
    });
}

/// Replaces the trigger's text in the subject with `text` and puts the caret after it.
pub(super) fn replace(open: &mut Open, trigger: &Trigger, text: &str) {
    let chars: Vec<char> = open.record.subject.chars().collect();
    let (start, end) = (
        trigger.start.offset.min(chars.len()),
        trigger.end.offset.min(chars.len()),
    );
    open.record.subject = chars[..start]
        .iter()
        .copied()
        .chain(text.chars())
        .chain(chars[end..].iter().copied())
        .collect();
    let state = &mut open.subject_assistant;
    state.caret = Some(start + text.chars().count());
    state.armed = false;
    state.trigger = None;
}

/// Escape on the subject's dropdown closes the dropdown and keeps the text.
pub(super) fn dismiss(open: &mut Open) {
    open.subject_assistant.dismissed = true;
    open.subject_assistant.trigger = None;
}
