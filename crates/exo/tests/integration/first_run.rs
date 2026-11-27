//! The first-run flow after the welcome splash: account setup's loading screen and split step,
//! and what ends it.

use crate::support;

use egui::{Key, pos2};
use support::{Fixture, start_new_account};

/// Where to click: the Calendar split's row on the split step (the first after Important), its
/// switch, and `Continue with this setup`.
const CALENDAR_ROW: egui::Pos2 = pos2(300.0, 345.0);
const CALENDAR_SWITCH: egui::Pos2 = pos2(523.0, 355.0);
const CONTINUE: egui::Pos2 = pos2(341.0, 755.0);
/// The step's title.
const TITLE: egui::Pos2 = pos2(200.0, 125.0);

fn calendar_on(app: &Fixture) -> bool {
    app.mail()
        .account()
        .settings()
        .enabled_splits()
        .any(|split| split.kind.as_deref() == Some("calendar"))
}

/// Account setup on a new account, past its loading screen.
fn setup(name: &str) -> Fixture {
    let mut app = start_new_account(name);
    app.with_sample_split_library();
    app.mail_mut().open_account_setup();
    app.wait(1.0);
    assert!(app.mail().account_setup_open() && app.mail().first_run_covers());
    assert_eq!(
        app.mail().account_setup_splits(),
        None,
        "tailoring the inbox first"
    );
    // Four messages, two seconds each.
    app.wait(8.0);
    app
}

#[test]
fn account_setup_offers_the_splits_the_inbox_has_mail_for_then_gets_you_to_zero() {
    let mut app = setup("account-setup");
    // The mock's inbox has two invitations, and nothing from the sample library's other senders.
    assert_eq!(
        app.mail().account_setup_splits(),
        Some(vec![("Calendar".to_owned(), true)])
    );
    assert!(calendar_on(&app), "the split is on while it's offered");

    // Its row turns it off and on again.
    app.click(CALENDAR_ROW);
    assert_eq!(
        app.mail().account_setup_splits(),
        Some(vec![("Calendar".to_owned(), false)])
    );
    assert!(!calendar_on(&app));
    app.click(CALENDAR_ROW);
    assert!(calendar_on(&app));

    // The mail's keys don't get through, so 2 doesn't open the week.
    app.key(Key::Num2);
    assert_eq!(app.mail().calendar_week(), None);

    // After `Continue with this setup`, an inbox of more than ten ends in Get Me To Zero.
    app.click(CONTINUE);
    app.settle();
    let mail = app.mail();
    assert!(!mail.account_setup_open() && !mail.account().device_settings().account_setup_pending);
    assert!(calendar_on(&app), "the split stays on");
    assert!(
        !app.mail().account().settings().unified_inbox,
        "with Important and Other"
    );
    assert!(app.mail().get_me_to_zero_open() && !app.mail().welcome_alert_open());
}

#[test]
fn quitting_in_account_setup_opens_it_again() {
    let mut app = setup("account-setup-again");
    assert!(app.mail().account_setup_splits().is_some());
    app.relaunch();
    assert!(
        app.mail().account_setup_open(),
        "the next launch picks setup up again"
    );
    app.wait(9.0);
    assert_eq!(
        app.mail().account_setup_splits(),
        Some(vec![("Calendar".to_owned(), true)])
    );
    app.key(Key::Enter);
    app.relaunch();
    assert!(
        !app.mail().account_setup_open(),
        "once it's been through, never again"
    );
}

#[test]
fn account_setup_without_a_split_to_offer_ends_after_loading() {
    // With no library there's nothing to offer, so the split step is skipped.
    let mut app = start_new_account("account-setup-nothing");
    app.mail_mut().open_account_setup();
    app.wait(9.0);
    assert!(!app.mail().account_setup_open());
    assert!(app.mail().get_me_to_zero_open());
}

#[test]
fn the_welcome_splash_gives_way_to_account_setup() {
    let mut app = start_new_account("first-run-from-splash");
    app.with_sample_split_library();
    app.mail_mut().welcome_first_time();
    // Five seconds once its photo is up.
    for _ in 0..40 {
        if !app.mail().welcome_covers() {
            break;
        }
        app.wait(0.5);
    }
    let mail = app.mail();
    assert!(mail.account_setup_open() && mail.first_run_covers() && !mail.welcome_covers());
    assert!(
        !mail.account().settings().do_show_tutorial
            && mail.account().device_settings().account_setup_pending
    );
}

#[test]
fn space_on_the_content_scrolls_it_a_page_in_a_short_window() {
    let mut app = setup("account-setup-space");
    app.harness.set_size(egui::vec2(1280.0, 440.0));
    app.settle();
    assert_eq!(app.mail().account_setup_scroll(), Some(0.0));
    // A click on the title focuses the content, as a click anywhere on the page does.
    app.click(TITLE);
    app.key(Key::Space);
    app.wait(0.2);
    assert!(app.mail().account_setup_scroll() > Some(0.0), "a page down");
    app.press(Key::Space, egui::Modifiers::SHIFT);
    app.wait(0.2);
    assert_eq!(
        app.mail().account_setup_scroll(),
        Some(0.0),
        "⇧Space goes back"
    );
    assert!(
        app.mail().account_setup_open() && calendar_on(&app),
        "and it pressed nothing"
    );
}

#[test]
fn account_setup_keys_follow_what_was_clicked() {
    let mut app = setup("account-setup-focus");
    // A click on a row toggles it and focuses the content, where Enter does nothing.
    app.click(CALENDAR_ROW);
    assert!(!calendar_on(&app));
    app.key(Key::Enter);
    assert!(app.mail().account_setup_open());
    // The switch takes the keyboard itself, so Space toggles it again.
    app.click(CALENDAR_SWITCH);
    assert!(calendar_on(&app));
    app.key(Key::Space);
    assert!(!calendar_on(&app));
    // Tab goes to the button's region, where the keys stop.
    app.key(Key::Tab);
    app.key(Key::Enter);
    app.key(Key::Tab);
    app.key(Key::Space);
    assert!(app.mail().account_setup_open() && !calendar_on(&app));
    app.click(CONTINUE);
    app.settle();
    assert!(!app.mail().account_setup_open());
}
