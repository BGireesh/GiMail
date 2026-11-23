//! Thread notes, end to end: notes saved through the account reach the open thread's snapshot.

mod support;

use egui::Key;
use exo_store::{SaveNote, ThreadId};
use support::start;

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
