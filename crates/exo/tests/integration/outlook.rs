//! A Microsoft account's sign-in, UI variants, local signature, and mail, end to end on
//! mock-servers' Microsoft Graph and identity platform.

use crate::support;

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{Event, Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use exo::{
    App, Screen, Start,
    sign_in::{Browser, SignIn},
};
use exo_auth::{Auth, ClientConfig, MemorySecrets, MicrosoftClient, Provider, SecretStore};
use url::Url;

use support::{start_outlook, start_outlook_new_account, start_outlook_with_contact_photos};

const EMAIL: &str = "maya@brightmoor.co";
const MICROSOFT_KEY: &str = "microsoft maya@brightmoor.co";
const CLIENT_ID: &str = "exo-test.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "GOCSPX-test";
const CTRL: Modifiers = Modifiers::CTRL;
const CMD: Modifiers = Modifiers::MAC_CMD.plus(Modifiers::COMMAND);
const SHIFT: Modifiers = Modifiers::SHIFT;
/// Center of "Sign in with Google" at 1280×800, valid both with and without a footer.
const GOOGLE_BUTTON: Pos2 = pos2(640.0, 435.0);
/// Center of "Sign in with Microsoft" at 1280×800, valid both with and without a footer.
const MICROSOFT_BUTTON: Pos2 = pos2(640.0, 495.0);
const FRAME: f32 = 1.0 / 60.0;

struct Device {
    runtime: tokio::runtime::Runtime,
    server: mock_servers::MockServer,
    dir: PathBuf,
    secrets: Arc<MemorySecrets>,
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Device {
    fn new(name: &str) -> Self {
        Self::with_config(name, mock_servers::Config::default())
    }

    fn with_config(name: &str, config: mock_servers::Config) -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let server = runtime
            .block_on(mock_servers::MockServer::start(config))
            .expect("mock starts");
        let dir = std::env::temp_dir().join(format!("exo-outlook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self {
            runtime,
            server,
            dir,
            secrets: Arc::default(),
        }
    }

    fn microsoft_client(&self) -> MicrosoftClient {
        MicrosoftClient::new(
            exo_auth::EXO_CLIENT_ID,
            outlook::Endpoints::mock(&self.server.url()),
        )
    }

    fn client_with_both(&self) -> ClientConfig {
        ClientConfig::new(CLIENT_ID, CLIENT_SECRET, self.server.endpoints())
            .with_microsoft(self.microsoft_client())
    }

    fn client_without_google(&self, missing_path: PathBuf) -> ClientConfig {
        ClientConfig::without_google(missing_path, false).with_microsoft(self.microsoft_client())
    }

    fn auth_with(&self, client: ClientConfig) -> Auth {
        Auth::open(client, &self.dir, self.secrets.clone()).expect("account list")
    }

    fn stores(&self) -> PathBuf {
        self.dir.join("stores")
    }

    fn fault(&self, fault: serde_json::Value) {
        let url = self.server.url().join("/__mock/faults").expect("url");
        self.runtime
            .block_on(async { reqwest::Client::new().post(url).json(&fault).send().await })
            .expect("fault request")
            .error_for_status()
            .expect("fault armed");
    }

    fn launch(&self, client: ClientConfig, browser: Browser) -> Harness<'static, App> {
        let (dir, secrets) = (self.dir.clone(), self.secrets.clone());
        let open = move || Auth::open(client.clone(), &dir, secrets.clone());
        let sign_in = SignIn::new(open, &self.stores(), browser).expect("sign-in runtime");
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

fn click(harness: &mut Harness<'_, App>, at: Pos2) {
    for to in [at - vec2(5.0, 0.0), at] {
        harness.input_mut().events.push(Event::PointerMoved(to));
        harness.run_steps(1);
    }
    for pressed in [true, false] {
        harness.input_mut().events.push(Event::PointerButton {
            pos: at,
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

fn log_in(app: &App) -> Option<&exo::LogIn> {
    match app.screen() {
        Screen::LogIn(screen) => Some(screen),
        Screen::SignedOut(_) | Screen::Unopened(_) | Screen::Mail(_) => None,
    }
}

fn inbox_shows(app: &App) -> bool {
    app.mail()
        .is_some_and(|mail| !mail.inbox().threads().is_empty())
}

#[test]
fn a_microsoft_account_opens_on_its_inbox() {
    let app = start_outlook("outlook-inbox");
    let graph = outlook::Client::new(
        outlook::Endpoints::mock(&app.mock_url()),
        outlook::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into()),
    );
    let inbox = app
        .block_on(
            graph
                .mail()
                .folder_messages(outlook::well_known::INBOX)
                .select("conversationId,subject")
                .order_by("receivedDateTime desc")
                .top(500)
                .all(),
        )
        .expect("Graph lists the Inbox")
        .value;
    let mut conversations: Vec<&str> = Vec::new();
    for message in &inbox {
        let id = message.conversation_id.as_deref().expect("a conversation");
        if !conversations.contains(&id) {
            conversations.push(id);
        }
    }
    let rows = app.mail().inbox().threads();
    assert_eq!(
        rows.len(),
        conversations.len(),
        "a row for each conversation in Outlook's Inbox"
    );
    assert!(
        rows.iter()
            .all(|row| conversations.contains(&row.id.as_str())),
        "{:?}",
        app.subjects()
    );
    // The newest mail is on top.
    let newest = inbox
        .first()
        .and_then(|message| message.subject.as_deref())
        .expect("mail in the Inbox");
    assert_eq!(app.subjects().first().map(String::as_str), Some(newest));
}

#[test]
fn microsoft_sign_in_works_without_a_google_oauth_client() {
    let device = Device::new("ms-only");
    let missing =
        PathBuf::from("/Users/someone/Library/Application Support/Exo/google-oauth-client.json");
    let client = device.client_without_google(missing.clone());
    let mut app = device.launch(client.clone(), approving());

    // Startup succeeds cleanly without a Google client file: no footer error is shown yet.
    let screen = log_in(app.state()).expect("sign-in screen");
    assert_eq!(screen.footer(), None);
    assert!(!screen.is_loading());

    // Clicking Google explains where to put the Google client JSON.
    click(&mut app, GOOGLE_BUTTON);
    let expected = format!(
        "Please save your Google OAuth client (Desktop app) as {}.",
        missing.display()
    );
    let screen = log_in(app.state()).expect("still on sign-in screen");
    assert_eq!(screen.footer(), Some(expected.as_str()));
    assert!(!screen.is_loading());

    // Clicking Microsoft signs in with the built-in client and opens Outlook's inbox.
    click(&mut app, MICROSOFT_BUTTON);
    run_until(&mut app, "Outlook inbox", inbox_shows);
    let mail = app.state().mail().expect("mail screen");
    assert!(mail.account().is_outlook());
    assert_eq!(mail.account().provider(), Provider::Microsoft);
    assert_eq!(mail.account().email(), EMAIL);
    assert_eq!(mail.account().key(), MICROSOFT_KEY);
    assert!(
        device
            .secrets
            .get(MICROSOFT_KEY)
            .expect("secret store")
            .is_some(),
        "Microsoft refresh token stored under provider-prefixed key"
    );
    let listed = device.auth_with(client).accounts();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider, Provider::Microsoft);
    assert_eq!(listed[0].key(), MICROSOFT_KEY);
}

#[test]
fn microsoft_consent_errors_show_admin_approval_and_stay_silent_on_cancel() {
    let device = Device::new("consent-errors");
    let missing = PathBuf::from("/tmp/missing-google.json");
    let mut app = device.launch(device.client_without_google(missing), approving());

    let admin_copy =
        "Your organization requires an administrator to approve Exo before you can sign in.";

    for code in ["AADSTS90094", "AADSTS65001"] {
        device.fault(serde_json::json!({
            "path": "/common/oauth2/v2.0/authorize",
            "status": 400,
            "reason": code,
        }));
        click(&mut app, MICROSOFT_BUTTON);
        run_until(&mut app, "admin consent footer", |app| {
            log_in(app).is_some_and(|screen| !screen.is_loading() && screen.footer().is_some())
        });
        assert_eq!(
            log_in(app.state()).expect("sign-in").footer(),
            Some(admin_copy),
            "{code} should explain admin consent"
        );
    }

    // Token-endpoint AADSTS90093 (user cannot consent) also explains admin approval.
    device.fault(serde_json::json!({
        "path": "/common/oauth2/v2.0/token",
        "status": 400,
        "reason": "AADSTS90093",
    }));
    click(&mut app, MICROSOFT_BUTTON);
    run_until(&mut app, "AADSTS90093 admin consent footer", |app| {
        log_in(app).is_some_and(|screen| !screen.is_loading() && screen.footer().is_some())
    });
    assert_eq!(
        log_in(app.state()).expect("sign-in").footer(),
        Some(admin_copy),
        "AADSTS90093 should explain admin consent"
    );

    // User cancellation (AADSTS65004) clears loading and shows no error message.
    device.fault(serde_json::json!({
        "path": "/common/oauth2/v2.0/authorize",
        "status": 400,
        "reason": "AADSTS65004",
    }));
    click(&mut app, MICROSOFT_BUTTON);
    run_until(&mut app, "loading finishes silently", |app| {
        log_in(app).is_some_and(|screen| !screen.is_loading())
    });
    // Drain any frames until the attempt outcome has been polled.
    app.run_steps(10);
    assert_eq!(log_in(app.state()).expect("sign-in").footer(), None);
}

#[test]
fn signed_out_microsoft_account_re_signs_in_with_microsoft() {
    let device = Device::new("ms-resignin");
    let client = device.client_with_both();
    let auth = device.auth_with(client.clone());
    device
        .runtime
        .block_on(auth.sign_in(None, |url| approving()(url)))
        .expect("initial Google sign-in");
    device
        .runtime
        .block_on(auth.sign_in_microsoft(None, |url| approving()(url)))
        .expect("initial Microsoft sign-in");
    auth.mark_signed_out(MICROSOFT_KEY)
        .expect("mark signed out");

    let ms_browser: Browser = Arc::new(|url: &Url| {
        assert!(
            url.path().contains("/oauth2/v2.0/authorize"),
            "re-sign-in must use Microsoft OAuth, got {url}"
        );
        let target = url.clone();
        tokio::spawn(async move {
            let _ = reqwest::get(target).await;
        });
        Ok(())
    });
    let mut app = device.launch(client, ms_browser);
    run_until(&mut app, "signed-in Google account opens", inbox_shows);
    assert!(!app.state().mail().expect("mail").account().is_outlook());

    // Switching to the signed-out Microsoft account shows `Sign in as {email}`.
    press_with(&mut app, Key::Num2, CTRL);
    let screen = log_in(app.state()).expect("sign-in screen with hint");
    assert_eq!(screen.hint(), Some(EMAIL));

    // Pressing Enter uses the hinted account's provider (Microsoft), not Google.
    press_with(&mut app, Key::Enter, Modifiers::NONE);
    run_until(&mut app, "Outlook inbox after re-sign-in", |app| {
        app.mail()
            .is_some_and(|mail| mail.account().is_outlook() && !mail.inbox().threads().is_empty())
    });
    assert!(app.state().mail().expect("mail").account().is_outlook());
}

#[test]
fn enter_while_microsoft_sign_in_is_loading_restarts_microsoft() {
    let device = Device::new("ms-enter-loading");
    let client = device.client_without_google(PathBuf::from("/tmp/missing-google.json"));
    let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = opened.clone();
    // First click opens a browser that never replies; pressing Enter restarts with Microsoft and
    // this second attempt approves so it signs in without hitting MissingClient.
    let browser: Browser = Arc::new(move |url: &Url| {
        assert!(
            url.path().contains("/oauth2/v2.0/authorize"),
            "must stay on Microsoft OAuth, got {url}"
        );
        let attempt = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if attempt >= 1 {
            let target = url.clone();
            tokio::spawn(async move {
                let _ = reqwest::get(target).await;
            });
        }
        Ok(())
    });
    let mut app = device.launch(client, browser);

    click(&mut app, MICROSOFT_BUTTON);
    run_until(&mut app, "first Microsoft browser open", |app| {
        log_in(app).is_some_and(exo::LogIn::is_loading)
            && opened.load(std::sync::atomic::Ordering::SeqCst) == 1
    });

    // Pressing Enter while loading restarts Microsoft sign-in rather than falling back to Google.
    press_with(&mut app, Key::Enter, Modifiers::NONE);
    run_until(&mut app, "Outlook inbox after Enter restart", inbox_shows);
    assert!(app.state().mail().expect("mail").account().is_outlook());
    assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn google_and_microsoft_accounts_with_the_same_email_coexist() {
    let device = Device::new("dual-provider");
    let client = device.client_with_both();
    let auth = device.auth_with(client.clone());
    device
        .runtime
        .block_on(auth.sign_in(None, |url| approving()(url)))
        .expect("Google sign-in");
    device
        .runtime
        .block_on(auth.sign_in_microsoft(None, |url| approving()(url)))
        .expect("Microsoft sign-in");
    assert_eq!(auth.accounts().len(), 2);

    let mut app = device.launch(client.clone(), approving());
    run_until(&mut app, "first account opens", inbox_shows);

    // Ctrl+1 opens Google (first), Ctrl+2 opens Microsoft (second), same email address.
    press_with(&mut app, Key::Num1, CTRL);
    run_until(&mut app, "Google account", |app| {
        app.mail().is_some_and(|mail| !mail.account().is_outlook())
    });
    assert_eq!(app.state().mail().unwrap().account().email(), EMAIL);
    assert_eq!(app.state().mail().unwrap().account().key(), EMAIL);

    press_with(&mut app, Key::Num2, CTRL);
    run_until(&mut app, "Microsoft account", |app| {
        app.mail()
            .is_some_and(|mail| mail.account().is_outlook() && !mail.inbox().threads().is_empty())
    });
    assert_eq!(app.state().mail().unwrap().account().email(), EMAIL);
    assert_eq!(app.state().mail().unwrap().account().key(), MICROSOFT_KEY);

    // Sign out the Microsoft account: the Google account stays signed in and takes over.
    press_with(&mut app, Key::K, CMD);
    type_text(&mut app, "sign out");
    press_with(&mut app, Key::Enter, Modifiers::NONE);
    run_until(&mut app, "falls back to Google account", |app| {
        app.mail().is_some_and(|mail| !mail.account().is_outlook())
    });
    let remaining = device.auth_with(client).accounts();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].provider, Provider::Google);
    assert_eq!(remaining[0].key(), EMAIL);
}

#[test]
fn outlook_triage_move_picker_and_offline_bar_match_outlook_variants() {
    let mut app = start_outlook("outlook-variants");

    // S flags and unflags with Outlook copy.
    app.key(Key::S);
    assert_eq!(app.mail().toast_text(), Some("Flagged."));
    app.key(Key::S);
    assert_eq!(app.mail().toast_text(), Some("Unflagged."));

    // Gmail-only shortcuts (Label, Remove all Labels, Mute) are hidden no-ops on Outlook.
    app.key(Key::L);
    assert_eq!(app.mail().sub_palette_rows(), None);
    app.press(Key::Y, SHIFT);
    assert_eq!(app.mail().sub_palette_rows(), None);
    app.press(Key::M, SHIFT);
    assert_ne!(app.mail().toast_text(), Some("Muted."));

    // V opens the Move picker with Outlook system folder names and "Create folder".
    app.key(Key::V);
    let (_, rows) = app.mail().sub_palette_rows().expect("Move picker opens");
    assert!(
        rows.iter().any(|row| row == "Junk Email"),
        "Move picker lists Junk Email: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row == "Deleted Items"),
        "Move picker lists Deleted Items: {rows:?}"
    );
    assert!(
        !rows.iter().any(|row| row == "Spam" || row == "Trash"),
        "Move picker hides Gmail names: {rows:?}"
    );
    app.type_text("Q4 Planning");
    let (_, filtered) = app.mail().sub_palette_rows().expect("Move picker open");
    assert!(
        filtered
            .iter()
            .any(|row| row == "Create folder “Q4 Planning”"),
        "Move picker offers folder creation: {filtered:?}"
    );
    // First Escape clears the query, second closes the Move picker.
    app.key(Key::Escape);
    app.key(Key::Escape);

    // `!` marks Junk directly on Outlook (no Mark Spam+Block sub-palette).
    app.press(Key::Num1, SHIFT);
    assert_eq!(app.mail().sub_palette_rows(), None);
    assert_eq!(app.mail().toast_text(), Some("Marked as Junk."));

    // Disconnecting shows the Outlook offline status bar (never "Gmail").
    app.disconnect();
    app.sync();
    let bar = app.mail().status_bar().expect("offline bar shows");
    assert_eq!(bar.title, "Offline");
    assert_eq!(
        bar.text(),
        "Outlook is not responding right now. Exo will work offline and catch up when Outlook responds."
    );
    app.reconnect();
}

#[test]
fn outlook_local_signature_saves_from_dialog_and_appends_on_send() {
    let mut app = start_outlook("outlook-signature");

    // Open the Outlook Signature dialog via Command Palette.
    app.press(Key::K, CMD);
    app.type_text("Signature");
    app.key(Key::Enter);
    assert!(app.mail().settings_dialog_open());

    // Type a signature and save with Cmd+Enter.
    app.type_text("Best, Maya");
    app.press(Key::Enter, CMD);
    assert!(!app.mail().settings_dialog_open());
    let saved = app
        .mail()
        .account()
        .local_signature()
        .expect("signature saved")
        .to_owned();
    assert!(saved.contains("Best, Maya"), "saved html: {saved:?}");

    // Reopen, type unsaved text, and cancel with Escape: saved signature stays intact.
    app.press(Key::K, CMD);
    app.type_text("Signature");
    app.key(Key::Enter);
    assert!(app.mail().settings_dialog_open());
    app.type_text(" UNSAVED");
    app.key(Key::Escape);
    assert!(!app.mail().settings_dialog_open());
    assert_eq!(app.mail().account().local_signature(), Some(saved.as_str()));

    // Relaunching Exo reloads the persisted local signature from SQLite settings.
    app.relaunch();
    assert_eq!(app.mail().account().local_signature(), Some(saved.as_str()));

    // Compose and send a new message after relaunch; verify the delivered Outlook Sent Items
    // message contains the persisted local signature.
    app.key(Key::C);
    assert!(app.mail().compose_pane_open());
    app.type_text("tom@brightmoor.co");
    app.key(Key::Enter);
    app.key(Key::Tab);
    app.type_text("Outlook signature check");
    app.key(Key::Tab);
    app.type_text("Quick note.");
    app.press(Key::Enter, CMD);
    // Instant Send (Cmd+Shift+Z on the undo-send toast) dispatches immediately.
    app.press(Key::Z, CMD.plus(SHIFT));
    app.wait(0.5);

    let graph = app.graph();
    let sent = app
        .block_on(
            graph
                .mail()
                .folder_messages(outlook::well_known::SENT_ITEMS)
                .select("subject,body")
                .top(500)
                .all(),
        )
        .expect("Sent Items list")
        .value;
    let delivered = sent
        .iter()
        .find(|message| message.subject.as_deref() == Some("Outlook signature check"))
        .expect("new email reached Sent Items");
    let body = delivered
        .body
        .as_ref()
        .and_then(|body| body.content.as_deref())
        .unwrap_or_default();
    assert!(
        body.contains("Best, Maya"),
        "sent Outlook email should include local signature: {body:?}"
    );
}

#[test]
fn outlook_out_of_office_reads_edits_saves_and_shows_status_bar() {
    use outlook::settings::{AutomaticRepliesStatus, ExternalAudienceScope};

    let mut app = start_outlook("outlook-ooo");
    assert_eq!(app.mail().status_bar(), None);

    // Open Out of Office Reply Settings via Command Palette.
    app.press(Key::K, CMD);
    app.type_text("out of office");
    app.key(Key::Enter);
    let initial = app
        .mail()
        .out_of_office_form()
        .expect("Outlook OOO dialog loaded");
    assert!(!initial.enabled);
    assert!(!initial.scheduled);
    // The mock seeds an off responder with external replies enabled (ExternalAudience::All) and
    // Thanksgiving copy.
    assert!(initial.external);
    assert!(!initial.contacts);
    assert!(initial.body.contains("Thanksgiving"));

    // Tab to Switch and enable OOO, check Schedule, navigate to internal message, append text,
    // keep External checked, check Contacts-only, navigate to external message, and append text.
    app.key(Key::Tab);
    app.key(Key::Space); // Enable OOO
    app.key(Key::Tab);
    app.key(Key::Space); // Schedule only during time period
    app.key(Key::Tab); // Start date
    app.key(Key::Tab); // Start time
    app.key(Key::Tab); // End date
    app.key(Key::Tab); // End time
    app.key(Key::Tab); // Internal message
    app.type_text(" — Away internally");
    app.key(Key::Tab); // External checkbox (already checked)
    app.key(Key::Tab); // Contacts-only checkbox
    app.key(Key::Space); // Check Contacts-only
    app.key(Key::Tab); // External message
    app.type_text(" — Away externally");

    // Pasting an image in an Outlook OOO editor shows the Outlook-specific alert with no Gmail link.
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 80, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("png");
    exo::clipboard::capture_image(&app.harness.ctx, png.into_inner());
    app.harness
        .input_mut()
        .events
        .push(Event::Paste(String::new()));
    app.harness.run_steps(1);
    app.settle();
    let (alert_title, alert_text) = app
        .mail()
        .out_of_office_alert_text()
        .expect("pasted file alert shows");
    assert_eq!(alert_title, "Sorry!");
    assert_eq!(
        alert_text,
        "Outlook doesn't support adding images to automatic replies."
    );
    app.key(Key::Enter); // Dismiss alert
    assert_eq!(app.mail().out_of_office_alert_text(), None);

    // Tab to Save and save to Microsoft Graph.
    app.key(Key::Tab);
    app.key(Key::Enter);
    assert_eq!(
        app.mail().out_of_office_form(),
        None,
        "dialog closes on save"
    );
    assert_eq!(
        app.mail().toast_text(),
        Some("Out of Office Reply settings updated.")
    );

    // Green status bar appears immediately for the active scheduled window.
    let bar = app.mail().status_bar().expect("OOO status bar shows");
    assert_eq!(
        (bar.title, bar.color),
        ("Out of Office Reply is On", exo_ui::MessageBarColor::Green)
    );

    // Verify Microsoft Graph mailboxSettings persisted the exact automaticRepliesSetting.
    let graph = app.graph();
    let remote = app
        .block_on(async {
            graph
                .settings()
                .mailbox_settings()
                .select("automaticRepliesSetting")
                .await
        })
        .expect("mailboxSettings read")
        .automatic_replies_setting
        .expect("automaticRepliesSetting present");
    assert_eq!(remote.status, Some(AutomaticRepliesStatus::Scheduled));
    assert_eq!(
        remote.external_audience,
        Some(ExternalAudienceScope::ContactsOnly)
    );
    assert!(
        remote
            .internal_reply_message
            .as_deref()
            .is_some_and(|html| html.contains("Away internally"))
    );
    assert!(
        remote
            .external_reply_message
            .as_deref()
            .is_some_and(|html| html.contains("Away externally"))
    );
    assert!(remote.scheduled_start_date_time.is_some());
    assert!(remote.scheduled_end_date_time.is_some());

    // Relaunching reloads Outlook OOO state and keeps the green status bar visible.
    app.relaunch();
    assert_eq!(
        app.mail().status_bar().map(|bar| bar.title),
        Some("Out of Office Reply is On")
    );

    // Reopen dialog, verify populated values, turn off, and save.
    app.press(Key::K, CMD);
    app.type_text("out of office");
    app.key(Key::Enter);
    let reopened = app.mail().out_of_office_form().expect("reopened dialog");
    assert!(reopened.enabled && reopened.scheduled && reopened.external && reopened.contacts);
    assert!(reopened.body.contains("Away internally"));
    assert!(reopened.external_body.contains("Away externally"));

    app.key(Key::Tab); // Switch
    app.key(Key::Space); // Disable
    app.key(Key::Tab); // Save (when disabled & initially enabled, Tab jumps directly to Save)
    app.key(Key::Enter);
    assert_eq!(app.mail().out_of_office_form(), None);
    assert_eq!(
        app.mail().status_bar(),
        None,
        "status bar disappears once off"
    );
    let disabled = app
        .block_on(async {
            graph
                .settings()
                .mailbox_settings()
                .select("automaticRepliesSetting")
                .await
        })
        .expect("mailboxSettings read")
        .automatic_replies_setting
        .expect("automaticRepliesSetting present");
    assert_eq!(disabled.status, Some(AutomaticRepliesStatus::Disabled));
}

#[test]
fn outlook_contacts_directory_and_photos_surface_in_compose_pane_and_profile() {
    let mut app = start_outlook_with_contact_photos("outlook-people");

    // Contact pane displays Tom's directory metadata and fetches his Outlook Graph user photo.
    assert_eq!(app.mail().contact_pane_email(), Some("tom@brightmoor.co"));
    let tom = exo::people::person(&app.harness.ctx, "tom@brightmoor.co")
        .expect("Tom synced from Outlook directory");
    assert_eq!(tom.title.as_deref(), Some("Product Designer"));
    assert!(
        tom.photo
            .as_deref()
            .is_some_and(|url| url.starts_with("outlook://users/")),
        "Tom has an Outlook photo locator: {:?}",
        tom.photo
    );
    let started = Instant::now();
    while exo::people::photo(&app.harness.ctx, "tom@brightmoor.co").is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timed out waiting for Outlook contact photo"
        );
        app.wait(0.05);
    }

    // Directory-only colleague Noor (not in mail) is suggested in Compose.
    app.key(Key::C);
    app.wait(0.5);
    app.type_text("haddad");
    app.wait(0.5);
    app.key(Key::Enter);
    assert_eq!(
        app.mail().compose_focus().map(|(to, _)| to),
        Some(vec!["noor@brightmoor.co".to_owned()]),
        "Outlook directory colleague suggested in compose"
    );

    // Account profile photo loads through Store::my_photo_metadata + Store::contact_photo on sign-in.
    let device = Device::with_config(
        "profile-photo",
        mock_servers::Config {
            photo: true,
            ..mock_servers::Config::default()
        },
    );
    let mut signed_in = device.launch(
        device.client_without_google(device.dir.join("missing.json")),
        approving(),
    );
    click(&mut signed_in, MICROSOFT_BUTTON);
    let started = Instant::now();
    while !(signed_in.state().mail().is_some()
        && exo::profile_pictures::photo(&signed_in.ctx, MICROSOFT_KEY).is_some())
    {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "timed out waiting for Outlook profile photo"
        );
        signed_in.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(signed_in);
    drop(device);
}

#[test]
fn outlook_sender_rules_block_unblock_and_focused_overrides() {
    use outlook::InferenceClassificationType;

    let mut app = start_outlook_new_account("outlook-rules");
    let graph = app.graph();
    let target_email = app
        .mail()
        .contact_pane_email()
        .expect("highlighted thread has sender")
        .to_owned();
    let before_count = app.subjects().len();
    let block_rule_name = format!("Block {target_email}");
    let baseline_rules = app
        .block_on(graph.settings().rules().all())
        .expect("Graph rules list")
        .value
        .len();

    // Block sender via Command Palette -> confirm -> Graph messageRule created + settings updated.
    app.press(Key::K, CMD);
    app.type_text(&format!("Block Sender: {target_email}"));
    app.key(Key::Enter);
    assert_eq!(
        app.mail().alert().map(|(title, _)| title),
        Some("Block sender")
    );
    app.key(Key::Enter); // Confirm Block
    app.wait(0.5);
    assert!(
        app.mail()
            .account()
            .settings()
            .blocked_senders
            .iter()
            .any(|sender| sender.eq_ignore_ascii_case(&target_email)),
        "blocked_senders records blocked Outlook sender"
    );
    let rules = app
        .block_on(graph.settings().rules().all())
        .expect("Graph rules list")
        .value;
    assert_eq!(rules.len(), baseline_rules + 1, "Graph block rule added");
    assert!(
        rules
            .iter()
            .any(|rule| rule.display_name.as_deref() == Some(block_rule_name.as_str())),
        "Graph block rule targets {target_email}: {rules:?}"
    );

    // Undo (z) removes the Graph block rule, clears blocked_senders, and restores the thread.
    app.key(Key::Z);
    app.wait(0.5);
    assert!(
        app.mail().account().settings().blocked_senders.is_empty(),
        "Undo clears blocked_senders"
    );
    let rules_after_undo = app
        .block_on(graph.settings().rules().all())
        .expect("Graph rules list")
        .value;
    assert_eq!(
        rules_after_undo.len(),
        baseline_rules,
        "Undo deletes Graph block rule while keeping pre-existing rules: {rules_after_undo:?}"
    );
    assert_eq!(app.subjects().len(), before_count);

    // Re-block sender, then open Blocked Senders settings dialog and Unblock.
    app.press(Key::K, CMD);
    app.type_text(&format!("Block Sender: {target_email}"));
    app.key(Key::Enter);
    app.key(Key::Enter); // Confirm Block
    app.wait(0.5);
    assert_eq!(
        app.block_on(graph.settings().rules().all())
            .expect("Graph rules")
            .value
            .len(),
        baseline_rules + 1
    );
    app.press(Key::K, CMD);
    app.type_text("Blocked Senders");
    app.key(Key::Enter);
    assert!(app.mail().settings_dialog_open());
    app.key(Key::Enter); // Select first blocked sender -> Unblock alert
    app.key(Key::Enter); // Confirm Unblock
    app.wait(0.5);
    assert!(app.mail().account().settings().blocked_senders.is_empty());
    assert_eq!(
        app.block_on(graph.settings().rules().all())
            .expect("Graph rules after unblock")
            .value
            .len(),
        baseline_rules,
        "Unblock deletes Graph block rule without touching other Inbox rules"
    );
    app.key(Key::Escape); // Close Blocked Senders dialog

    // Move to Other: domain row must be hidden on Outlook; sender row creates Focused Inbox override.
    let sender = app
        .mail()
        .contact_pane_email()
        .expect("sender on top thread")
        .to_owned();
    app.press(Key::K, CMD);
    app.type_text("Move to Other");
    app.key(Key::Enter);
    let (_, rows) = app
        .mail()
        .sub_palette_rows()
        .expect("Move to Other sub-palette opens");
    assert_eq!(
        rows,
        vec![
            "Move conversation to Other".to_owned(),
            format!("Move all conversations from {sender} to Other"),
        ],
        "Outlook omits bare-domain override row"
    );
    app.key(Key::ArrowDown);
    app.key(Key::Enter); // Choose "Move all conversations from <sender> to Other"
    assert_eq!(
        app.mail().alert().map(|(title, _)| title),
        Some("Move sender to Other")
    );
    app.key(Key::Enter); // Confirm Move
    app.wait(0.5);
    let overrides = app
        .block_on(graph.settings().overrides().all())
        .expect("Graph overrides")
        .value;
    assert_eq!(overrides.len(), 1, "Graph override created: {overrides:?}");
    assert_eq!(
        overrides[0].classify_as,
        Some(InferenceClassificationType::Other)
    );

    // Undo deletes the newly created override.
    app.key(Key::Z);
    app.wait(0.5);
    assert!(
        app.block_on(graph.settings().overrides().all())
            .expect("Graph overrides after undo")
            .value
            .is_empty(),
        "Undo removes Graph Focused Inbox override"
    );

    // Move to Other again, switch to Other list, and Move all from sender to Important -> updates
    // override to Focused.
    app.press(Key::K, CMD);
    app.type_text("Move to Other");
    app.key(Key::Enter);
    app.key(Key::ArrowDown);
    app.key(Key::Enter);
    app.key(Key::Enter);
    app.wait(0.5);
    app.key(Key::Tab); // Switch from Important to Other tab
    app.wait(0.3);
    app.press(Key::K, CMD);
    app.type_text("Move to Important");
    app.key(Key::Enter);
    app.key(Key::ArrowDown);
    app.key(Key::Enter);
    app.key(Key::Enter);
    app.wait(0.5);
    let focused_overrides = app
        .block_on(graph.settings().overrides().all())
        .expect("Graph overrides")
        .value;
    assert_eq!(focused_overrides.len(), 1);
    assert_eq!(
        focused_overrides[0].classify_as,
        Some(InferenceClassificationType::Focused)
    );
}

#[test]
fn outlook_view_raw_email_data_shows_graph_json() {
    let mut app = start_outlook("outlook-raw-source");

    // Open View Raw Email Data via Command Palette.
    app.press(Key::K, CMD);
    app.type_text("View Raw Email Data");
    app.key(Key::Enter);
    let (raw, selected) = app
        .mail()
        .raw_source()
        .expect("raw source viewer opens on Outlook");
    assert!(selected, "raw source text is selected once loaded");
    let parsed: serde_json::Value =
        serde_json::from_str(raw).expect("Outlook raw source is valid JSON");
    assert!(parsed["id"].is_string(), "thread id present: {parsed}");
    assert!(
        parsed["messages"]
            .as_array()
            .is_some_and(|msgs| !msgs.is_empty()),
        "Graph messages present: {parsed}"
    );

    app.key(Key::Escape);
    assert_eq!(app.mail().raw_source(), None);
}

#[test]
fn dual_provider_same_email_notifications_route_clicks_to_matching_account() {
    use exo::{
        desktop::{Desktop, Launcher, Opening, Placement},
        notifications::{Click, Notifier, Open},
    };

    let device = Device::new("dual-provider-notify");
    let client = device.client_with_both();
    let auth = device.auth_with(client.clone());
    device
        .runtime
        .block_on(auth.sign_in(None, |url| approving()(url)))
        .expect("Google sign-in");
    device
        .runtime
        .block_on(auth.sign_in_microsoft(None, |url| approving()(url)))
        .expect("Microsoft sign-in");

    let mut app = device.launch(client, approving());
    let launcher = Launcher::default();
    let desktop = Desktop::new(
        &app.ctx,
        launcher.clone(),
        Placement::load(&device.dir.join("window")),
        Opening::Shown,
    );
    app.state_mut().set_desktop(desktop);
    let notifier = Notifier::recording(launcher.clone());
    app.state_mut().set_notifier(notifier.clone());

    // Open both accounts so both complete their quiet first sync and stay open.
    press_with(&mut app, Key::Num1, CTRL);
    run_until(&mut app, "Google inbox", |app| {
        app.mail()
            .is_some_and(|mail| !mail.account().is_outlook() && !mail.inbox().threads().is_empty())
    });
    run_until(&mut app, "Google settled", App::is_idle);
    press_with(&mut app, Key::Num2, CTRL);
    run_until(&mut app, "Outlook inbox", |app| {
        app.mail()
            .is_some_and(|mail| mail.account().is_outlook() && !mail.inbox().threads().is_empty())
    });
    run_until(&mut app, "Outlook settled", App::is_idle);

    // Deliver a new message so both accounts notify after first sync.
    let deliver_url = device.server.url().join("/__mock/deliver").expect("url");
    let delivered: serde_json::Value = device.runtime.block_on(async {
        reqwest::Client::new()
            .post(deliver_url)
            .json(&serde_json::json!({
                "from": "Priya Raman <priya@brightmoor.co>",
                "subject": "Dual provider alert",
                "text": "Ping across both providers",
            }))
            .send()
            .await
            .expect("deliver")
            .json()
            .await
            .expect("json")
    });
    let gmail_thread = delivered["threadId"].as_str().expect("threadId").to_owned();

    // Switch back and forth to trigger focused sync on both accounts immediately.
    press_with(&mut app, Key::Num1, CTRL);
    run_until(&mut app, "Google notification emitted", |_| {
        notifier.posted().iter().any(|n| {
            n.click.account == EMAIL
                && matches!(&n.click.open, Open::Thread { thread, .. } if thread == &gmail_thread)
        })
    });
    press_with(&mut app, Key::Num2, CTRL);
    run_until(&mut app, "Outlook notification emitted", |_| {
        notifier
            .posted()
            .iter()
            .any(|n| n.click.account == MICROSOFT_KEY && n.body.contains("Dual provider alert"))
    });

    let posted = notifier.posted();
    let google_click = posted
        .iter()
        .find(|n| {
            n.click.account == EMAIL
                && matches!(&n.click.open, Open::Thread { thread, .. } if thread == &gmail_thread)
        })
        .expect("Google notification")
        .click
        .clone();
    let outlook_click = posted
        .iter()
        .find(|n| n.click.account == MICROSOFT_KEY && n.body.contains("Dual provider alert"))
        .expect("Outlook notification")
        .click
        .clone();
    let Open::Thread {
        thread: ref outlook_thread,
        ..
    } = outlook_click.open
    else {
        panic!("expected Outlook thread click: {outlook_click:?}");
    };
    let outlook_thread = outlook_thread.clone();

    // Outlook is currently on screen. Clicking the Google notification (plain email key) must
    // switch to Google instead of matching Outlook's display email.
    assert_eq!(app.state().mail().unwrap().account().key(), MICROSOFT_KEY);
    launcher.clicked(google_click);
    run_until(&mut app, "switches to Google and opens thread", |app| {
        app.mail().is_some_and(|mail| {
            mail.account().key() == EMAIL
                && mail.thread().is_some_and(|open| open.id() == gmail_thread)
        })
    });

    // Google is now on screen. Clicking the Outlook notification (microsoft-prefixed key) must
    // switch back to Outlook and open Outlook's thread.
    launcher.clicked(Click {
        account: MICROSOFT_KEY.into(),
        open: outlook_click.open,
    });
    run_until(&mut app, "switches to Outlook and opens thread", |app| {
        app.mail().is_some_and(|mail| {
            mail.account().key() == MICROSOFT_KEY
                && mail
                    .thread()
                    .is_some_and(|open| open.id() == outlook_thread)
        })
    });
}
