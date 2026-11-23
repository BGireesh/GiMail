//! Thread notes, end to end: notes saved through the account reach the open thread's snapshot.

mod support;

use egui::{Key, Modifiers};
use exo::notes::{CardAction, Control, Item};
use exo_store::NotesPlacement;
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

/// Points at note `id`'s card and clicks its link for `action`.
fn click_link(app: &mut Fixture, id: &str, action: CardAction) {
    let rects = app.mail().note_rects();
    let (_, rect) = rects
        .iter()
        .find(|(note, _)| note == id)
        .expect("the note is drawn");
    app.hover(rect.center());
    click_control(app, &Control::Card(id.to_owned(), action));
}

/// Clicks the notes' control `control`, as last drawn.
fn click_control(app: &mut Fixture, control: &Control) {
    let controls = app.mail().note_controls();
    let (_, rect) = controls
        .iter()
        .find(|(drawn, _)| drawn == control)
        .unwrap_or_else(|| panic!("{control:?} is drawn: {controls:?}"));
    app.click(rect.center());
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
    click_link(&mut app, &note.id, CardAction::Edit);
    assert_eq!(app.mail().note_editor_text().as_deref(), Some("First"));
    app.type_text(" and second");
    app.press(Key::Enter, COMMAND);
    let notes = &app.mail().notes().notes;
    assert_eq!(notes.len(), 1, "the same note");
    assert_eq!(notes[0].id, note.id);
    assert_eq!(notes[0].text, "First and second");
    assert_eq!(notes[0].version, 2);
    // Opening it and closing it unchanged writes none.
    click_link(&mut app, &note.id, CardAction::Edit);
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
    click_link(&mut app, &id, CardAction::Delete);
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

const COMMAND_SHIFT: Modifiers = COMMAND.plus(Modifiers::SHIFT);

/// Opens the palette and runs the row `query` ranks first.
fn palette(app: &mut Fixture, query: &str) {
    app.press(Key::K, COMMAND);
    app.harness
        .input_mut()
        .events
        .push(egui::Event::Text(query.to_owned()));
    app.harness.run_steps(1);
    app.key(Key::Enter);
}

/// Writes a note with the editor `open` opens, and saves it.
fn write_note(app: &mut Fixture, open: impl FnOnce(&mut Fixture), text: &str) {
    open(app);
    app.type_text(text);
    app.press(Key::Enter, COMMAND);
}

#[test]
fn command_shift_n_switches_the_pane_to_notes_and_back() {
    let mut app = start("notes-command-shift-n");
    app.key(Key::Enter);
    assert!(!app.mail().notes_tab(), "the pane starts on the contact");
    app.press(Key::N, COMMAND_SHIFT);
    assert!(app.mail().notes_tab(), "\u{2318}\u{21e7}N shows the notes");
    app.press(Key::N, COMMAND_SHIFT);
    assert!(!app.mail().notes_tab(), "and again the contact");
    app.press(Key::N, COMMAND_SHIFT);
    // The pane remembers the tab from one thread to the next, and across a relaunch.
    app.key(Key::Escape);
    assert!(!app.mail().notes_tab(), "no notes tab without a thread");
    app.key(Key::Enter);
    assert!(app.mail().notes_tab(), "remembered for the next thread");
    app.relaunch();
    app.key(Key::Enter);
    assert!(app.mail().notes_tab(), "and after a relaunch");
}

#[test]
fn the_timeline_lists_notes_and_emails_in_order() {
    let mut app = start("notes-timeline-order");
    open_debrief(&mut app);
    let messages: Vec<(String, jiff::Timestamp)> = app
        .mail()
        .thread()
        .expect("open")
        .messages()
        .iter()
        .map(|message| (message.id.clone(), message.date))
        .collect();
    let zone = jiff::tz::TimeZone::UTC;
    // A note on the first email, written before the second arrived.
    app.set_now((messages[0].1 + jiff::SignedDuration::from_mins(1)).to_zoned(zone.clone()));
    app.key(Key::P);
    app.key(Key::P);
    write_note(
        &mut app,
        |app| app.press(Key::T, Modifiers::SHIFT),
        "Ask about the offer deadline",
    );
    // A thread note, written after the last.
    app.set_now((messages[2].1 + jiff::SignedDuration::from_mins(1)).to_zoned(zone));
    write_note(&mut app, |app| app.key(Key::T), "Decide by Friday");
    app.press(Key::N, COMMAND_SHIFT);
    let notes = &app.mail().notes().notes;
    let id = |text: &str| {
        notes
            .iter()
            .find(|note| note.text == text)
            .map(|note| note.id.clone())
            .expect("the note was saved")
    };
    let expected = vec![
        Item::Email(messages[0].0.clone()),
        Item::Note(id("Ask about the offer deadline")),
        Item::Email(messages[1].0.clone()),
        Item::Email(messages[2].0.clone()),
        Item::Note(id("Decide by Friday")),
    ];
    let listed: Vec<Item> = app
        .mail()
        .notes_timeline()
        .into_iter()
        .map(|(item, _)| item)
        .collect();
    assert_eq!(listed, expected);
    let pane = app.mail().right_pane();
    assert!(
        app.mail()
            .notes_timeline()
            .iter()
            .all(|(_, rect)| pane.contains_rect(*rect)),
        "in the right pane"
    );
}

#[test]
fn clicking_a_timeline_entry_reveals_the_note() {
    let mut app = start("notes-timeline-reveal");
    open_debrief(&mut app);
    // Tall thread notes push the messages down past the fold.
    for _ in 0..5 {
        write_note(
            &mut app,
            |app| {
                app.key(Key::T);
                for line in ["One", "Two", "Three", "Four"] {
                    app.type_text(line);
                    app.key(Key::Enter);
                }
            },
            "Five",
        );
    }
    // The topmost of them.
    let id = app
        .mail()
        .note_rects()
        .into_iter()
        .min_by(|(_, a), (_, b)| a.top().total_cmp(&b.top()))
        .map(|(id, _)| id)
        .expect("the notes are drawn");
    // The messages take the keyboard, and the thread scrolls to its end, where the note is out
    // of view.
    app.click(egui::pos2(640.0, 700.0));
    app.press(Key::G, Modifiers::SHIFT);
    app.settle();
    let card = |app: &Fixture| {
        app.mail()
            .note_rects()
            .into_iter()
            .find(|(note, _)| *note == id)
            .map(|(_, rect)| rect)
            .expect("the card is drawn")
    };
    let content = app.mail().thread().expect("still open").messages_rect();
    assert!(
        !content.contains_rect(card(&app)),
        "scrolled away: {:?} in {content:?}",
        card(&app)
    );
    app.press(Key::N, COMMAND_SHIFT);
    let (_, entry) = app
        .mail()
        .notes_timeline()
        .into_iter()
        .find(|(item, _)| *item == Item::Note(id.clone()))
        .expect("the note is listed");
    app.click(entry.center());
    assert_eq!(
        app.mail().flashing_note(),
        Some(id.as_str()),
        "the note flashes"
    );
    app.wait(1.2);
    assert_eq!(app.mail().flashing_note(), None, "for a second");
    let card = card(&app);
    assert!(
        content.contains_rect(card),
        "in view: {card:?} in {content:?}"
    );
}

#[test]
fn pane_only_mode_keeps_notes_out_of_the_thread() {
    let mut app = start_with_settings("notes-pane-only", &|settings| {
        settings.notes_placement = NotesPlacement::Pane;
    });
    app.key(Key::Enter);
    let pane = app.mail().right_pane();
    app.key(Key::T);
    assert!(app.mail().notes_tab(), "T shows the notes");
    let editor = app.mail().note_editor_rect().expect("an editor is open");
    assert!(pane.contains_rect(editor), "in the pane: {editor:?}");
    app.type_text("Kept out of the column");
    app.press(Key::Enter, COMMAND);
    assert_eq!(labels(&app), ["Thread note"]);
    let rects = app.mail().note_rects();
    assert_eq!(rects.len(), 1, "its card is drawn");
    assert!(
        rects.iter().all(|(_, rect)| pane.contains_rect(*rect)),
        "in the pane alone: {rects:?}"
    );
}

#[test]
fn the_command_switches_placement() {
    let mut app = start("notes-placement-command");
    app.key(Key::Enter);
    assert_eq!(app.mail().notes_placement(), NotesPlacement::Thread);
    palette(&mut app, "Show Notes in Pane Only");
    assert_eq!(app.mail().notes_placement(), NotesPlacement::Pane);
    palette(&mut app, "Show Notes in Thread");
    assert_eq!(app.mail().notes_placement(), NotesPlacement::Thread);
}

#[test]
fn a_thread_with_notes_is_marked_in_the_inbox() {
    let mut app = start_thread_note("notes-inbox-icon");
    let thread = app.mail().thread().expect("open").id().to_owned();
    app.type_text("Follow up");
    app.press(Key::Enter, COMMAND);
    app.key(Key::Escape);
    app.settle();
    let threads = app.mail().inbox().threads();
    let row = threads
        .iter()
        .find(|row| row.id == thread)
        .expect("still listed");
    assert_eq!(row.notes, 1);
    assert!(
        threads
            .iter()
            .filter(|row| row.id != thread)
            .all(|row| row.notes == 0),
        "the others have none"
    );
}

#[test]
fn escape_closes_a_note_with_a_colon_in_it() {
    let mut app = start_thread_note("notes-escape-colon");
    app.type_text("Ratio :smi");
    app.key(Key::Escape);
    assert_eq!(
        app.mail().note_editor_text(),
        None,
        "the first Escape closed it"
    );
    assert_eq!(app.mail().notes().notes[0].text, "Ratio :smi");
}

#[test]
fn moving_notes_to_the_pane_takes_the_open_editor_along() {
    let mut app = start_thread_note("notes-move-to-pane");
    app.type_text("Half");
    palette(&mut app, "Show Notes in Pane Only");
    assert_eq!(app.mail().notes_placement(), NotesPlacement::Pane);
    assert!(app.mail().notes_tab(), "the pane shows the notes");
    let editor = app.mail().note_editor_rect().expect("still open");
    assert!(
        app.mail().right_pane().contains_rect(editor),
        "in the pane: {editor:?}"
    );
    app.type_text(" and more");
    assert_eq!(
        app.mail().note_editor_text().as_deref(),
        Some("Half and more"),
        "and it still has the keyboard"
    );
}

#[test]
fn a_hidden_editor_lets_go_of_the_keyboard_and_keeps_its_text() {
    let mut app = start_with_settings("notes-hidden-editor", &|settings| {
        settings.notes_placement = NotesPlacement::Pane;
    });
    app.key(Key::Enter);
    let thread = app.mail().thread().expect("open").id().to_owned();
    app.key(Key::T);
    app.type_text("Kept");
    // The settings menu takes the right pane, and the editor with it.
    palette(&mut app, "Settings");
    assert!(app.mail().settings_visible());
    assert_eq!(app.mail().note_editor_text().as_deref(), Some("Kept"));
    // J now moves to the next conversation, which saves the note.
    app.key(Key::J);
    assert_ne!(
        app.mail().thread().map(|open| open.id().to_owned()),
        Some(thread),
        "J went to the next conversation"
    );
    app.key(Key::K);
    assert_eq!(labels(&app), ["Thread note"]);
    assert_eq!(app.mail().notes().notes[0].text, "Kept");
}

/// A file dropped on the window.
#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|error| error.to_string())
    }
}

/// Drops the file `name` holding `bytes` on the window, and lets it be kept.
fn drop_file(app: &mut Fixture, name: &str, bytes: &[u8]) {
    let folder = app.folder("drops");
    std::fs::create_dir_all(&folder).expect("a folder");
    let path = folder.join(name);
    std::fs::write(&path, bytes).expect("written");
    app.harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(path)));
    app.harness.run_steps(2);
    app.wait(0.5);
}

/// A small PNG.
fn png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(40, 30, image::Rgba([40, 90, 200, 255]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encoded");
    png
}

/// Pastes `png` as an image on the clipboard, and lets it be kept.
fn paste_image(app: &mut Fixture, png: Vec<u8>) {
    exo::clipboard::capture_image(&app.harness.ctx, png);
    app.harness
        .input_mut()
        .events
        .push(egui::Event::Paste(String::new()));
    app.harness.run_steps(2);
    app.wait(0.5);
}

#[test]
fn a_dropped_file_is_listed_under_the_note_and_saved() {
    let mut app = start_thread_note("notes-drop-file");
    app.type_text("Agenda attached");
    drop_file(&mut app, "agenda.txt", b"1. Budget");
    assert!(
        !app.mail().compose_pane_open() && app.mail().compose_body().is_none(),
        "no draft takes the drop"
    );
    app.press(Key::Enter, COMMAND);
    let note = &app.mail().notes().notes[0];
    assert_eq!(note.files.len(), 1, "{:?}", note.files);
    let file = &note.files[0];
    assert_eq!(
        (file.name.as_str(), file.content_id.as_deref()),
        ("agenda.txt", None)
    );
    let kept = app.mail().account().note_files().join(&file.uuid);
    assert_eq!(
        std::fs::read(kept).expect("its bytes are kept"),
        b"1. Budget"
    );
}

#[test]
fn a_pasted_image_goes_inline() {
    let mut app = start_thread_note("notes-paste-image");
    app.type_text("Whiteboard");
    paste_image(&mut app, png());
    app.wait(0.5);
    assert!(
        app.mail().compose_body().is_none(),
        "the paste makes no draft"
    );
    app.press(Key::Enter, COMMAND);
    let note = &app.mail().notes().notes[0];
    assert!(note.html.contains("src-cid"), "inline: {}", note.html);
    let [file] = note.files.as_slice() else {
        panic!("one file: {:?}", note.files)
    };
    let cid = file.content_id.as_deref().expect("a content id");
    assert!(note.html.contains(cid), "the text shows it: {}", note.html);
    assert_eq!(file.mime_type, "image/png");
    assert!(
        app.mail().account().note_files().join(&file.uuid).is_file(),
        "its bytes are kept"
    );
}

#[test]
fn an_image_taken_out_of_the_text_is_not_saved() {
    let mut app = start_thread_note("notes-image-undone");
    app.type_text("Whiteboard");
    paste_image(&mut app, png());
    app.press(Key::Z, COMMAND);
    app.press(Key::Enter, COMMAND);
    let note = &app.mail().notes().notes[0];
    assert!(!note.html.contains("src-cid"), "{}", note.html);
    assert!(note.files.is_empty(), "{:?}", note.files);
}

#[test]
fn an_empty_file_is_refused_with_a_toast() {
    let mut app = start_thread_note("notes-empty-file");
    drop_file(&mut app, "empty.txt", b"");
    assert_eq!(app.mail().toast_text(), Some("That file is empty."));
    app.type_text("Nothing attached");
    app.press(Key::Enter, COMMAND);
    assert!(app.mail().notes().notes[0].files.is_empty());
}

#[test]
fn command_v_in_a_note_pastes_into_it_not_a_new_draft() {
    let mut app = start_thread_note("notes-paste-text");
    exo::clipboard::capture(&app.harness.ctx);
    app.harness
        .input_mut()
        .events
        .push(egui::Event::Paste("Pasted".to_owned()));
    app.harness.run_steps(2);
    // The editor pastes the system clipboard's HTML over the event's text when there is some,
    // so what's pasted depends on the machine; that it lands in the note doesn't.
    assert!(
        app.mail()
            .note_editor_text()
            .is_some_and(|text| !text.is_empty()),
        "the paste goes into the note"
    );
    assert!(
        !app.mail().compose_pane_open() && app.mail().compose_body().is_none(),
        "no draft is made of the clipboard"
    );
}

#[test]
fn a_file_dropped_while_the_note_editor_lacks_the_keyboard_goes_to_the_note() {
    let mut app = start_thread_note("notes-drop-unfocused");
    app.type_text("Agenda");
    let editor = app.mail().note_editor_rect().expect("the editor is open");
    // A press in the thread, below the editor, takes the keyboard from it.
    app.click(egui::pos2(editor.center().x, editor.bottom() + 400.0));
    app.type_text("x");
    assert_eq!(
        app.mail().note_editor_text().as_deref(),
        Some("Agenda"),
        "the editor no longer has the keyboard"
    );
    drop_file(&mut app, "agenda.txt", b"1. Budget");
    assert!(
        !app.mail().compose_pane_open() && app.mail().compose_body().is_none(),
        "no draft takes the drop"
    );
    // Back in the editor, and saved.
    app.click(editor.center());
    app.press(Key::Enter, COMMAND);
    let note = &app.mail().notes().notes[0];
    assert_eq!(note.text, "Agenda", "the typed text is kept");
    assert_eq!(
        note.files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>(),
        ["agenda.txt"]
    );
}

/// Searches `query` from the list, and lets the results take the keyboard.
fn search_for(app: &mut Fixture, query: &str) {
    app.key(Key::Slash);
    app.type_text(query);
    app.key(Key::Enter);
}

/// Writes a thread note on the first conversation, deletes the conversation in Gmail and syncs,
/// then searches `notes:overage`, which lists it alone, its mail gone. Returns its id and subject.
fn deleted_result(name: &str) -> (Fixture, String, String) {
    let mut app = start_thread_note(name);
    let (thread, subject) = {
        let open = app.mail().thread().expect("open");
        let subject = open.detail().expect("loaded").subject.clone();
        (open.id().to_owned(), subject)
    };
    app.type_text("Ask about the overage fee");
    app.press(Key::Enter, COMMAND);
    app.key(Key::Escape);
    let gmail = app.gmail();
    app.block_on(async { gmail.gmail().delete_thread(&thread).await })
        .expect("deleted in Gmail");
    app.sync();
    assert!(
        !app.subjects().contains(&subject),
        "gone from the inbox: {:?}",
        app.subjects()
    );
    search_for(&mut app, "notes:overage");
    let rows = app.mail().inbox().threads();
    assert_eq!(rows.len(), 1, "{:?}", app.subjects());
    assert_eq!(rows[0].id, thread);
    (app, thread, subject)
}

#[test]
fn a_note_on_a_deleted_thread_opens_alone() {
    let (mut app, thread, subject) = deleted_result("notes-deleted-thread");
    let rows = app.mail().inbox().threads();
    assert_eq!(rows[0].subject, subject);
    assert_eq!(rows[0].snippet, "Email deleted");
    let hits = app.mail().search_note_hits(0);
    assert!(
        hits.first()
            .is_some_and(|hit| hit.ends_with("— Ask about the overage fee")),
        "{hits:?}"
    );
    app.key(Key::Enter);
    assert_eq!(
        app.mail().thread().map(|open| open.id()),
        Some(thread.as_str()),
        "it stays open"
    );
    assert_eq!(app.mail().notes_alone(), Some(subject));
    let note = app.mail().notes().notes[0].id.clone();
    assert!(
        app.mail().note_rects().iter().any(|(id, _)| *id == note),
        "its card is drawn"
    );
    // Notes are still written there.
    write_note(&mut app, |app| app.key(Key::T), "Call Dana");
    assert_eq!(labels(&app), ["Thread note", "Thread note"]);
}

#[test]
fn done_and_trash_leave_a_notes_only_thread_alone() {
    let (mut app, thread, subject) = deleted_result("notes-deleted-triage");
    app.key(Key::Enter);
    assert_eq!(app.mail().notes_alone(), Some(subject.clone()));
    app.wait(0.5);
    let units = app.units();
    app.key(Key::E);
    assert_eq!(app.mail().toast_text(), None, "nothing was tried");
    type_key(&mut app, Key::Num3, Modifiers::SHIFT, "#");
    assert_eq!(app.mail().toast_text(), None, "nothing was tried");
    app.wait(0.5);
    assert_eq!(app.units(), units, "Gmail was asked nothing");
    assert_eq!(
        app.mail().thread().map(|open| open.id()),
        Some(thread.as_str()),
        "still open"
    );
    assert_eq!(app.mail().notes_alone(), Some(subject));
    app.key(Key::Escape);
    let row = app
        .mail()
        .inbox()
        .threads()
        .iter()
        .find(|row| row.id == thread)
        .cloned()
        .expect("still listed");
    assert!(!row.done && !row.trashed, "{row:?}");
}

#[test]
fn done_on_a_deleted_result_does_nothing() {
    let (mut app, thread, _) = deleted_result("notes-deleted-row-done");
    assert_eq!(
        app.mail().inbox().cursor().map(|row| row.id.clone()),
        Some(thread.clone()),
        "the results have the keyboard"
    );
    app.wait(0.5);
    let units = app.units();
    app.key(Key::E);
    assert_eq!(app.mail().toast_text(), None, "nothing was tried");
    app.wait(0.5);
    assert_eq!(app.units(), units, "Gmail was asked nothing");
    let rows = app.mail().inbox().threads();
    assert_eq!(rows.len(), 1, "the row stays");
    assert!(!rows[0].done);
}

#[test]
fn a_note_whose_message_is_gone_stays_in_place() {
    let mut app = start("notes-message-gone");
    open_debrief(&mut app);
    app.key(Key::P);
    let (ids, from) = {
        let thread = app.mail().thread().expect("open");
        let ids: Vec<String> = thread
            .messages()
            .iter()
            .map(|message| message.id.clone())
            .collect();
        let from = thread.messages()[1]
            .from
            .as_ref()
            .and_then(|from| from.name.clone())
            .expect("a sender's name");
        (ids, from)
    };
    write_note(
        &mut app,
        |app| app.press(Key::T, Modifiers::SHIFT),
        "Between the two",
    );
    let note = app.mail().notes().notes[0].id.clone();
    let gmail = app.gmail();
    app.block_on(async { gmail.gmail().delete_message(&ids[1]).await })
        .expect("deleted in Gmail");
    app.sync();
    assert_eq!(
        app.mail().thread().expect("still open").messages().len(),
        2,
        "the message is gone"
    );
    assert_eq!(labels(&app), ["Note 1"], "the note keeps its number");
    let first = from.split_whitespace().next().expect("a first name");
    let said = app.mail().note_where(&note).expect("the note is drawn");
    assert!(
        said.starts_with(&format!("on a deleted email from {first} of ")),
        "{said}"
    );
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
    let (above, below) = (card(&ids[0]), card(&ids[2]));
    assert!(rect.top() >= above.bottom(), "{rect:?} {above:?}");
    assert!(rect.bottom() <= below.top(), "{rect:?} {below:?}");
}

/// The open thread's subject.
fn open_subject(app: &Fixture) -> String {
    app.mail()
        .thread()
        .and_then(|open| open.detail())
        .expect("a thread is loaded")
        .subject
        .clone()
}

#[test]
fn share_opens_a_draft_with_the_notes() {
    let mut app = start("notes-share");
    open_debrief(&mut app);
    let subject = open_subject(&app);
    write_note(&mut app, |app| app.key(Key::T), "Ask about the budget");
    write_note(
        &mut app,
        |app| app.press(Key::T, Modifiers::SHIFT),
        "Strong on systems",
    );
    let before = app.mail().notes().clone();
    palette(&mut app, "Share Notes by Email");
    assert!(app.mail().compose_popped_out(), "a draft pops out");
    let (to, said) = app.mail().compose_focus().expect("it has the keyboard");
    assert!(to.is_empty(), "{to:?}");
    assert_eq!(said, format!("Notes: {subject}"));
    assert_eq!(
        app.mail().compose_recipients(),
        Some([Vec::new(), Vec::new(), Vec::new()]),
        "no recipients"
    );
    let body = app.mail().compose_body().expect("a body");
    for text in [
        "Ask about the budget",
        "Strong on systems",
        "Thread note",
        "Note 1",
    ] {
        assert!(body.contains(text), "{text} in {body}");
    }
    assert!(
        body.find("Ask about the budget") < body.find("Strong on systems"),
        "in time order: {body}"
    );
    app.settle();
    assert_eq!(*app.mail().notes(), before, "the notes are unchanged");
}

#[test]
fn shared_files_ride_along_and_oversized_ones_are_listed() {
    let mut app = start("notes-share-files");
    app.key(Key::Enter);
    let thread = app.mail().thread().expect("open").id().to_owned();
    let folder = app.mail().account().note_files();
    std::fs::create_dir_all(&folder).expect("a folder");
    std::fs::write(folder.join("small-uuid"), b"1. Budget").expect("written");
    std::fs::write(folder.join("after-uuid"), b"2. Hiring").expect("written");
    std::fs::File::create(folder.join("big-uuid"))
        .and_then(|file| file.set_len(40 * 1024 * 1024))
        .expect("written");
    std::fs::write(folder.join("image-uuid"), png()).expect("written");
    let file = |uuid: &str, name: &str, size: u64, cid: Option<&str>| exo_store::NoteFile {
        uuid: uuid.to_owned(),
        name: name.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        size,
        content_id: cid.map(str::to_owned),
    };
    app.mail_mut().account_mut().save_note(SaveNote {
        draft_id: "6b0d3c55-6f43-4a8e-9a39-0c1f6f3f8a11".to_owned(),
        note_id: None,
        thread_id: ThreadId::new(thread),
        message_id: None,
        html: "<div>Files</div><div><img class=\"Exo-inline-image\" src-cid=\"board\"></div>"
            .to_owned(),
        text: "Files".to_owned(),
        files: vec![
            file("small-uuid", "agenda.txt", 9, None),
            file("big-uuid", "walkthrough.mov", 40 * 1024 * 1024, None),
            file("after-uuid", "after.txt", 9, None),
            file("image-uuid", "board.png", png().len() as u64, Some("board")),
        ],
    });
    app.settle();
    palette(&mut app, "Share Notes by Email");
    assert_eq!(
        app.mail().compose_attachments(),
        ["agenda.txt"],
        "the file that fits is attached"
    );
    let body = app.mail().compose_body().expect("a body");
    assert!(
        body.contains("Not attached (too large)") && body.contains("walkthrough.mov"),
        "{body}"
    );
    assert!(
        body.contains("after.txt"),
        "and every file after the one that didn't fit: {body}"
    );
    assert!(body.contains("board"), "the image is shown inline: {body}");
    assert_eq!(app.mail().compose_alert(), None);
}

#[test]
fn sharing_nothing_says_so() {
    let mut app = start("notes-share-nothing");
    app.key(Key::Enter);
    palette(&mut app, "Share Notes by Email");
    assert_eq!(app.mail().toast_text(), Some("No notes to share."));
    assert!(!app.mail().compose_popped_out(), "no draft");
}

#[test]
fn notes_on_a_deleted_thread_are_shared_too() {
    let (mut app, _, subject) = deleted_result("notes-share-deleted");
    app.key(Key::Enter);
    palette(&mut app, "Share Notes by Email");
    let (_, said) = app
        .mail()
        .compose_focus()
        .expect("a draft has the keyboard");
    assert_eq!(said, format!("Notes: {subject}"));
    let body = app.mail().compose_body().expect("a body");
    assert!(body.contains("Ask about the overage fee"), "{body}");
}

/// Writes `First`, then edits it to `First and second`, and returns the note's id.
fn note_with_two_versions(name: &str) -> (Fixture, String) {
    let mut app = start_thread_note(name);
    app.type_text("First");
    app.press(Key::Enter, COMMAND);
    let id = app.mail().notes().notes[0].id.clone();
    click_link(&mut app, &id, CardAction::Edit);
    app.type_text(" and second");
    app.press(Key::Enter, COMMAND);
    assert_eq!(app.mail().notes().notes[0].version, 2);
    (app, id)
}

#[test]
fn history_lists_every_save_newest_first() {
    use exo_store::NoteChange;
    let (mut app, id) = note_with_two_versions("notes-history-lists");
    assert_eq!(app.mail().note_history(), None, "no history yet");
    click_link(&mut app, &id, CardAction::History);
    assert_eq!(
        app.mail().note_history(),
        Some(vec![(2, NoteChange::Edited), (1, NoteChange::Created)])
    );
    assert!(
        !app.mail()
            .note_controls()
            .iter()
            .any(|(control, _)| *control == Control::RestoreVersion),
        "the newest can't be restored"
    );
    // ↓ selects the older version, which can be.
    app.key(Key::ArrowDown);
    assert!(
        app.mail()
            .note_controls()
            .iter()
            .any(|(control, _)| *control == Control::RestoreVersion)
    );
    // Single-key shortcuts don't reach the thread, and Escape closes the history alone.
    let thread = app.mail().thread().expect("open").id().to_owned();
    app.key(Key::E);
    app.key(Key::Escape);
    assert_eq!(app.mail().note_history(), None, "Escape closed it");
    assert_eq!(
        app.mail().thread().map(|open| open.id()),
        Some(thread.as_str()),
        "the thread is still open"
    );
    // T closes it too, for the editor, while the history has the keyboard.
    click_link(&mut app, &id, CardAction::History);
    assert!(app.mail().note_history().is_some());
    app.key(Key::T);
    assert_eq!(app.mail().note_history(), None, "T closed it");
    assert_eq!(app.mail().note_editor_text().as_deref(), Some(""));
}

#[test]
fn restoring_a_version_brings_its_text_back_as_a_new_version() {
    use exo_store::NoteChange;
    let (mut app, id) = note_with_two_versions("notes-history-restore");
    click_link(&mut app, &id, CardAction::History);
    app.key(Key::ArrowDown);
    click_control(&mut app, &Control::RestoreVersion);
    assert_eq!(
        app.mail().note_history(),
        None,
        "restoring closes the history"
    );
    assert_eq!(app.mail().toast_text(), Some("Version restored."));
    let note = &app.mail().notes().notes[0];
    assert_eq!(note.id, id);
    assert_eq!(note.text, "First");
    assert_eq!(note.version, 3);
    click_link(&mut app, &id, CardAction::History);
    assert_eq!(
        app.mail().note_history(),
        Some(vec![
            (3, NoteChange::Restored),
            (2, NoteChange::Edited),
            (1, NoteChange::Created)
        ])
    );
    click_control(&mut app, &Control::CloseHistory);
    assert_eq!(app.mail().note_history(), None, "Close closes it");
}

#[test]
fn opening_a_history_saves_the_open_editor() {
    let (mut app, id) = note_with_two_versions("notes-history-saves-editor");
    app.key(Key::T);
    app.type_text("Another");
    click_link(&mut app, &id, CardAction::History);
    assert_eq!(app.mail().note_editor_text(), None, "the editor closed");
    assert_eq!(
        app.mail().note_history().map(|list| list.len()),
        Some(2),
        "the history is open"
    );
    let mut texts: Vec<String> = app
        .mail()
        .notes()
        .notes
        .iter()
        .map(|note| note.text.clone())
        .collect();
    texts.sort();
    assert_eq!(
        texts,
        ["Another", "First and second"],
        "the editor's note saved"
    );
}

#[test]
fn a_history_opens_in_the_pane_with_notes_in_the_pane_only() {
    let mut app = start_with_settings("notes-history-pane", &|settings| {
        settings.notes_placement = NotesPlacement::Pane;
    });
    app.key(Key::Enter);
    write_note(&mut app, |app| app.key(Key::T), "In the pane");
    let id = app.mail().notes().notes[0].id.clone();
    click_link(&mut app, &id, CardAction::History);
    assert_eq!(app.mail().note_history().map(|list| list.len()), Some(1));
    let pane = app.mail().right_pane();
    let (_, rect) = app.mail().note_rects()[0];
    assert!(pane.contains_rect(rect), "in its card's place: {rect:?}");
    app.key(Key::Escape);
    assert_eq!(app.mail().note_history(), None);
}

#[test]
fn a_deleted_note_can_be_restored_from_the_pane() {
    let mut app = start_thread_note("notes-restore-deleted");
    app.type_text("Bring me back");
    app.press(Key::Enter, COMMAND);
    let id = app.mail().notes().notes[0].id.clone();
    app.press(Key::N, COMMAND_SHIFT);
    assert!(
        !app.mail()
            .note_controls()
            .iter()
            .any(|(control, _)| *control == Control::DeletedNotes),
        "nothing deleted, nothing listed"
    );
    click_link(&mut app, &id, CardAction::Delete);
    assert!(app.mail().notes().notes.is_empty());
    // Collapsed at first.
    assert!(
        !app.mail()
            .note_controls()
            .iter()
            .any(|(control, _)| *control == Control::RestoreDeleted(id.clone()))
    );
    click_control(&mut app, &Control::DeletedNotes);
    click_control(&mut app, &Control::RestoreDeleted(id.clone()));
    assert_eq!(app.mail().toast_text(), Some("Note restored."));
    let notes = app.mail().notes();
    assert_eq!(notes.notes.len(), 1);
    assert_eq!(notes.notes[0].id, id);
    assert_eq!(notes.notes[0].text, "Bring me back");
    assert!(notes.deleted.is_empty());
    assert!(
        !app.mail()
            .note_controls()
            .iter()
            .any(|(control, _)| *control == Control::DeletedNotes),
        "none left to list"
    );
}

#[test]
fn share_saves_the_open_editor_first() {
    let mut app = start("notes-share-saves-editor");
    app.key(Key::Enter);
    write_note(&mut app, |app| app.key(Key::T), "Ask about the budget");
    let id = app.mail().notes().notes[0].id.clone();
    click_link(&mut app, &id, CardAction::Edit);
    app.type_text(" and the timeline");
    palette(&mut app, "Share Notes by Email");
    assert_eq!(app.mail().note_editor_text(), None, "the editor closed");
    assert_eq!(
        app.mail().notes().notes[0].text,
        "Ask about the budget and the timeline",
        "and its text was saved"
    );
    let body = app.mail().compose_body().expect("a draft with a body");
    assert!(
        body.contains("Ask about the budget and the timeline"),
        "what was on screen is shared: {body}"
    );
}

#[test]
fn share_includes_a_new_note_being_written() {
    let mut app = start_thread_note("notes-share-new-note");
    app.type_text("Not saved yet");
    palette(&mut app, "Share Notes by Email");
    assert_eq!(labels(&app), ["Thread note"]);
    let body = app.mail().compose_body().expect("a draft with a body");
    assert!(body.contains("Not saved yet"), "{body}");
}

#[test]
fn a_long_history_scrolls_and_keeps_the_selection_in_view() {
    let mut app = start_thread_note("notes-history-long");
    app.type_text("Version 1");
    app.press(Key::Enter, COMMAND);
    let note = app.mail().notes().notes[0].clone();
    for n in 2..=11 {
        app.mail_mut().account_mut().save_note(SaveNote {
            draft_id: note.id.clone(),
            note_id: Some(note.id.clone()),
            thread_id: note.thread_id.clone(),
            message_id: None,
            html: format!("<div>Version {n}</div>"),
            text: format!("Version {n}"),
            files: Vec::new(),
        });
        app.settle();
    }
    assert_eq!(app.mail().notes().notes[0].version, 11);
    click_link(&mut app, &note.id, CardAction::History);
    assert_eq!(app.mail().note_history().map(|list| list.len()), Some(11));
    let rows = |app: &Fixture| -> Vec<u32> {
        app.mail()
            .note_controls()
            .iter()
            .filter_map(|(control, rect)| match control {
                Control::Version(version) if rect.height() > 0.0 => Some(*version),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        rows(&app),
        (4..=11).rev().collect::<Vec<_>>(),
        "8 rows show"
    );
    for _ in 0..10 {
        app.key(Key::ArrowDown);
    }
    let shown = rows(&app);
    assert!(
        shown.contains(&1),
        "the oldest, selected, is in view: {shown:?}"
    );
    assert!(!shown.contains(&11), "the newest scrolled away: {shown:?}");
    assert_eq!(shown.len(), 8);
}

/// Starts an empty bulleted list in the open note editor: blank text the editor doesn't count as
/// empty, which the store doesn't keep.
fn blank_list(app: &mut Fixture) {
    app.press(Key::Num8, COMMAND_SHIFT);
    app.type_text("  ");
}

#[test]
fn share_with_a_blank_new_note_goes_ahead() {
    let mut app = start_thread_note("notes-share-blank-new");
    blank_list(&mut app);
    palette(&mut app, "Share Notes by Email");
    assert_eq!(app.mail().note_editor_text(), None, "the editor closed");
    assert_eq!(app.mail().toast_text(), Some("No notes to share."));
    write_note(&mut app, |app| app.key(Key::T), "Saved before");
    app.key(Key::T);
    blank_list(&mut app);
    palette(&mut app, "Share Notes by Email");
    assert_eq!(labels(&app), ["Thread note"], "the blank note isn't kept");
    let body = app.mail().compose_body().expect("a draft with a body");
    assert!(body.contains("Saved before"), "{body}");
}

/// Keeps the Markdown copy of the notes in `folder` from now on, as the Notes dialog does.
fn keep_markdown_in(app: &mut Fixture, folder: &std::path::Path) {
    let folder = folder.display().to_string();
    app.mail_mut()
        .account_mut()
        .update_device_settings(|device| device.notes_folder = Some(folder));
    app.settle();
}

/// The text of the file in `folder` whose name ends with `suffix`, if there's one.
fn file_ending(folder: &std::path::Path, suffix: &str) -> Option<String> {
    let entries = std::fs::read_dir(folder).ok()?;
    entries
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().ends_with(suffix))
        .map(|entry| std::fs::read_to_string(entry.path()).expect("readable"))
}

/// Saves a thread note saying `text` on `thread`, through the account.
fn save_thread_note(app: &mut Fixture, thread: &str, id: &str, text: &str) {
    app.mail_mut().account_mut().save_note(SaveNote {
        draft_id: id.to_owned(),
        note_id: None,
        thread_id: ThreadId::new(thread),
        message_id: None,
        html: format!("<div>{text}</div>"),
        text: text.to_owned(),
        files: Vec::new(),
    });
    app.settle();
}

#[test]
fn saving_a_note_writes_its_markdown_file_when_the_folder_is_on() {
    let mut app = start("notes-folder-save");
    let folder = app.folder("Email Notes");
    keep_markdown_in(&mut app, &folder);
    app.key(Key::Enter);
    let thread = app.mail().thread().expect("open").id().to_owned();
    write_note(&mut app, |app| app.key(Key::T), "Ask about the budget");
    let markdown = file_ending(&folder, &format!("-{thread}.md")).expect("the thread's file");
    assert!(
        markdown.starts_with(&format!("---\nthread_id: \"{thread}\"\n")),
        "{markdown}"
    );
    assert!(
        markdown.contains("\n## Thread note\n") && markdown.contains("\nAsk about the budget\n"),
        "{markdown}"
    );
    let history =
        file_ending(&folder, &format!("-{thread}.history.jsonl")).expect("the thread's history");
    assert_eq!(history.lines().count(), 1, "{history}");
    assert!(history.contains("\"change\":\"created\""), "{history}");
    // Deleting the thread's only note takes its file away, and the history stays.
    let id = app.mail().notes().notes[0].id.clone();
    app.mail_mut().account_mut().delete_note(id);
    app.settle();
    assert_eq!(file_ending(&folder, &format!("-{thread}.md")), None);
    let history =
        file_ending(&folder, &format!("-{thread}.history.jsonl")).expect("the thread's history");
    assert_eq!(history.lines().count(), 2, "{history}");
}

#[test]
fn no_markdown_is_written_while_the_folder_is_off() {
    let mut app = start("notes-folder-off");
    app.key(Key::Enter);
    write_note(&mut app, |app| app.key(Key::T), "Ask about the budget");
    assert_eq!(app.mail().account().device_settings().notes_folder, None);
    assert!(
        !app.folder("Email Notes").exists(),
        "nothing is written to a folder"
    );
}

#[test]
fn rebuild_writes_every_thread() {
    let mut app = start("notes-folder-rebuild");
    app.key(Key::Enter);
    let first = app.mail().thread().expect("open").id().to_owned();
    app.key(Key::J);
    let second = app.mail().thread().expect("open").id().to_owned();
    assert_ne!(first, second);
    save_thread_note(
        &mut app,
        &first,
        "6b0d3c55-6f43-4a8e-9a39-0c1f6f3f8a11",
        "First",
    );
    save_thread_note(
        &mut app,
        &second,
        "0f1e2d3c-4b5a-4968-8776-655443322110",
        "Second",
    );
    let folder = app.folder("Email Notes");
    // Turning the copy on writes every thread's notes.
    keep_markdown_in(&mut app, &folder);
    for (thread, text) in [(&first, "First"), (&second, "Second")] {
        let markdown = file_ending(&folder, &format!("-{thread}.md")).expect("written");
        assert!(markdown.contains(text), "{markdown}");
    }
    std::fs::remove_dir_all(&folder).expect("removed");
    palette(&mut app, "Rebuild Notes Folder");
    assert_eq!(app.mail().toast_text(), Some("Notes folder rebuilt."));
    for (thread, text) in [(&first, "First"), (&second, "Second")] {
        let markdown = file_ending(&folder, &format!("-{thread}.md")).expect("written again");
        assert!(markdown.contains(text), "{markdown}");
    }
}

#[test]
fn a_folder_that_cannot_be_written_says_so_once() {
    let mut app = start("notes-folder-unwritable");
    let blocker = app.folder("blocker");
    std::fs::create_dir_all(blocker.parent().expect("a parent")).expect("a folder");
    std::fs::write(&blocker, b"a file, not a folder").expect("written");
    keep_markdown_in(&mut app, &blocker.join("Email Notes"));
    assert_eq!(
        app.mail().toast_text(),
        Some("Couldn't write the notes folder.")
    );
    app.wait(10.0);
    assert_eq!(app.mail().toast_text(), None, "the toast is gone");
    app.key(Key::Enter);
    write_note(&mut app, |app| app.key(Key::T), "Ask about the budget");
    assert_eq!(labels(&app), ["Thread note"], "the note is saved");
    assert_eq!(app.mail().toast_text(), None, "said once a session");
}

#[test]
fn a_new_note_whose_only_image_was_taken_out_shares_at_once() {
    let mut app = start_thread_note("notes-image-only-undone");
    // A blank, so the editor's text differs from an empty note's.
    app.type_text(" ");
    paste_image(&mut app, png());
    app.press(Key::Z, COMMAND);
    assert_eq!(app.mail().note_editor_text().as_deref(), Some(" "));
    palette(&mut app, "Share Notes by Email");
    assert_eq!(
        app.mail().toast_text(),
        Some("No notes to share."),
        "the share doesn't wait on a note that isn't saved"
    );
    assert!(app.mail().notes().notes.is_empty());
}
