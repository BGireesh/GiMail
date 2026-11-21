//! The shell's own keys end to end: zoom, back and forward, and the rail's back area.

mod support;

use egui::{CursorIcon, Key, Modifiers, Pos2};
use exo_store::ListId;
use support::start;

const COMMAND: Modifiers = Modifiers::MAC_CMD.plus(Modifiers::COMMAND);
/// The rail under Mail and Calendar, at 1280×800.
const RAIL_BACK: Pos2 = Pos2::new(20.0, 400.0);

#[test]
fn command_equals_zooms_by_chromiums_steps_and_command_zero_goes_back() {
    let mut app = start("zoom");
    app.press(Key::Equals, COMMAND);
    assert_eq!(app.harness.ctx.zoom_factor(), 1.2);
    app.press(Key::Equals, COMMAND);
    assert!((app.harness.ctx.zoom_factor() - 1.44).abs() < 1e-5);
    // ⌘⇧= isn't one of the shell's keys, nor is egui's own zoom on.
    app.press(Key::Equals, COMMAND | Modifiers::SHIFT);
    assert!((app.harness.ctx.zoom_factor() - 1.44).abs() < 1e-5);
    app.press(Key::Num0, COMMAND);
    assert_eq!(app.harness.ctx.zoom_factor(), 1.0);
    app.press(Key::Minus, COMMAND);
    assert!((app.harness.ctx.zoom_factor() - 1.0 / 1.2).abs() < 1e-5);
}

#[test]
fn back_and_forward_walk_lists_and_conversations() {
    let mut app = start("history");
    let home = app.mail().list().clone();
    app.key(Key::Enter);
    let opened = app
        .mail()
        .thread()
        .map(|thread| thread.id().to_owned())
        .expect("a conversation opens");
    // Escape goes back to where it was opened from, so forward opens it again.
    app.key(Key::Escape);
    assert!(app.mail().thread().is_none());
    app.press(Key::CloseBracket, COMMAND);
    assert_eq!(
        app.mail().thread().map(|thread| thread.id().to_owned()),
        Some(opened)
    );
    app.press(Key::OpenBracket, COMMAND);
    assert!(app.mail().thread().is_none(), "back to the list");
    // Lists too: ⌘← is ⌘[.
    app.key(Key::G);
    app.key(Key::T);
    assert_eq!(*app.mail().list(), ListId::Sent);
    app.press(Key::ArrowLeft, COMMAND);
    assert_eq!(*app.mail().list(), home);
    app.press(Key::ArrowRight, COMMAND);
    assert_eq!(*app.mail().list(), ListId::Sent);
}

#[test]
fn back_and_forward_go_through_search_the_week_and_filters() {
    let mut app = start("history-views");
    let home = app.mail().list().clone();
    app.key(Key::Slash);
    app.type_text("potato");
    app.key(Key::Enter);
    // Escape selects the bar's text, which the next search replaces.
    app.key(Key::Escape);
    app.type_text("thanksgiving");
    app.key(Key::Enter);
    assert_eq!(app.mail().search_query(), Some("thanksgiving"));
    // From the results, back to the first search, then out of search.
    app.press(Key::OpenBracket, COMMAND);
    assert_eq!(app.mail().search_query(), Some("potato"));
    app.press(Key::OpenBracket, COMMAND);
    assert!(!app.mail().is_searching());
    assert_eq!(*app.mail().list(), home);
    app.press(Key::CloseBracket, COMMAND);
    assert_eq!(app.mail().search_query(), Some("potato"));
    // Leaving search goes back to where it was opened.
    app.key(Key::Escape);
    app.key(Key::Escape);
    assert!(!app.mail().is_searching());

    // The week: ⌘[ closes it, ⌘] opens it again.
    app.key(Key::Num2);
    let week = app.mail().calendar_week();
    assert!(week.is_some());
    app.press(Key::OpenBracket, COMMAND);
    assert_eq!(app.mail().calendar_week(), None);
    app.press(Key::CloseBracket, COMMAND);
    assert_eq!(app.mail().calendar_week(), week);
    app.key(Key::Num1);

    // A filter, and back to the list without it.
    app.press(Key::U, Modifiers::SHIFT);
    assert_eq!(app.mail().filter(), Some(exo_store::Filter::Unread));
    app.press(Key::OpenBracket, COMMAND);
    assert_eq!(app.mail().filter(), None);
    app.press(Key::CloseBracket, COMMAND);
    assert_eq!(app.mail().filter(), Some(exo_store::Filter::Unread));
}

#[test]
fn a_search_opened_from_another_list_is_a_place_of_its_own() {
    let mut app = start("history-prefilled");
    app.key(Key::G);
    app.key(Key::T);
    // Sent fills the bar in.
    app.key(Key::Slash);
    assert_eq!(app.mail().search_text(), Some("in:sent "));
    app.type_text("budget");
    app.key(Key::Enter);
    assert_eq!(app.mail().search_query(), Some("in:sent budget"));
    // Back to the search Sent opened, which runs, then back to Sent.
    app.press(Key::OpenBracket, COMMAND);
    assert_eq!(app.mail().search_query(), Some("in:sent "));
    app.press(Key::OpenBracket, COMMAND);
    assert!(!app.mail().is_searching());
    assert_eq!(*app.mail().list(), ListId::Sent);
    // Forward to that search again.
    app.press(Key::CloseBracket, COMMAND);
    assert_eq!(app.mail().search_query(), Some("in:sent "));
    app.press(Key::CloseBracket, COMMAND);
    assert_eq!(app.mail().search_query(), Some("in:sent budget"));
}

#[test]
fn back_does_nothing_while_typing() {
    let mut app = start("history-typing");
    app.key(Key::G);
    app.key(Key::T);
    app.key(Key::C);
    // ⌘[ in a new message's body is the editor's (Decrease Indent), not back.
    app.key(Key::Tab);
    app.key(Key::Tab);
    app.press(Key::OpenBracket, COMMAND);
    assert_eq!(*app.mail().list(), ListId::Sent);
}

#[test]
fn the_rail_under_the_switcher_goes_back() {
    let mut app = start("rail-back");
    let home = app.mail().list().clone();
    // On the home list, with nothing open, there's nowhere to go back to.
    assert_ne!(app.hover(RAIL_BACK), CursorIcon::PointingHand);
    app.key(Key::Enter);
    assert_eq!(app.hover(RAIL_BACK), CursorIcon::PointingHand);
    app.click(RAIL_BACK);
    assert!(app.mail().thread().is_none(), "out of the conversation");
    app.key(Key::G);
    app.key(Key::T);
    app.click(RAIL_BACK);
    assert_eq!(*app.mail().list(), home, "home from another list");
    app.key(Key::Num2);
    assert!(app.mail().calendar_week().is_some());
    app.click(RAIL_BACK);
    assert_eq!(app.mail().calendar_week(), None, "out of the week view");
}
