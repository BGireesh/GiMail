//! Thread notes in the app: the open thread's notes as the thread view draws them
//! ([`Notes::card`]), and the editor a note is written in, in place ([`Notes::editor`]).
//!
//! Notes are the user's own. They're kept in the store and never reach Gmail, a reply, a forward
//! or a printout.
//!
//! One editor is open at a time. What's typed in it is kept as a draft a second after it last
//! changed, so a crash loses nothing, and closing the editor saves the note
//! ([`Notes::close`]).

mod card;
mod editor;
pub mod pane;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use egui::Rect;
use exo_commands::Region;
use exo_editor::{BodyText, Editor, FormatCommand, Pos, Selection};
use exo_store::{Note, NoteDraft, NoteFile, NotesPlacement, SaveNote, ThreadId, ThreadNotes};
use exo_ui::{Family, Font, TextStyle};
use jiff::{Timestamp, Zoned};

pub use self::{
    card::{ADD_HEIGHT, CardAction, add_thread_note, count_chip, count_chip_width},
    editor::EditorAction,
    pane::{Item, PaneAction},
};
use crate::account::Account;

/// How long after the last change the editor's content is kept as a draft.
const AUTOSAVE_AFTER: f64 = 1.0;
/// How long a note the thread scrolled to flashes.
const FLASH_FOR: f64 = 1.0;

/// A note's text, read or being written: 14px in 20px lines, with the content area (17px)
/// centred and the odd pixel of leading below.
const BODY: BodyText = BodyText {
    style: TextStyle::new(Font::new(Family::SansEmail, 400), 14.0, 20.0),
    content_top: 1.0,
    content_height: 17.0,
    placeholder_opacity: 0.45,
};
/// A card's or the editor's first line: the label in bold, then the rest.
const LABEL: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 13.0, 16.0);
const META: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 16.0);
/// The corners of a card, the editor and the add row: a message card's.
const RADIUS: u8 = 4;
/// The padding at a card's and the editor's sides.
const PADDING_X: f32 = 14.0;

/// What a new editor is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A new note on the thread.
    Thread,
    /// A new note on this message.
    Message(String),
    /// This note, to edit.
    Existing(String),
}

/// The open editor and the note it writes.
pub struct Editing {
    /// The editor's id: the note's, or a new UUID that a new note takes as its own.
    pub id: String,
    /// The note it edits, or `None` for a new one.
    pub note_id: Option<String>,
    /// The thread the note is on.
    pub thread_id: ThreadId,
    /// The message the note is on, or `None` for a thread note.
    pub message_id: Option<String>,
    /// The note's files.
    pub files: Vec<NoteFile>,
    editor: Editor,
    /// The HTML the note has saved (a new note's is the empty editor's), which closing compares
    /// with.
    saved: String,
    /// Whether the editor has the keyboard.
    focused: bool,
    /// When the content last changed without being kept since.
    changed: Option<f64>,
    /// Whether a draft of it is stored.
    drafted: bool,
    /// Whether saving it has failed and said so.
    failed: bool,
    /// The editor's left edge on screen when last drawn, which its layout is kept for.
    left: f32,
}

impl Editing {
    /// The editor.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// What's typed, as plain text.
    pub fn text(&self) -> String {
        let document = self.editor.document();
        document.text(Pos::default(), document.end())
    }

    /// Whether what's typed differs from what the note has saved.
    fn differs(&self) -> bool {
        self.editor.html() != self.saved
    }

    /// What's typed, as the draft that keeps it.
    fn draft(&self, now: Timestamp) -> NoteDraft {
        NoteDraft {
            id: self.id.clone(),
            thread_id: self.thread_id.clone(),
            message_id: self.message_id.clone(),
            note_id: self.note_id.clone(),
            html: self.editor.html(),
            text: self.text(),
            files: self.files.clone(),
            updated_at: now,
        }
    }
}

/// A note's read-only text, laid out by an editor nothing is typed in.
struct Body {
    /// The note's version it shows.
    version: u32,
    editor: Editor,
    /// Its left edge on screen when last drawn.
    left: f32,
}

/// The open thread's notes and the editor one is written in.
pub struct Notes {
    /// The open thread.
    thread: Option<ThreadId>,
    /// The thread's notes, as the latest snapshot has them.
    notes: Arc<ThreadNotes>,
    placement: NotesPlacement,
    editing: Option<Editing>,
    /// The notes' read-only text, by note.
    bodies: HashMap<String, Body>,
    /// Where each note's card was last painted.
    rects: Vec<(String, Rect)>,
    /// The editors closed here, whose drafts a snapshot may still hold and don't open again.
    closed: HashSet<String>,
    /// The thread's messages: who each is from (a first name), and whether that's the user.
    senders: HashMap<String, (String, bool)>,
    /// The clock's time, whose zone dates are told in.
    now: Zoned,
    /// Whether the editor may hold the keyboard: nothing over the thread has taken it.
    active: bool,
    /// The editor just opened, and the thread should scroll to it.
    reveal: bool,
    /// Whether the open thread's drafts have been looked through for one to open again.
    recovered: bool,
    /// The notes pane's timeline entries where they were last painted.
    timeline: Vec<(Item, Rect)>,
    /// How far the notes pane's list is scrolled.
    pane_scroll: f32,
    /// The note the thread last scrolled to, and when, which flashes for [`FLASH_FOR`].
    flash: Option<(String, f64)>,
    /// The frame's time.
    time: f64,
    /// Where the editor was last painted.
    editor_rect: Option<Rect>,
}

impl Notes {
    /// No thread's notes, dated in `now`'s zone.
    pub fn new(now: Zoned) -> Self {
        Self {
            thread: None,
            notes: Arc::default(),
            placement: NotesPlacement::default(),
            editing: None,
            bodies: HashMap::new(),
            rects: Vec::new(),
            closed: HashSet::new(),
            senders: HashMap::new(),
            now,
            active: false,
            reveal: false,
            recovered: false,
            timeline: Vec::new(),
            pane_scroll: 0.0,
            flash: None,
            time: 0.0,
            editor_rect: None,
        }
    }

    /// Sets the clock's time.
    pub fn set_now(&mut self, now: Zoned) {
        self.now = now;
    }

    /// Thread `thread` opened, or with `None` the open one closed. Close the editor first
    /// ([`Self::close`]).
    pub fn open(&mut self, thread: Option<&str>) {
        self.thread = thread.map(ThreadId::new);
        self.notes = Arc::default();
        self.bodies.clear();
        self.rects.clear();
        self.senders.clear();
        self.recovered = false;
        self.timeline.clear();
        self.pane_scroll = 0.0;
        self.flash = None;
    }

    /// Takes the open thread's notes from a snapshot of it, and where they show. The first
    /// time since the thread opened, a draft that no editor is open for opens its editor
    /// again, without the keyboard: it was being typed when Exo last quit.
    pub fn set(&mut self, notes: Arc<ThreadNotes>, placement: NotesPlacement) {
        if placement != self.placement {
            self.placement = placement;
            self.rects.clear();
        }
        if !Arc::ptr_eq(&self.notes, &notes) {
            self.bodies
                .retain(|id, _| notes.notes.iter().any(|note| note.id == *id));
            self.rects
                .retain(|(id, _)| notes.notes.iter().any(|note| note.id == *id));
            self.notes = notes;
        }
        // Only what the thread opened with: a draft that turns up later is another window's.
        if std::mem::replace(&mut self.recovered, true) || self.editing.is_some() {
            return;
        }
        let Some(draft) = self.notes.drafts.iter().rev().find(|draft| {
            Some(&draft.thread_id) == self.thread.as_ref() && !self.closed.contains(&draft.id)
        }) else {
            return;
        };
        let saved = match &draft.note_id {
            Some(id) => match self.notes.notes.iter().find(|note| note.id == *id) {
                Some(note) => note_editor(&note.html).html(),
                // Its note was deleted since.
                None => return,
            },
            None => note_editor("").html(),
        };
        self.editing = Some(Editing {
            id: draft.id.clone(),
            note_id: draft.note_id.clone(),
            thread_id: draft.thread_id.clone(),
            message_id: draft.message_id.clone(),
            files: draft.files.clone(),
            editor: note_editor(&draft.html),
            saved,
            focused: false,
            changed: None,
            drafted: true,
            failed: false,
            left: 0.0,
        });
    }

    /// The thread's notes, as the latest snapshot has them.
    pub fn all(&self) -> Arc<ThreadNotes> {
        self.notes.clone()
    }

    /// Where the notes show.
    pub fn placement(&self) -> NotesPlacement {
        self.placement
    }

    /// The open editor.
    pub fn editing(&self) -> Option<&Editing> {
        self.editing.as_ref()
    }

    /// Where each note's card was last painted.
    pub fn rects(&self) -> &[(String, Rect)] {
        &self.rects
    }

    /// The notes pane's timeline entries where they were last painted.
    pub fn timeline(&self) -> &[(Item, Rect)] {
        &self.timeline
    }

    /// Where the open editor was last painted.
    pub fn editor_rect(&self) -> Option<Rect> {
        self.editing.as_ref().and(self.editor_rect)
    }

    /// Flashes note `id`'s card, which the thread is scrolling to, from now on.
    pub fn flash(&mut self, id: String) {
        self.flash = Some((id, self.time));
    }

    /// The note flashing now, if one is.
    pub fn flashing(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(_, at)| self.time - at < FLASH_FOR)
            .map(|(id, _)| id.as_str())
    }

    /// How far into its flash note `id` is, from 0 to 1, while it flashes.
    fn flash_progress(&self, id: &str) -> Option<f32> {
        let (flashing, at) = self.flash.as_ref()?;
        let progress = (self.time - at) / FLASH_FOR;
        (flashing == id && progress < 1.0).then_some(progress.max(0.0) as f32)
    }

    /// Says who the open thread's messages are from: each one's id, its sender's name or
    /// address, and whether the sender is the user.
    pub fn set_senders<'a>(&mut self, messages: impl Iterator<Item = (&'a str, &'a str, bool)>) {
        for (id, from, mine) in messages {
            let known = self
                .senders
                .get(id)
                .is_some_and(|(name, was_mine)| *was_mine == mine && name == first_name(from));
            if !known {
                self.senders
                    .insert(id.to_owned(), (first_name(from).to_owned(), mine));
            }
        }
    }

    /// Says whether the editor may hold the keyboard this frame: the note editor's region is the
    /// one with it, not the palette or a dialog over the thread.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// Opens an editor for `target` with the keyboard, saving the open one first. A note that
    /// isn't in the thread, or no thread open, opens none.
    pub fn start(&mut self, target: Target, account: &Account) {
        let Some(thread_id) = self.thread.clone() else {
            return;
        };
        let (note, message_id) = match target {
            Target::Thread => (None, None),
            Target::Message(message) => (None, Some(message)),
            Target::Existing(id) => {
                if self.editing.as_ref().is_some_and(|open| open.id == id) {
                    self.focus();
                    return;
                }
                let Some(note) = self.notes.notes.iter().find(|note| note.id == id) else {
                    return;
                };
                (Some(note.clone()), note.message_id.clone())
            }
        };
        self.close(account);
        let mut editor = note_editor(note.as_ref().map_or("", |note| note.html.as_str()));
        // Typing goes on from the note's end.
        editor.set_selection(Selection::caret(editor.document().end()));
        self.editing = Some(Editing {
            id: note
                .as_ref()
                .map_or_else(exo_editor::uuid_v4, |note| note.id.clone()),
            note_id: note.as_ref().map(|note| note.id.clone()),
            thread_id,
            message_id,
            files: note.map(|note| note.files).unwrap_or_default(),
            saved: editor.html(),
            editor,
            focused: true,
            changed: None,
            drafted: false,
            failed: false,
            left: 0.0,
        });
        self.reveal = true;
    }

    /// Gives the open editor the keyboard.
    pub fn focus(&mut self) {
        if let Some(open) = &mut self.editing {
            open.focused = true;
            self.reveal = true;
        }
    }

    /// Takes the keyboard from the open editor, which stays open.
    pub fn blur(&mut self) {
        if let Some(open) = &mut self.editing {
            open.focused = false;
        }
    }

    /// Whether the editor opened or took the keyboard since this was last asked, so the thread
    /// scrolls to it.
    pub fn take_reveal(&mut self) -> bool {
        std::mem::take(&mut self.reveal)
    }

    /// Closes the open editor. A note that changed is saved, an empty new note is dropped, and
    /// what was kept of the editor's text goes either way.
    pub fn close(&mut self, account: &Account) {
        let Some(open) = self.editing.take() else {
            return;
        };
        self.closed.insert(open.id.clone());
        let empty = open.note_id.is_none() && open.editor.is_empty() && open.files.is_empty();
        if empty || !open.differs() {
            if open.drafted {
                account.discard_note_draft(open.id);
            }
            return;
        }
        account.save_note(SaveNote {
            html: open.editor.html(),
            text: open.text(),
            draft_id: open.id,
            note_id: open.note_id,
            thread_id: open.thread_id,
            message_id: open.message_id,
            files: open.files,
        });
    }

    /// Keeps what's typed as a draft, [`AUTOSAVE_AFTER`] its last change at `time` (the frame's),
    /// dated `now`.
    pub fn tick(&mut self, account: &Account, time: f64, now: Timestamp) {
        self.time = time;
        let Some(open) = &mut self.editing else {
            return;
        };
        if open
            .changed
            .is_some_and(|changed| time - changed >= AUTOSAVE_AFTER)
        {
            self.keep(account, now);
        }
    }

    /// Keeps what's typed and not kept yet as a draft now, dated `now`: the window is closing
    /// with the editor open, and the next launch opens it again.
    pub fn keep(&mut self, account: &Account, now: Timestamp) {
        let Some(open) = &mut self.editing else {
            return;
        };
        if open.changed.take().is_some() {
            open.drafted = true;
            account.save_note_draft(open.draft(now));
        }
    }

    /// A note, or the editor's draft, by id, couldn't be saved. Returns whether to say so: once
    /// for an open editor, however often its draft fails.
    pub fn failed(&mut self, id: &str) -> bool {
        match self.editing.as_mut().filter(|open| open.id == id) {
            Some(open) => !std::mem::replace(&mut open.failed, true),
            None => true,
        }
    }

    /// The region the editor holds the keyboard in, while it does.
    pub fn region(&self) -> Option<Region> {
        self.editing
            .as_ref()
            .is_some_and(|open| open.focused)
            .then_some(Region::NoteEditor)
    }

    /// Runs a formatting key in the open editor.
    pub fn format(&mut self, command: FormatCommand) {
        if let Some(open) = &mut self.editing {
            command.run(&mut open.editor);
        }
    }
}

impl crate::Mail {
    /// Runs each frame, once the snapshot is in: the notes follow the open thread's, the editor
    /// keeps the keyboard only while nothing over the thread has it, and what's typed is kept.
    pub(crate) fn notes_frame(&mut self, time: f64) {
        // A snapshot of another thread, on its way out, has nothing of this one's. Until this
        // one's is in, the notes are as opening the thread left them: none.
        let current = self.thread.as_ref().is_some_and(|thread| {
            self.snapshot
                .thread
                .as_ref()
                .is_some_and(|detail| detail.id.as_str() == thread.id())
        });
        if current {
            self.notes.set(
                self.snapshot.notes.clone(),
                self.account.settings().notes_placement,
            );
        }
        // A draft that took the keyboard has it.
        if self.compose.focused().is_some() {
            self.notes.blur();
        }
        self.notes
            .set_active(self.commands.region() == Region::NoteEditor);
        self.notes.tick(&self.account, time, self.now.timestamp());
    }

    /// Keeps what's typed in the open note editor, as the window closes or the account leaves
    /// the screen. The editor opens on it again with its thread.
    pub(crate) fn notes_keep(&mut self) {
        self.notes.keep(&self.account, self.now.timestamp());
    }

    /// Carries out a note command.
    pub(crate) fn notes_run(&mut self, action: crate::commands::Action) {
        use crate::commands::Action;
        match action {
            Action::NewThreadNote => self.notes_start(Target::Thread),
            Action::NewMessageNote => {
                let focused = self
                    .thread
                    .as_ref()
                    .and_then(|thread| thread.focused())
                    .map(str::to_owned);
                if let Some(message) = focused {
                    self.notes_start(Target::Message(message));
                }
            }
            Action::EditNote(id) => self.notes_start(Target::Existing(id)),
            Action::SaveNote | Action::CloseNote => self.notes.close(&self.account),
            Action::DeleteNote(id) => {
                self.account.delete_note(id);
                self.commands
                    .toast(exo_commands::Toast::new("Note deleted."));
            }
            Action::FormatNote(command) => self.notes.format(command),
            Action::ToggleNotesPane => {
                let shown = self.account.device_settings().notes_tab;
                self.notes_show_tab(!shown);
            }
            Action::SetNotesPlacement(placement) => self
                .account
                .update_settings(|settings| settings.notes_placement = placement),
            Action::ShareNotes => self.commands.toast(exo_commands::Toast::new(
                "Sharing arrives with the next update.",
            )),
            _ => {}
        }
    }

    /// Opens an editor for `target`. With notes in the pane only, it opens there, and the pane
    /// shows its Notes tab.
    pub(crate) fn notes_start(&mut self, target: Target) {
        self.notes.start(target, &self.account);
        if self.notes.editing().is_some()
            && self.account.settings().notes_placement == NotesPlacement::Pane
        {
            self.notes_show_tab(true);
        }
    }

    /// Shows the right pane's Notes tab (`true`) or its Contact tab, and remembers which.
    pub(crate) fn notes_show_tab(&mut self, notes: bool) {
        // With notes in the pane only, an editor the Contact tab hides lets go of the keyboard.
        if !notes && self.account.settings().notes_placement == NotesPlacement::Pane {
            self.notes.blur();
        }
        self.account
            .update_device_settings(|device| device.notes_tab = notes);
    }

    /// While a thread is open (and no new message's pane is), draws the right pane's tab row at
    /// `pane`'s top and returns the rest of the pane, which shows the chosen tab.
    pub(crate) fn notes_pane_tabs(&mut self, ui: &egui::Ui, pane: Rect, rem: f32) -> Option<Rect> {
        if self.thread.is_none() || self.compose.pane().is_some() {
            return None;
        }
        let row = Rect::from_min_max(
            pane.min,
            egui::pos2(pane.right(), pane.top() + pane::TABS_HEIGHT),
        );
        let shown = self.account.device_settings().notes_tab;
        if let Some(notes) = pane::tabs(ui, row, rem, shown, self.notes.all().notes.len()) {
            self.notes_show_tab(notes);
        }
        Some(Rect::from_min_max(
            egui::pos2(pane.left(), row.bottom()),
            pane.max,
        ))
    }

    /// Draws the Notes tab in `pane` and carries out what's clicked in it.
    pub(crate) fn show_notes_pane(&mut self, ui: &mut egui::Ui, pane: Rect, rem: f32, now: f64) {
        let detail = self
            .thread
            .as_ref()
            .and_then(|thread| thread.shared_detail());
        let Some(action) = pane::show(ui, pane, rem, &mut self.notes, detail.as_deref()) else {
            return;
        };
        use crate::commands::Action;
        match action {
            PaneAction::Reveal(id) => {
                if let Some(thread) = &mut self.thread {
                    thread.reveal_note(&id);
                }
                self.notes.flash(id);
            }
            PaneAction::RevealMessage(id) => {
                if let Some(thread) = &mut self.thread {
                    thread.focus_message(&id);
                }
            }
            PaneAction::Start(Target::Thread) => self.run(Action::NewThreadNote, now),
            PaneAction::Start(target) => self.notes_start(target),
            PaneAction::Card(id, CardAction::Edit) => self.run(Action::EditNote(id), now),
            PaneAction::Card(id, CardAction::Delete) => self.run(Action::DeleteNote(id), now),
            PaneAction::Card(_, CardAction::History) => {}
            PaneAction::Save => self.run(Action::SaveNote, now),
            PaneAction::Share => self.run(Action::ShareNotes, now),
            PaneAction::RestoreDeleted(id) => self.account.restore_note(id, None),
        }
    }
}

/// The first word of a sender's name, or the mailbox of a bare address.
fn first_name(from: &str) -> &str {
    let name = from.split_whitespace().next().unwrap_or(from);
    match name.split_once('@') {
        Some((mailbox, _)) if !mailbox.is_empty() => mailbox,
        _ => name,
    }
}

/// A date as notes tell it: `3 Nov`, with its year when that isn't `now`'s.
pub fn date(at: Timestamp, now: &Zoned) -> String {
    let at = at.to_zoned(now.time_zone().clone());
    let month = at.strftime("%b");
    if at.year() == now.year() {
        format!("{} {month}", at.day())
    } else {
        format!("{} {month} {}", at.day(), at.year())
    }
}

/// What a note is on, for its card and wherever else notes are listed: `whole thread`,
/// `on Dana's email of 3 Nov`, or `on my reply of 3 Nov` when `mine` says its message is the
/// user's (the message's own is-me flag). Dates are told in `now`'s zone.
pub fn where_line(note: &Note, mine: bool, now: &Zoned) -> String {
    if note.message_id.is_none() {
        return "whole thread".to_owned();
    }
    let what = match note.message_from.as_deref().map(first_name) {
        _ if mine => "my reply".to_owned(),
        Some(name) => format!("{name}'s email"),
        None => "an email".to_owned(),
    };
    match note.message_date {
        Some(at) => format!("on {what} of {}", date(at, now)),
        None => format!("on {what}"),
    }
}

/// An editor on `html`, set in the notes' text: the one a note is written in, and the one its
/// card shows it read-only in.
fn note_editor(html: &str) -> Editor {
    let mut editor = Editor::from_html(html);
    editor.set_body_text(BODY);
    editor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(message: Option<(&str, &str)>) -> Note {
        let at = |text: &str| text.parse::<Timestamp>().expect("a timestamp");
        Note {
            id: "6b0d3c55-6f43-4a8e-9a39-0c1f6f3f8a11".to_owned(),
            thread_id: ThreadId::new("thread".to_owned()),
            message_id: message.map(|_| "message".to_owned()),
            message_from: message.map(|(from, _)| from.to_owned()),
            message_date: message.map(|(_, date)| at(date)),
            number: message.map(|_| 2),
            kind: "note".to_owned(),
            html: "<div>Ask</div>".to_owned(),
            text: "Ask".to_owned(),
            files: Vec::new(),
            created_at: at("2026-11-06T15:00:00Z"),
            updated_at: at("2026-11-06T15:00:00Z"),
            deleted_at: None,
            version: 1,
        }
    }

    fn now() -> Zoned {
        "2026-11-23T10:00:00-05:00[America/New_York]"
            .parse()
            .expect("a time")
    }

    #[test]
    fn where_lines_say_what_a_note_is_on() {
        assert_eq!(where_line(&note(None), false, &now()), "whole thread");
        assert_eq!(
            where_line(
                &note(Some(("Dana Reyes", "2026-11-03T15:00:00Z"))),
                false,
                &now()
            ),
            "on Dana's email of 3 Nov"
        );
        assert_eq!(
            where_line(
                &note(Some(("dana@example.com", "2025-11-03T15:00:00Z"))),
                false,
                &now()
            ),
            "on dana's email of 3 Nov 2025"
        );
        // Whatever name the message carries, the user's own is their reply.
        assert_eq!(
            where_line(
                &note(Some(("Maya Okafor", "2026-11-12T15:00:00Z"))),
                true,
                &now()
            ),
            "on my reply of 12 Nov"
        );
    }

    #[test]
    fn dates_are_told_in_the_clocks_zone() {
        // 3 Nov, 03:00 UTC is still 2 Nov in New York.
        let at = "2026-11-03T03:00:00Z".parse().expect("a timestamp");
        assert_eq!(date(at, &now()), "2 Nov");
    }

    #[test]
    fn a_message_with_no_sender_kept_is_an_email() {
        let mut orphan = note(Some(("Dana", "2026-11-03T15:00:00Z")));
        orphan.message_from = None;
        orphan.message_date = None;
        assert_eq!(where_line(&orphan, false, &now()), "on an email");
    }
}
