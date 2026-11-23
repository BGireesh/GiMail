//! Thread notes, end to end: notes saved through the account reach the open thread's snapshot.

mod support;

use egui::{Key, Modifiers};
use exo_store::{SaveNote, ThreadId};
use support::{Fixture, start, start_with_settings};

#[test]
fn a_note_saved_through_the_account_reaches_the_snapshot() {
    let mut app = start("notes-reach-snapshot");
    app.key(Key::Enter);
    let thread = app
        .mail()
        .thread()
        .expect("a thread is open")
        .id()
        .to_owned();
    assert!(app.mail().notes().notes.is_empty(), "no notes yet");
    app.mail_mut().account_mut().save_note(SaveNote {
        draft_id: "6b0d3c55-6f43-4a8e-9a39-0c1f6f3f8a11".to_owned(),
        note_id: None,
        thread_id: ThreadId::new(thread),
        message_id: None,
        html: "<p>Ask about the budget</p>".to_owned(),
        text: "Ask about the budget".to_owned(),
        files: Vec::new(),
    });
    app.settle();
    let notes = &app.mail().notes().notes;
    assert_eq!(
        notes.iter().map(exo_store::Note::label).collect::<Vec<_>>(),
        ["Thread note"]
    );
    assert_eq!(notes[0].text, "Ask about the budget");
}

const COMMAND: Modifiers = Modifiers::MAC_CMD.plus(Modifiers::COMMAND);

/// Opens the first conversation and a new thread note in it.
fn start_thread_note(name: &str) -> Fixture {
    let mut app = start(name);
    app.key(Key::Enter);
    app.key(Key::T);
    assert_eq!(
        app.mail().note_editor_text().as_deref(),
        Some(""),
        "T opens an empty editor"
    );
    app
}

/// Opens `Debrief: Senior Product Designer (Aiko)`: three messages, the last one focused.
fn open_debrief(app: &mut Fixture) {
    for _ in 0..8 {
        app.key(Key::J);
    }
    app.key(Key::Enter);
    let thread = app.mail().thread().expect("a thread is open");
    assert_eq!(thread.messages().len(), 3);
}

/// Presses `key` with `modifiers` as a keyboard does: the key, then the text it types.
fn type_key(app: &mut Fixture, key: Key, modifiers: Modifiers, text: &str) {
    let event = |pressed| egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    };
    let events = &mut app.harness.input_mut().events;
    events.push(event(true));
    events.push(egui::Event::Text(text.to_owned()));
    app.harness.run_steps(1);
    app.harness.input_mut().events.push(event(false));
    app.settle();
}

/// How far left of a note card's right edge its hover links' middles are.
const DELETE: f32 = 33.0;
const EDIT: f32 = 77.0;

/// Points at note `id`'s card and clicks the link `from_right` of its right edge.
fn click_link(app: &mut Fixture, id: &str, from_right: f32) {
    let rects = app.mail().note_rects();
    let (_, rect) = rects
        .iter()
        .find(|(note, _)| note == id)
        .expect("the note is drawn");
    let at = egui::pos2(rect.right() - from_right, rect.top() + 17.0);
    app.hover(at);
    app.click(at);
}

/// The labels of the open thread's live notes.
fn labels(app: &Fixture) -> Vec<String> {
    app.mail()
        .notes()
        .notes
        .iter()
        .map(exo_store::Note::label)
        .collect()
}

#[test]
fn t_opens_a_thread_note_and_command_enter_saves_it() {
    let mut app = start_thread_note("notes-t-saves");
    app.type_text("Ask about the budget");
    app.press(Key::Enter, COMMAND);
    assert_eq!(labels(&app), ["Thread note"]);
    let note = &app.mail().notes().notes[0];
    assert_eq!(note.text, "Ask about the budget");
    assert_eq!(note.message_id, None);
    assert_eq!(app.mail().note_editor_text(), None, "the editor closed");
    assert_eq!(app.mail().toast_text(), None, "saving says nothing");
}

#[test]
fn shift_t_notes_the_focused_message() {
    let mut app = start("notes-shift-t");
    open_debrief(&mut app);
    app.key(Key::P);
    let focused = app
        .mail()
        .thread()
        .and_then(|thread| thread.focused())
        .expect("a message is focused")
        .to_owned();
    app.press(Key::T, Modifiers::SHIFT);
    app.type_text("She was strong on systems");
    app.press(Key::Enter, COMMAND);
    assert_eq!(labels(&app), ["Note 1"]);
    let note = &app.mail().notes().notes[0];
    assert_eq!(note.message_id.as_deref(), Some(focused.as_str()));
    assert_eq!(note.text, "She was strong on systems");
}

#[test]
fn escape_keeps_what_was_typed() {
    let mut app = start_thread_note("notes-escape-keeps");
    app.type_text("Call Dana back");
    app.key(Key::Escape);
    assert_eq!(app.mail().note_editor_text(), None, "Escape closed it");
    assert!(app.mail().thread().is_some(), "and didn't leave the thread");
    let notes = &app.mail().notes().notes;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "Call Dana back");
    app.key(Key::Escape);
    assert!(app.mail().thread().is_none(), "a second Escape leaves");
}

#[test]
fn an_empty_note_is_dropped() {
    let mut app = start_thread_note("notes-empty-dropped");
    app.press(Key::Enter, COMMAND);
    assert_eq!(app.mail().note_editor_text(), None);
    assert!(app.mail().notes().notes.is_empty());
    app.key(Key::T);
    app.type_text("x");
    app.key(Key::Backspace);
    app.wait(1.5);
    app.key(Key::Escape);
    assert_eq!(app.mail().note_editor_text(), None);
    let notes = app.mail().notes();
    assert!(notes.notes.is_empty(), "nothing was saved");
    assert!(notes.drafts.is_empty(), "and nothing is kept of it");
}

#[test]
fn typing_in_a_note_does_not_trigger_shortcuts() {
    let mut app = start_thread_note("notes-typing-no-shortcuts");
    let thread = app.mail().thread().expect("open").id().to_owned();
    let listed = app.subjects().len();
    type_key(&mut app, Key::E, Modifiers::NONE, "e");
    type_key(&mut app, Key::Num3, Modifiers::SHIFT, "#");
    type_key(&mut app, Key::J, Modifiers::NONE, "j");
    type_key(&mut app, Key::T, Modifiers::NONE, "t");
    assert_eq!(app.mail().note_editor_text().as_deref(), Some("e#jt"));
    assert_eq!(
        app.mail().thread().map(|open| open.id()),
        Some(thread.as_str()),
        "the thread is still open"
    );
    assert_eq!(app.subjects().len(), listed, "neither done nor trashed");
    let row = app
        .mail()
        .inbox()
        .threads()
        .iter()
        .find(|row| row.id == thread)
        .expect("still listed");
    assert!(!row.done);
}

#[test]
fn editing_a_note_adds_a_version() {
    let mut app = start_thread_note("notes-edit-version");
    app.type_text("First");
    app.press(Key::Enter, COMMAND);
    let note = app.mail().notes().notes[0].clone();
    assert_eq!(note.version, 1);
    click_link(&mut app, &note.id, EDIT);
    assert_eq!(app.mail().note_editor_text().as_deref(), Some("First"));
    app.type_text(" and second");
    app.press(Key::Enter, COMMAND);
    let notes = &app.mail().notes().notes;
    assert_eq!(notes.len(), 1, "the same note");
    assert_eq!(notes[0].id, note.id);
    assert_eq!(notes[0].text, "First and second");
    assert_eq!(notes[0].version, 2);
    // Opening it and closing it unchanged writes none.
    click_link(&mut app, &note.id, EDIT);
    assert_eq!(
        app.mail().note_editor_text().as_deref(),
        Some("First and second")
    );
    app.key(Key::Escape);
    assert_eq!(app.mail().notes().notes[0].version, 2);
}

#[test]
fn deleting_a_note_removes_it_from_the_thread() {
    let mut app = start_thread_note("notes-delete");
    app.type_text("Not for long");
    app.press(Key::Enter, COMMAND);
    let id = app.mail().notes().notes[0].id.clone();
    assert_eq!(app.mail().note_rects().len(), 1, "its card is drawn");
    click_link(&mut app, &id, DELETE);
    let notes = app.mail().notes();
    assert!(notes.notes.is_empty());
    assert_eq!(notes.deleted.len(), 1, "it can come back");
    assert_eq!(notes.deleted[0].id, id);
    assert_eq!(app.mail().toast_text(), Some("Note deleted."));
    assert!(app.mail().note_rects().is_empty(), "its card is gone");
}

#[test]
fn leaving_the_thread_mid_note_keeps_the_text() {
    let mut app = start_thread_note("notes-leave-keeps");
    let thread = app.mail().thread().expect("open").id().to_owned();
    app.type_text("Half a thought");
    app.key(Key::Escape);
    app.key(Key::T);
    app.type_text("Another one");
    // The messages take the keyboard, and J goes to the next conversation.
    app.click(egui::pos2(640.0, 700.0));
    app.key(Key::J);
    let open = app.mail().thread().expect("the next one").id().to_owned();
    assert_ne!(open, thread);
    assert_eq!(app.mail().note_editor_text(), None, "no editor follows");
    app.key(Key::K);
    assert_eq!(
        app.mail().thread().map(|open| open.id()),
        Some(thread.as_str())
    );
    // Both were saved in the same instant of the fixture's clock, so their order isn't told.
    let mut texts: Vec<&str> = app
        .mail()
        .notes()
        .notes
        .iter()
        .map(|note| note.text.as_str())
        .collect();
    texts.sort_unstable();
    assert_eq!(texts, ["Another one", "Half a thought"]);
    assert_eq!(app.mail().note_editor_text(), None, "saved, not left open");
}

#[test]
fn mail_arriving_mid_note_keeps_the_editor() {
    let mut app = start_thread_note("notes-mail-arrives");
    let thread = app.mail().thread().expect("open").id().to_owned();
    app.type_text("Mid-thought");
    let deliver = app.mock_url().join("__mock/deliver").expect("a URL");
    let reply = serde_json::json!({ "threadId": thread, "from": "Ann Lee <ann@example.com>", "text": "Thanks for this." });
    app.block_on(async {
        reqwest::Client::new()
            .post(deliver)
            .json(&reply)
            .send()
            .await?
            .error_for_status()
    })
    .expect("delivered");
    app.sync();
    assert!(
        app.mail().thread().expect("still open").has_new_messages(),
        "the reply arrived"
    );
    assert_eq!(
        app.mail().note_editor_text().as_deref(),
        Some("Mid-thought")
    );
    app.type_text(" continues");
    assert_eq!(
        app.mail().note_editor_text().as_deref(),
        Some("Mid-thought continues"),
        "and it still has the keyboard"
    );
}

#[test]
fn a_draft_is_recovered_after_a_relaunch() {
    let mut app = start_thread_note("notes-draft-recovered");
    app.type_text("Unsaved words");
    app.wait(1.5);
    assert_eq!(app.mail().notes().drafts.len(), 1, "kept as it's typed");
    app.relaunch();
    assert_eq!(app.mail().note_editor_text(), None, "no thread is open yet");
    app.key(Key::Enter);
    assert_eq!(
        app.mail().note_editor_text().as_deref(),
        Some("Unsaved words")
    );
    assert!(app.mail().notes().notes.is_empty(), "still not a note");
    // It opens without the keyboard, and leaving saves it as the note it would have been.
    app.key(Key::Escape);
    assert!(
        app.mail().thread().is_none(),
        "Escape in the messages left the thread"
    );
    app.key(Key::Enter);
    let notes = &app.mail().notes().notes;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "Unsaved words");
}

#[test]
fn notes_sit_under_their_message() {
    let mut app = start("notes-under-message");
    open_debrief(&mut app);
    app.key(Key::P);
    let thread = app.mail().thread().expect("open");
    let focused = thread.focused().expect("a message is focused").to_owned();
    let ids: Vec<String> = thread
        .messages()
        .iter()
        .map(|message| message.id.clone())
        .collect();
    assert_eq!(focused, ids[1], "the middle message");
    app.press(Key::T, Modifiers::SHIFT);
    app.type_text("Between the two");
    app.press(Key::Enter, COMMAND);
    let note = app.mail().notes().notes[0].id.clone();
    let rects = app.mail().note_rects();
    let (_, rect) = rects
        .iter()
        .find(|(id, _)| *id == note)
        .expect("the note is drawn");
    let cards = app.mail().thread().expect("open").card_rects();
    let card = |id: &str| {
        cards
            .iter()
            .find(|(card, _)| card == id)
            .map(|(_, rect)| *rect)
            .expect("the message is drawn")
    };
    let (above, below) = (card(&ids[1]), card(&ids[2]));
    assert!(
        rect.top() >= above.bottom(),
        "under its message: {rect:?} {above:?}"
    );
    assert!(
        rect.bottom() <= below.top(),
        "above the next: {rect:?} {below:?}"
    );
    assert!(rect.left() > above.left(), "and indented");
}

#[test]
fn t_in_a_reply_is_typed_not_a_note() {
    let mut app = start("notes-t-in-reply");
    app.key(Key::Enter);
    app.key(Key::R);
    type_key(&mut app, Key::T, Modifiers::NONE, "t");
    type_key(&mut app, Key::T, Modifiers::SHIFT, "T");
    assert_eq!(app.mail().note_editor_text(), None, "no note opened");
    assert!(
        app.mail()
            .compose_body()
            .is_some_and(|body| body.contains("tT")),
        "the reply took both"
    );
}

#[test]
fn in_the_pane_only_the_thread_draws_no_note_cards() {
    let mut app = start_with_settings("notes-pane-placement", &|settings| {
        settings.notes_placement = exo_store::NotesPlacement::Pane;
    });
    app.key(Key::Enter);
    app.key(Key::T);
    app.type_text("Kept out of the column");
    app.press(Key::Enter, COMMAND);
    assert_eq!(labels(&app), ["Thread note"], "T still writes one");
    assert!(app.mail().note_rects().is_empty(), "but no card is drawn");
}

#[test]
fn a_note_stays_out_of_a_reply() {
    let mut app = start_thread_note("notes-stay-private");
    app.type_text("zebra-crossing");
    app.press(Key::Enter, COMMAND);
    assert_eq!(labels(&app), ["Thread note"]);
    app.key(Key::R);
    assert!(
        app.mail()
            .compose_body()
            .is_some_and(|body| !body.contains("zebra-crossing")),
        "the reply quotes the mail alone"
    );
}
