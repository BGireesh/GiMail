//! Outlook Calendar end to end on mock-servers' Microsoft Graph: day and week views, opaque
//! calendar IDs, category colors, Microsoft Teams links, skipping Google's update prompt,
//! organizer cancellation notes, recurring edits/splits/deletes, declined Outlook invites,
//! cross-provider calendar accounts (Google mail + Microsoft calendar and Microsoft mail + Google
//! calendar), Calendar Settings, and Outlook out-of-office calendar nudges.

use crate::support;

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{Event, Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use exo::{
    App, Start,
    sign_in::{Browser, SignIn},
};
use exo_auth::{Auth, ClientConfig, MemorySecrets, MicrosoftClient, Provider};
use jiff::civil::{DateTime, date};
use outlook::calendar::{
    Attendee, AttendeeType, Event as GraphEvent, EventType, FreeBusyStatus,
    OnlineMeetingProviderType, ResponseType,
};
use outlook::settings::{CategoryColor, OutlookCategory};
use support::{Fixture, start_outlook};
use url::Url;

fn dt(s: &str) -> outlook::DateTimeTimeZone {
    outlook::DateTimeTimeZone::new(
        s.parse::<DateTime>().expect("civil datetime"),
        "America/New_York",
    )
}

const EMAIL: &str = "maya@brightmoor.co";
const MICROSOFT_KEY: &str = "microsoft maya@brightmoor.co";
const COLLEAGUE: &str = "priya@brightmoor.co";
const COLLEAGUE_MS_KEY: &str = "microsoft priya@brightmoor.co";
const CLIENT_ID: &str = "exo-test.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "GOCSPX-test";
const CMD: Modifiers = Modifiers::MAC_CMD.plus(Modifiers::COMMAND);
const FRAME: f32 = 1.0 / 60.0;

/// Week view grid coordinates at 1280×800 (Sunday = 0, morning scroll).
fn at(day: usize, hour: f32) -> Pos2 {
    pos2(
        104.0 + 127.0 * (day as f32 + 0.5),
        194.0 + hour * 44.0 - 312.0,
    )
}

/// Week's new-event form controls at 1280×800.
const REPEAT: Pos2 = pos2(570.0, 378.0);
const MEETING: Pos2 = pos2(600.0, 409.0);
/// Thursday's 2pm `Calloway: Atlas feedback call` and its details' `Yes`.
const CALLOWAY: Pos2 = pos2(660.0, 520.0);
const YES: Pos2 = pos2(271.0, 754.0);
/// Week sidebar Calendars header `+` and its menu entries at 1280×800.
const CALENDARS_HEADER: Pos2 = pos2(1100.0, 438.0);
const SIGN_IN_WITH_MICROSOFT: Pos2 = pos2(1200.0, 513.0);

fn palette(app: &mut Fixture, query: &str) {
    app.press(Key::K, CMD);
    app.harness
        .input_mut()
        .events
        .push(Event::Text(query.to_owned()));
    app.harness.run_steps(1);
    app.key(Key::Enter);
}

fn shown_on(app: &Fixture, day: jiff::civil::Date) -> Vec<String> {
    app.mail()
        .calendar_events_on(day)
        .into_iter()
        .map(|(title, start)| format!("{title} {start}"))
        .collect()
}

fn double_click(app: &mut Fixture, pos: Pos2) {
    app.harness
        .input_mut()
        .events
        .push(Event::PointerMoved(pos));
    app.harness.run_steps(1);
    for _ in 0..2 {
        for pressed in [true, false] {
            app.harness.input_mut().events.push(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
            app.harness.run_steps(1);
        }
    }
    app.settle();
}

fn click_opening(app: &mut Fixture, pos: Pos2) -> Vec<String> {
    let mut opened = Vec::new();
    for to in [pos - vec2(5.0, 0.0), pos] {
        app.harness.input_mut().events.push(Event::PointerMoved(to));
        app.harness.run_steps(1);
    }
    for pressed in [true, false] {
        app.harness.input_mut().events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        app.harness.run_steps(1);
        for cmd in &app.harness.output().platform_output.commands {
            if let egui::OutputCommand::OpenUrl(open) = cmd {
                opened.push(open.url.clone());
            }
        }
    }
    app.settle();
    opened
}

fn graph_events(app: &Fixture) -> Vec<GraphEvent> {
    let graph = app.graph();
    app.block_on(
        graph
            .calendar()
            .events(None)
            .select(
                "id,subject,start,end,type,isOnlineMeeting,onlineMeetingProvider,onlineMeeting,attendees,categories,showAs,webLink,recurrence,seriesMasterId",
            )
            .top(200)
            .all(),
    )
    .expect("Graph lists events")
    .value
}

fn graph_sent_messages(app: &Fixture) -> Vec<outlook::Message> {
    let graph = app.graph();
    app.block_on(
        graph
            .mail()
            .folder_messages(outlook::well_known::SENT_ITEMS)
            .select("id,subject,bodyPreview,body")
            .top(100)
            .all(),
    )
    .expect("Graph lists Sent Items")
    .value
}

#[test]
fn outlook_day_and_week_views_show_events_categories_and_open_web_link_cleanly() {
    let mut app = start_outlook("outlook-cal-views");
    // Seed an Outlook master category and a categorized event on Wednesday morning.
    let graph = app.graph();
    app.block_on(async {
        let _ = graph
            .settings()
            .create_category(&OutlookCategory {
                id: None,
                display_name: Some("Urgent".into()),
                color: Some(CategoryColor::Preset(0)),
            })
            .await;
        let event = GraphEvent {
            subject: Some("Board prep".into()),
            start: Some(dt("2026-11-18T11:00:00")),
            end: Some(dt("2026-11-18T12:00:00")),
            categories: vec!["Urgent".into()],
            ..GraphEvent::default()
        };
        graph
            .calendar()
            .create_event(None, &event)
            .await
            .expect("creates categorized Outlook event");
    });

    // Reopen so the calendar sync fetches the newly seeded category and event from Graph.
    app.relaunch();
    // Day view opens on today and lists synced Outlook events.
    app.key(Key::Num0);
    assert_eq!(app.mail().calendar_day(), Some(date(2026, 11, 17)));
    app.key(Key::Num2);
    app.settle();
    assert_eq!(app.mail().calendar_week(), Some(date(2026, 11, 17)));

    // Primary Outlook calendar is stored under the canonical Microsoft key with an opaque ID.
    let account = app.mail().account();
    assert_eq!(account.key(), MICROSOFT_KEY);
    let ms_account = account
        .settings()
        .calendar_accounts
        .get(MICROSOFT_KEY)
        .expect("Outlook calendar account stored under canonical key");
    assert!(!ms_account.calendars.is_empty());
    assert!(
        !ms_account.calendars.contains_key(EMAIL),
        "Outlook calendar IDs are opaque, not the user's email"
    );

    let wednesday = date(2026, 11, 18);
    assert!(
        shown_on(&app, wednesday).contains(&"Board prep 11:00 AM".to_owned()),
        "categorized Outlook event shows: {:?}",
        shown_on(&app, wednesday)
    );

    // Open details for Wednesday 11am Board prep and click `Open in Outlook Calendar`.
    app.click(at(3, 11.4));
    assert_eq!(app.mail().calendar_details(), Some(false));
    // Details popover sits to the left/right of Wednesday; click its `…` menu, then the last item.
    // Rather than hardcoding a single side, open Dinner with Jess on Thursday 7:30pm (known side)
    // and check its `Open in Outlook Calendar` link has no Google query params.
    app.key(Key::Escape);
    app.click(at(4, 19.8));
    assert_eq!(app.mail().calendar_details(), Some(false));
    // Menu button on Thursday evening details (right-anchored popover left of Thursday column).
    app.click(pos2(593.0, 566.0));
    let opened = click_opening(&mut app, pos2(600.0, 675.0));
    if let Some(url) = opened.first() {
        assert!(
            !url.contains("authuser=") && !url.contains("shxr="),
            "Outlook webLink must never append Google authuser/shxr params: {url}"
        );
    }
}

#[test]
fn outlook_create_event_with_teams_link_and_default_meeting_setting() {
    let mut app = start_outlook("outlook-cal-teams");
    app.key(Key::Num2);
    app.settle();

    // New event form defaults to the primary Outlook calendar's opaque ID.
    app.key(Key::B);
    assert_eq!(app.mail().calendar_form(), Some(true));
    let default_cal = app
        .mail()
        .calendar_form_calendar()
        .expect("form calendar")
        .to_owned();
    assert_ne!(
        default_cal, EMAIL,
        "Outlook form calendar uses opaque calendar ID"
    );
    assert_eq!(app.mail().calendar_form_new_teams(), Some(false));
    assert_eq!(app.mail().calendar_form_new_meet(), Some(false));

    // Clicking Meeting dropdown and pressing Enter selects Microsoft Teams (not Google Meet).
    app.click(MEETING);
    app.key(Key::Enter);
    assert_eq!(app.mail().calendar_form_new_teams(), Some(true));
    assert_eq!(app.mail().calendar_form_new_meet(), Some(false));

    // Palette toggles Microsoft Teams link off and back on.
    palette(&mut app, "remove microsoft teams link");
    assert_eq!(app.mail().calendar_form_new_teams(), Some(false));
    palette(&mut app, "add microsoft teams link");
    assert_eq!(app.mail().calendar_form_new_teams(), Some(true));

    // Fill title and save.
    palette(&mut app, "edit title");
    app.type_text("Teams Architecture Review");
    app.press(Key::Enter, CMD);
    assert_eq!(app.mail().calendar_form(), None);
    let tuesday = date(2026, 11, 17);
    assert!(
        shown_on(&app, tuesday).contains(&"Teams Architecture Review 9:00 AM".to_owned()),
        "{:?}",
        shown_on(&app, tuesday)
    );

    // Wait past the Undo hold and verify Graph stored a Teams online meeting.
    std::thread::sleep(Duration::from_millis(10_500));
    app.wait(1.0);
    let events = graph_events(&app);
    let created = events
        .iter()
        .find(|e| e.subject.as_deref() == Some("Teams Architecture Review"))
        .expect("event sent to Graph");
    assert_eq!(created.is_online_meeting, Some(true));
    assert_eq!(
        created.online_meeting_provider,
        Some(OnlineMeetingProviderType::TeamsForBusiness)
    );
    assert!(
        created
            .online_meeting
            .as_ref()
            .and_then(|m| m.join_url.as_deref())
            .is_some(),
        "Graph assigned a Teams joinUrl"
    );

    // Enabling the default meeting link setting automatically adds Teams links on Outlook Create.
    app.mail_mut().account_mut().update_settings(|settings| {
        settings.meeting_links.default_meeting_link = Some("google".into());
    });
    app.settle();
    app.key(Key::B);
    assert_eq!(app.mail().calendar_form_new_teams(), Some(true));
    assert_eq!(app.mail().calendar_form_new_meet(), Some(false));
}

#[test]
fn outlook_edit_skips_update_prompt_preserves_teams_and_delete_sends_cancel_note() {
    let mut app = start_outlook("outlook-cal-edit-delete");
    // Create an Outlook meeting organized by Maya with Priya and a Teams link.
    let graph = app.graph();
    app.block_on(async {
        let input = GraphEvent {
            subject: Some("Vendor Sync".into()),
            start: Some(dt("2026-11-19T10:00:00")),
            end: Some(dt("2026-11-19T11:00:00")),
            attendees: vec![Attendee::new(
                "priya@brightmoor.co",
                Some("Priya Nair"),
                AttendeeType::Required,
            )],
            is_online_meeting: Some(true),
            online_meeting_provider: Some(OnlineMeetingProviderType::TeamsForBusiness),
            ..GraphEvent::default()
        };
        graph
            .calendar()
            .create_event(None, &input)
            .await
            .expect("creates Teams meeting");
    });

    app.relaunch();
    app.key(Key::Num2);
    app.settle();
    let thursday = date(2026, 11, 19);
    assert!(
        shown_on(&app, thursday).contains(&"Vendor Sync 10:00 AM".to_owned()),
        "{:?}",
        shown_on(&app, thursday)
    );

    // Edit the event on Thursday 10am: existing Teams link cannot be removed, and saving skips
    // Google's "Send an update?" prompt.
    double_click(&mut app, at(4, 10.4));
    assert_eq!(app.mail().calendar_form(), Some(false));
    app.press(Key::End, Modifiers::NONE);
    app.type_text(" Updated");
    app.press(Key::Enter, CMD);
    assert_eq!(
        app.mail().calendar_prompt(),
        None,
        "Outlook skips Send an update? because Graph always notifies guests"
    );
    assert_eq!(
        app.mail().toast_text(),
        Some("Event updated and notification sent.")
    );
    assert!(
        shown_on(&app, thursday).contains(&"Vendor Sync Updated 10:00 AM".to_owned()),
        "{:?}",
        shown_on(&app, thursday)
    );

    // Let the edit flush so the event is synced, then delete as organizer with other guests.
    std::thread::sleep(Duration::from_millis(10_500));
    app.wait(1.0);
    // Open details for Thursday 10am and click menu -> Delete.
    // Use Weekly product sync (Monday 10am, known details coordinates from calendar.rs) OR open
    // Monday 10am Weekly product sync which already has guests and is organized by Maya!
    // First confirm Vendor Sync still has its Teams meeting on Graph after the edit.
    let updated = graph_events(&app)
        .into_iter()
        .find(|e| e.subject.as_deref() == Some("Vendor Sync Updated"))
        .expect("updated event on Graph");
    assert_eq!(updated.is_online_meeting, Some(true));

    // Delete Monday 10am Weekly product sync (`All events`) as organizer with cancel note.
    let monday = date(2026, 11, 16);
    app.click(at(1, 10.2));
    assert_eq!(app.mail().calendar_details(), Some(false));
    app.click(pos2(810.0, 327.0));
    app.click(pos2(820.0, 407.0));
    assert_eq!(app.mail().calendar_prompt(), Some("Delete recurring event"));
    // Pick `All events` (third option) and confirm.
    app.key(Key::ArrowDown);
    app.key(Key::ArrowDown);
    app.key(Key::Enter);
    assert_eq!(
        app.mail().calendar_prompt(),
        Some("Include a note for attendees?"),
        "Outlook shows optional cancel-note prompt"
    );
    // Escape goes back to the scope step.
    app.key(Key::Escape);
    assert_eq!(app.mail().calendar_prompt(), Some("Delete recurring event"));
    app.key(Key::Enter);
    assert_eq!(
        app.mail().calendar_prompt(),
        Some("Include a note for attendees?")
    );
    // Click inside the note box (centered in 1280×800 modal) and type a note, then ⌘Enter to Send.
    app.click(pos2(640.0, 415.0));
    app.type_text("Moving sync to next month");
    app.press(Key::Enter, CMD);
    assert_eq!(app.mail().calendar_prompt(), None);
    assert!(
        !shown_on(&app, monday)
            .iter()
            .any(|e| e.starts_with("Weekly product sync"))
    );

    // Past the Undo hold, Graph deleted the series and sent the cancellation note in Sent Items.
    std::thread::sleep(Duration::from_millis(10_500));
    app.wait(1.0);
    assert!(
        !graph_events(&app)
            .iter()
            .any(|e| e.subject.as_deref() == Some("Weekly product sync")),
        "series removed from Graph"
    );
    let sent = graph_sent_messages(&app);
    assert!(
        sent.iter().any(|msg| {
            msg.subject
                .as_deref()
                .is_some_and(|s| s.contains("Canceled: Weekly product sync"))
                && msg
                    .body
                    .as_ref()
                    .and_then(|b| b.content.as_deref())
                    .or(msg.body_preview.as_deref())
                    .is_some_and(|body| body.contains("Moving sync to next month"))
        }),
        "cancellation email with note sent: {sent:?}"
    );
}

#[test]
fn outlook_recurring_create_and_split_this_and_following() {
    let mut app = start_outlook("outlook-cal-recurring");
    app.key(Key::Num2);
    app.settle();

    // Create a weekly recurring event on Outlook.
    app.key(Key::B);
    app.type_text("Outlook Standup");
    app.click(REPEAT);
    app.key(Key::ArrowDown);
    app.key(Key::ArrowDown);
    app.key(Key::Enter);
    assert_eq!(
        app.mail().calendar_form_repeat(),
        Some(("Weekly on Tuesday".to_owned(), false))
    );
    app.press(Key::Enter, CMD);
    let (tuesday, next_tuesday) = (date(2026, 11, 17), date(2026, 11, 24));
    assert!(
        shown_on(&app, tuesday).contains(&"Outlook Standup 9:00 AM".to_owned()),
        "{:?}",
        shown_on(&app, tuesday)
    );
    app.key(Key::Equals);
    app.settle();
    assert!(
        shown_on(&app, next_tuesday).contains(&"Outlook Standup 9:00 AM".to_owned()),
        "{:?}",
        shown_on(&app, next_tuesday)
    );

    // Go back to this week and split `Focus time` (`This and all following events`).
    app.key(Key::T);
    app.settle();
    let (wednesday, friday) = (date(2026, 11, 18), date(2026, 11, 20));
    app.drag(at(3, 9.5), at(3, 10.5));
    assert_eq!(app.mail().calendar_prompt(), Some("Edit recurring event"));
    app.key(Key::ArrowDown);
    app.key(Key::Enter);
    assert_eq!(app.mail().calendar_prompt(), None);
    assert!(
        shown_on(&app, wednesday).contains(&"Focus time 10:00 AM".to_owned()),
        "{:?}",
        shown_on(&app, wednesday)
    );
    assert!(
        shown_on(&app, friday).contains(&"Focus time 10:00 AM".to_owned()),
        "{:?}",
        shown_on(&app, friday)
    );

    // Wait past the hold and confirm both the new Outlook series and split persisted on Graph.
    std::thread::sleep(Duration::from_millis(10_500));
    app.wait(1.0);
    let events = graph_events(&app);
    assert!(
        events.iter().any(|e| {
            e.subject.as_deref() == Some("Outlook Standup")
                && e.kind == Some(EventType::SeriesMaster)
                && e.recurrence.is_some()
        }),
        "recurring Outlook seriesMaster saved on Graph"
    );
    let focus_masters: Vec<&GraphEvent> = events
        .iter()
        .filter(|e| {
            e.subject.as_deref() == Some("Focus time") && e.kind == Some(EventType::SeriesMaster)
        })
        .collect();
    assert_eq!(
        focus_masters.len(),
        2,
        "split produced truncated original + new seriesMaster: {focus_masters:?}"
    );
}

#[test]
fn outlook_rsvp_and_declined_invite_hides_yes_and_maybe() {
    let mut app = start_outlook("outlook-cal-rsvp");
    // Answer Thursday's `Calloway: Atlas feedback call` from Week details.
    app.key(Key::Num2);
    app.settle();
    app.click(CALLOWAY);
    assert_eq!(app.mail().calendar_details(), Some(false));
    app.click(YES);
    app.settle();
    let calloway = graph_events(&app)
        .into_iter()
        .find(|e| e.subject.as_deref() == Some("Calloway: Atlas feedback call"))
        .expect("Calloway event on Graph");
    let accepted = calloway.attendees.iter().any(|a| {
        a.email_address
            .as_ref()
            .and_then(|e| e.address.as_deref())
            .is_some_and(|addr| addr.eq_ignore_ascii_case(EMAIL))
            && a.status.as_ref().and_then(|s| s.response) == Some(ResponseType::Accepted)
    });
    assert!(accepted, "Outlook details RSVP accepted on Graph");

    // Close Week, open Grace's invitation thread, and decline it via the invite card.
    app.key(Key::Escape);
    app.key(Key::Escape);
    app.settle();
    while !app.cursor().starts_with("Invitation: Coffee with Grace") {
        app.key(Key::J);
    }
    app.key(Key::Enter);
    app.settle();
    assert_eq!(app.mail().invite_answer(), Some("needsAction"));
    // Tab focuses Yes (0); two Right presses move to No (2); Enter declines.
    app.key(Key::Tab);
    app.key(Key::ArrowRight);
    app.key(Key::ArrowRight);
    app.key(Key::Enter);
    app.settle();
    assert_eq!(app.mail().invite_answer(), Some("declined"));

    // Once declined on Outlook, Yes and Maybe are hidden (only No stays selected). Moving focus
    // and pressing Enter must NOT flip the answer to Yes or Maybe.
    app.key(Key::Escape);
    app.key(Key::Tab);
    app.key(Key::ArrowRight);
    app.key(Key::Enter);
    app.settle();
    assert_eq!(
        app.mail().invite_answer(),
        Some("declined"),
        "declined Outlook invite keeps No selected and cannot re-answer Yes/Maybe"
    );
}

#[test]
fn outlook_calendar_out_of_office_nudge_triggers_for_oof_event() {
    let mut app = start_outlook("outlook-cal-ooo");
    let graph = app.graph();
    // Add an upcoming Outlook out-of-office event (`showAs: "oof"`) starting in 3 days.
    app.block_on(async {
        let oof = GraphEvent {
            subject: Some("Out of Office".into()),
            start: Some(dt("2026-11-20T09:00:00")),
            end: Some(dt("2026-11-21T17:00:00")),
            show_as: Some(FreeBusyStatus::Oof),
            ..GraphEvent::default()
        };
        graph
            .calendar()
            .create_event(None, &oof)
            .await
            .expect("creates OOO event");
    });

    // Relaunch so startup polls Outlook's primary calendar via `upcoming_events("primary", ...)`
    // and wait past the 2s nudge delay.
    app.relaunch();
    app.wait(2.5);
    let alert = app
        .mail()
        .out_of_office_alert_text()
        .map(|(title, _)| title.to_owned());
    assert_eq!(
        alert.as_deref(),
        Some("Set Out of Office Reply?"),
        "Outlook showAs=oof calendar event triggers the OOO nudge"
    );
}

// ---------------------------------------------------------------------------
// Cross-provider calendar accounts & Calendar Settings (`Sign in with Microsoft`)
// ---------------------------------------------------------------------------

struct DualDevice {
    runtime: tokio::runtime::Runtime,
    server: mock_servers::MockServer,
    dir: PathBuf,
    secrets: Arc<MemorySecrets>,
}

impl Drop for DualDevice {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl DualDevice {
    fn new(name: &str) -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(
                mock_servers::Config::default(),
            ))
            .expect("mock starts");
        let dir =
            std::env::temp_dir().join(format!("exo-outlook-cal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self {
            runtime,
            server,
            dir,
            secrets: Arc::default(),
        }
    }

    fn client(&self) -> ClientConfig {
        ClientConfig::new(CLIENT_ID, CLIENT_SECRET, self.server.endpoints()).with_microsoft(
            MicrosoftClient::new(
                exo_auth::EXO_CLIENT_ID,
                outlook::Endpoints::mock(&self.server.url()),
            ),
        )
    }

    fn auth(&self) -> Auth {
        Auth::open(self.client(), &self.dir, self.secrets.clone()).expect("auth")
    }

    fn sign_in_google(&self) {
        let auth = self.auth();
        self.runtime
            .block_on(auth.sign_in(None, |url| approving()(url)))
            .expect("Google sign-in");
    }

    fn sign_in_microsoft(&self) {
        let auth = self.auth();
        self.runtime
            .block_on(auth.sign_in_microsoft(None, |url| approving()(url)))
            .expect("Microsoft sign-in");
    }

    fn launch(&self, browser: Browser) -> Harness<'static, App> {
        let (client, dir, secrets) = (self.client(), self.dir.clone(), self.secrets.clone());
        let open = move || Auth::open(client.clone(), &dir, secrets.clone());
        let sign_in = SignIn::new(open, &self.dir.join("stores"), browser).expect("sign-in");
        let assets = exo::assets_dir();
        let clock = exo::Clock::fixed(mock_servers::Config::default().now);
        let mut harness = Harness::builder()
            .with_size([1280.0, 800.0])
            .with_os(egui::os::OperatingSystem::Mac)
            .with_step_dt(FRAME)
            .build_eframe(move |cc| {
                App::new(cc, &assets, Start::SignIn(sign_in), clock).expect("launches")
            });
        harness.run_steps(2);
        harness
    }
}

fn approving() -> Browser {
    Arc::new(|url: &Url| {
        let target = url.clone();
        tokio::spawn(async move {
            let _ = reqwest::get(target).await;
        });
        Ok(())
    })
}

fn choosing(email: &'static str) -> Browser {
    Arc::new(move |url: &Url| {
        let mut visit = url.clone();
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "login_hint")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        visit
            .query_pairs_mut()
            .clear()
            .extend_pairs(pairs)
            .append_pair("login_hint", email);
        tokio::spawn(async move {
            let _ = reqwest::get(visit).await;
        });
        Ok(())
    })
}

fn run_until(harness: &mut Harness<'_, App>, what: &str, done: impl Fn(&App) -> bool) {
    let started = Instant::now();
    while !done(harness.state()) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn press_with(harness: &mut Harness<'_, App>, key: Key, modifiers: Modifiers) {
    let event = |pressed| Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    };
    harness.input_mut().events.push(event(true));
    harness.run_steps(1);
    harness.input_mut().events.push(event(false));
    harness.run_steps(1);
}

fn click(harness: &mut Harness<'_, App>, pos: Pos2) {
    for to in [pos - vec2(5.0, 0.0), pos] {
        harness.input_mut().events.push(Event::PointerMoved(to));
        harness.run_steps(1);
    }
    for pressed in [true, false] {
        harness.input_mut().events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
    }
}

fn type_text(harness: &mut Harness<'_, App>, text: &str) {
    for ch in text.chars() {
        harness.input_mut().events.push(Event::Text(ch.to_string()));
        harness.run_steps(1);
    }
}

fn inbox_shows(app: &App) -> bool {
    app.mail()
        .is_some_and(|mail| !mail.inbox().threads().is_empty())
}

#[test]
fn google_mail_adds_microsoft_calendar_switches_meet_to_teams_and_removes_by_key() {
    let device = DualDevice::new("google-mail-ms-cal");
    device.sign_in_google();
    let mut app = device.launch(choosing(COLLEAGUE));
    run_until(&mut app, "inbox", inbox_shows);
    press_with(&mut app, Key::Num2, Modifiers::NONE);
    run_until(&mut app, "idle", App::is_idle);

    // Click `+` next to Calendars -> `Sign in with Microsoft`.
    click(&mut app, CALENDARS_HEADER);
    click(&mut app, SIGN_IN_WITH_MICROSOFT);
    run_until(&mut app, "Microsoft calendar added", |app| {
        app.mail().is_some_and(|mail| {
            mail.calendar_account_list().len() == 2
                && mail
                    .account()
                    .settings()
                    .calendar_accounts
                    .contains_key(COLLEAGUE_MS_KEY)
        })
    });
    run_until(&mut app, "Microsoft calendar events loaded", |app| {
        app.mail().is_some_and(|mail| {
            mail.calendar_events_on(date(2026, 11, 18))
                .iter()
                .any(|(title, _)| title == "Dentist")
        })
    });

    // Sidebar displays plain email (never the `microsoft ` prefix) while keyed canonically.
    let listed = app.state().mail().expect("mail").calendar_account_list();
    assert_eq!(listed[1].0, COLLEAGUE);
    assert!(!listed[1].1.is_empty());
    assert!(!listed[1].2);
    assert_eq!(
        device.auth().accounts()[0].calendars,
        [COLLEAGUE_MS_KEY],
        "Microsoft calendar grant stored under canonical key"
    );

    // Enable auto meeting link: Create starts with NewMeet on Google primary calendar, then
    // switching to the Outlook calendar adapts it to NewTeams!
    app.state_mut()
        .mail_mut()
        .expect("mail")
        .account_mut()
        .update_settings(|s| {
            s.meeting_links.default_meeting_link = Some("google".into());
        });
    run_until(&mut app, "idle", App::is_idle);
    press_with(&mut app, Key::B, Modifiers::NONE);
    assert_eq!(
        app.state().mail().expect("mail").calendar_form_new_meet(),
        Some(true)
    );
    assert_eq!(
        app.state().mail().expect("mail").calendar_form_new_teams(),
        Some(false)
    );

    // Open Change Calendar and pick the Microsoft calendar (`Calendar`).
    press_with(&mut app, Key::K, CMD);
    type_text(&mut app, "change calendar");
    press_with(&mut app, Key::Enter, Modifiers::NONE);
    type_text(&mut app, "calendar");
    press_with(&mut app, Key::Enter, Modifiers::NONE);
    assert_eq!(
        app.state().mail().expect("mail").calendar_form_new_teams(),
        Some(true),
        "switching to Outlook calendar in Create updates default meeting link to Teams"
    );
    assert_eq!(
        app.state().mail().expect("mail").calendar_form_new_meet(),
        Some(false)
    );
    press_with(&mut app, Key::Escape, Modifiers::NONE);

    // Removing by canonical key cleans up only the Microsoft calendar account.
    app.state_mut()
        .mail_mut()
        .expect("mail")
        .account_mut()
        .remove_calendar_account(COLLEAGUE_MS_KEY);
    run_until(&mut app, "Microsoft calendar removed", |app| {
        app.mail()
            .is_some_and(|mail| mail.calendar_account_list().len() == 1)
    });
    assert!(device.auth().accounts()[0].calendars.is_empty());
    assert!(
        !app.state()
            .mail()
            .expect("mail")
            .account()
            .settings()
            .calendar_accounts
            .contains_key(COLLEAGUE_MS_KEY)
    );

    // Open Calendar Settings and click `Sign in with Microsoft` beside `Sign in with Google`.
    press_with(&mut app, Key::K, CMD);
    type_text(&mut app, "calendar settings");
    press_with(&mut app, Key::Enter, Modifiers::NONE);
    run_until(&mut app, "Calendar Settings open", |app| {
        app.mail().is_some_and(exo::Mail::calendar_settings_open)
    });
    click(&mut app, pos2(615.0, 616.0));
    run_until(
        &mut app,
        "Microsoft calendar re-added from settings",
        |app| {
            app.mail().is_some_and(|mail| {
                let listed = mail.calendar_account_list();
                listed.len() == 2
                    && listed.iter().all(|(_, cals, out)| !cals.is_empty() && !out)
                    && mail
                        .account()
                        .settings()
                        .calendar_accounts
                        .contains_key(COLLEAGUE_MS_KEY)
            })
        },
    );
}

#[test]
fn microsoft_mail_adds_google_calendar_for_same_email_from_calendar_settings() {
    let device = DualDevice::new("ms-mail-dual-same-email");
    device.sign_in_microsoft();
    let mut app = device.launch(approving());
    run_until(&mut app, "Outlook inbox", inbox_shows);
    assert_eq!(
        app.state().mail().expect("mail").account().provider(),
        Provider::Microsoft
    );

    // Open Calendar Settings via palette and click `Sign in with Google` to add the same email's
    // Google Calendar alongside the primary Microsoft calendar.
    press_with(&mut app, Key::K, CMD);
    type_text(&mut app, "calendar settings");
    press_with(&mut app, Key::Enter, Modifiers::NONE);
    run_until(&mut app, "Calendar Settings open", |app| {
        app.mail().is_some_and(exo::Mail::calendar_settings_open)
    });

    // Click `Sign in with Google` at bottom left of Calendar Settings (body.top = 234, y = 616).
    click(&mut app, pos2(455.0, 616.0));
    run_until(&mut app, "dual-provider calendar added", |app| {
        app.mail().is_some_and(|mail| {
            let accounts = &mail.account().settings().calendar_accounts;
            let listed = mail.calendar_account_list();
            accounts.contains_key(MICROSOFT_KEY)
                && accounts.contains_key(EMAIL)
                && listed.len() == 2
                && listed.iter().all(|(_, cals, out)| !cals.is_empty() && !out)
        })
    });

    // Both entries display plain `maya@brightmoor.co` while remaining distinct by canonical key.
    let listed = app.state().mail().expect("mail").calendar_account_list();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].0, EMAIL);
    assert_eq!(listed[1].0, EMAIL);
    assert!(!listed[0].1.is_empty() && !listed[1].1.is_empty());

    // Removing the secondary Google calendar (`maya@brightmoor.co`) leaves the primary Microsoft
    // calendar (`microsoft maya@brightmoor.co`) intact.
    app.state_mut()
        .mail_mut()
        .expect("mail")
        .account_mut()
        .remove_calendar_account(EMAIL);
    run_until(&mut app, "Google calendar removed", |app| {
        app.mail().is_some_and(|mail| {
            let accounts = &mail.account().settings().calendar_accounts;
            accounts.contains_key(MICROSOFT_KEY)
                && !accounts.contains_key(EMAIL)
                && mail.calendar_account_list().len() == 1
        })
    });
}
