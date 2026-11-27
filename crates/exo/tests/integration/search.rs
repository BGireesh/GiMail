//! Search and filters driven by keys, end to end: mock-servers, a fresh store, the bar.

use crate::support;

use egui::{Key, Modifiers};
use exo_store::Filter;
use support::start;

#[test]
fn slash_searches_and_escape_backs_out_step_by_step() {
    let mut app = start("search");
    app.key(Key::Slash);
    assert!(app.mail().is_searching());
    assert!(app.subjects().is_empty(), "an empty search shows no rows");
    app.type_text("potato");
    app.key(Key::Enter);
    assert_eq!(app.mail().search_query(), Some("potato"));
    assert_eq!(app.subjects(), ["Thanksgiving 🦃"]);
    assert!(
        !app.mail().search_bar_focused(),
        "the results take the keyboard"
    );
    let row = &app.mail().inbox().threads()[0];
    assert!(row.messages > 1);
    let marks = row.search.as_ref().expect("a search row");
    assert_eq!(
        marks
            .snippet
            .iter()
            .map(|range| &row.snippet[range.clone()])
            .collect::<Vec<_>>(),
        ["potato"]
    );

    // Escape gives the bar the keyboard, then (with no suggestions open) leaves search.
    app.key(Key::Escape);
    assert!(app.mail().search_bar_focused());
    app.key(Key::Escape);
    assert!(!app.mail().is_searching());
    assert_eq!(app.subjects().len(), 24, "back to the inbox");
}

#[test]
fn a_pause_in_typing_runs_the_search_ahead_for_enter() {
    let mut app = start("search-instant");
    app.key(Key::Slash);
    // Typing settles for longer than the pause.
    app.type_text("kitchen");
    assert_eq!(app.mail().search_query(), None, "nothing shows yet");
    // Enter shows what Gmail answered then (a page, all it has), and Gmail isn't asked again.
    app.disconnect();
    app.key(Key::Enter);
    assert_eq!(app.subjects(), ["Offsite catering — Hudson, Dec 9–11"]);
    let snapshot = app.mail().account().snapshot();
    let results = snapshot.search.as_ref().expect("results");
    assert!(results.finished && !results.offline, "{results:?}");
}

#[test]
fn escape_then_slash_replaces_the_search() {
    let mut app = start("search-escape-slash");
    app.key(Key::Slash);
    app.type_text("offsite");
    app.key(Key::Enter);
    assert!(!app.mail().inbox().threads().is_empty());
    app.key(Key::Escape);
    assert!(app.mail().search_bar_focused());
    assert!(
        app.mail().inbox().cursor().is_none(),
        "the results lose their cursor while the bar has the keyboard"
    );
    app.type_text("/");
    assert_eq!(
        app.mail().search_text(),
        Some("/"),
        "Escape selected the search, which `/` replaces"
    );
}

#[test]
fn an_empty_search_says_so_and_keeps_the_keyboard() {
    let mut app = start("search-none");
    app.key(Key::Slash);
    app.type_text("zzqx");
    app.key(Key::Enter);
    assert!(app.subjects().is_empty());
    assert!(
        app.mail().search_bar_focused(),
        "focus waits for a row that never comes"
    );
}

#[test]
fn opening_a_result_remembers_the_search() {
    let mut app = start("search-history");
    app.key(Key::Slash);
    app.type_text("potato");
    app.key(Key::Enter);
    app.key(Key::Enter);
    assert!(app.mail().thread().is_some(), "the result opens");
    let history = &app.mail().account().settings().search_history;
    assert_eq!(
        history
            .iter()
            .map(|item| item.value.as_str())
            .collect::<Vec<_>>(),
        ["potato"]
    );
}

#[test]
fn shift_u_filters_the_inbox_and_escape_clears_it() {
    let mut app = start("filter");
    app.press(Key::U, Modifiers::SHIFT);
    assert_eq!(app.mail().filter(), Some(Filter::Unread));
    let unread = app.subjects();
    assert!(!unread.is_empty() && unread.len() < 24);
    assert!(
        app.mail()
            .inbox()
            .threads()
            .iter()
            .all(|thread| thread.unread)
    );
    // Read rows stay while the filter is on (sticky rows).
    app.key(Key::U);
    assert_eq!(app.subjects(), unread);
    app.key(Key::Escape);
    assert_eq!(app.mail().filter(), None);
    assert_eq!(app.subjects().len(), 24);
}

#[test]
fn a_filter_becomes_a_search_token() {
    let mut app = start("filter-search");
    // Search opens over a filtered list with the filter's token.
    app.press(Key::S, Modifiers::SHIFT);
    app.key(Key::Slash);
    assert_eq!(app.mail().search_text(), Some("is:starred "));
    app.key(Key::Escape);
    assert!(!app.mail().is_searching());
    app.key(Key::Escape);
    assert_eq!(app.mail().filter(), None, "Escape clears the filter");
    // In search, ⇧U adds `is:unread` to the query and runs it.
    app.key(Key::Slash);
    app.type_text("potato");
    app.key(Key::Enter);
    app.press(Key::U, Modifiers::SHIFT);
    assert_eq!(app.mail().search_query(), Some("potato is:unread "));
    app.press(Key::U, Modifiers::SHIFT);
    assert_eq!(app.mail().search_query(), Some("potato "));
}

#[test]
fn tab_completes_an_operator_and_a_label() {
    let mut app = start("search-tab");
    app.key(Key::Slash);
    app.type_text("fr");
    app.key(Key::Tab);
    assert_eq!(app.mail().search_text(), Some("from:"));
    app.type_text("emekao@gmail.com");
    app.key(Key::Enter);
    assert!(
        app.subjects().contains(&"Thanksgiving 🦃".to_owned()),
        "{:?}",
        app.subjects()
    );
    app.key(Key::Escape);
    app.press(Key::A, Modifiers::COMMAND);
    app.type_text("in:fam");
    app.key(Key::Tab);
    assert_eq!(app.mail().search_text(), Some("in:family "));
}

#[test]
fn a_copied_subject_pastes_into_search_as_its_search() {
    let mut app = start("search-paste-subject");
    exo::clipboard::capture(&app.harness.ctx);
    app.key(Key::Enter);
    app.press(Key::K, Modifiers::MAC_CMD.plus(Modifiers::COMMAND));
    app.type_text("Copy Subject");
    app.key(Key::Enter);
    let subject = app
        .mail()
        .thread()
        .and_then(|thread| thread.detail())
        .map(|detail| detail.subject.clone())
        .expect("an open thread");
    let search = format!("subject:\"{subject}\"");
    assert_eq!(
        exo::clipboard::captured_search(&app.harness.ctx),
        Some((subject.clone(), search.clone()))
    );

    // `/`, then the paste egui makes of ⌘V, which is the subject's text.
    app.key(Key::Slash);
    app.type_text("from:me ");
    app.harness
        .input_mut()
        .events
        .push(egui::Event::Paste(subject.clone()));
    app.harness.run_steps(1);
    assert_eq!(
        app.mail().search_text(),
        Some(format!("from:me {search}").as_str())
    );

    // After `subject:`, the text alone.
    app.press(Key::A, Modifiers::MAC_CMD.plus(Modifiers::COMMAND));
    app.type_text("subject:\"");
    app.harness
        .input_mut()
        .events
        .push(egui::Event::Paste(subject.clone()));
    app.harness.run_steps(1);
    assert_eq!(
        app.mail().search_text(),
        Some(format!("subject:\"{subject}").as_str())
    );
}

const COMMAND: Modifiers = Modifiers::MAC_CMD.plus(Modifiers::COMMAND);

/// Writes a thread note holding `lines` in the open conversation, and saves it.
fn write_thread_note(app: &mut support::Fixture, lines: &[&str]) {
    app.key(Key::T);
    for (at, line) in lines.iter().enumerate() {
        if at > 0 {
            app.key(Key::Enter);
        }
        app.type_text(line);
    }
    app.press(Key::Enter, COMMAND);
}

/// Searches `query` from the list, and lets the results take the keyboard.
fn search_for(app: &mut support::Fixture, query: &str) {
    app.key(Key::Slash);
    app.type_text(query);
    app.key(Key::Enter);
}

#[test]
fn a_notes_search_shows_the_matching_note_under_the_row() {
    let mut app = start("search-notes-under-row");
    app.key(Key::Enter);
    let thread = app.mail().thread().expect("open").id().to_owned();
    let matching = [
        "Ask about the overage fee",
        "Overage was 12% last year",
        "No overage next time",
    ];
    for text in matching {
        write_thread_note(&mut app, &[text]);
    }
    write_thread_note(&mut app, &["Nothing to see here"]);
    app.key(Key::Escape);
    search_for(&mut app, "notes:overage");
    let rows = app.mail().inbox().threads();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        [thread.as_str()],
        "only the noted conversation"
    );
    let hits = app.mail().search_note_hits(0);
    assert_eq!(hits.len(), 3, "two notes and the rest counted: {hits:?}");
    let mut shown = Vec::new();
    for hit in &hits[..2] {
        assert!(hit.starts_with("Thread note · "), "{hit}");
        let (_, snippet) = hit.split_once(" — ").expect("a snippet after the label");
        assert!(matching.contains(&snippet), "{hit}");
        shown.push(snippet);
    }
    assert_ne!(shown[0], shown[1], "two different notes");
    assert_eq!(hits[2], "+1 more");
}

#[test]
fn opening_a_result_reveals_the_note() {
    let mut app = start("search-notes-reveal");
    app.key(Key::Enter);
    let thread = app.mail().thread().expect("open").id().to_owned();
    // Tall notes first, then the one to find, written later, so it comes last.
    for _ in 0..6 {
        write_thread_note(&mut app, &["One", "Two", "Three", "Four", "Five", "Six"]);
    }
    let written = app.mail().notes().notes[0].created_at;
    app.set_now((written + jiff::SignedDuration::from_mins(1)).to_zoned(jiff::tz::TimeZone::UTC));
    write_thread_note(&mut app, &["Ask the zebra about it"]);
    let id = app
        .mail()
        .notes()
        .notes
        .iter()
        .find(|note| note.text.contains("zebra"))
        .map(|note| note.id.clone())
        .expect("saved");
    app.key(Key::Escape);
    search_for(&mut app, "notes:zebra");
    app.key(Key::Enter);
    assert_eq!(
        app.mail().thread().map(|open| open.id()),
        Some(thread.as_str())
    );
    assert_eq!(
        app.mail().flashing_note(),
        Some(id.as_str()),
        "the note flashes"
    );
    assert_eq!(app.mail().note_highlights(), ["zebra"]);
    app.wait(1.2);
    let content = app.mail().thread().expect("open").messages_rect();
    let (_, card) = app
        .mail()
        .note_rects()
        .into_iter()
        .find(|(note, _)| *note == id)
        .expect("the card is drawn");
    assert!(
        content.contains_rect(card),
        "in view: {card:?} in {content:?}"
    );

    // Opened any other way, nothing is highlighted.
    app.key(Key::Escape);
    app.key(Key::Escape);
    app.key(Key::Escape);
    assert!(!app.mail().is_searching());
    app.key(Key::Enter);
    assert!(app.mail().thread().is_some());
    assert!(app.mail().note_highlights().is_empty());
}

#[test]
fn opening_a_result_reveals_the_note_in_the_pane() {
    let mut app = support::start_with_settings("search-notes-reveal-pane", &|settings| {
        settings.notes_placement = exo_store::NotesPlacement::Pane;
    });
    app.key(Key::Enter);
    write_thread_note(&mut app, &["Ask the zebra about it"]);
    let id = app.mail().notes().notes[0].id.clone();
    // The pane shows the contact again before the search.
    app.press(Key::N, COMMAND.plus(Modifiers::SHIFT));
    assert!(!app.mail().notes_tab());
    app.key(Key::Escape);
    search_for(&mut app, "zebra");
    app.key(Key::Enter);
    assert!(app.mail().notes_tab(), "the pane shows the notes");
    assert_eq!(app.mail().flashing_note(), Some(id.as_str()));
    let pane = app.mail().right_pane();
    assert!(
        app.mail()
            .note_rects()
            .iter()
            .any(|(note, rect)| *note == id && pane.contains_rect(*rect)),
        "in the pane"
    );
    assert_eq!(app.mail().note_highlights(), ["zebra"]);
}

#[test]
fn autocomplete_offers_notes() {
    let mut app = start("search-autocomplete-notes");
    app.key(Key::Slash);
    app.type_text("no");
    app.key(Key::Tab);
    assert_eq!(app.mail().search_text(), Some("notes:"));
    app.press(Key::A, COMMAND);
    app.type_text("has:");
    let suggested = app.mail().search_suggestions();
    assert!(
        suggested.iter().any(|value| value == "has:notes")
            && suggested.iter().any(|value| value == "has:attachment"),
        "{suggested:?}"
    );
}
