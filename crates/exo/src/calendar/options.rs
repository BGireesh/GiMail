//! Calendar Settings, the dialog that the week view's gear, its time zone labels and the palette
//! open. It holds the calendar's options (the secondary time zone's input among them), its meeting
//! links, the way to its notifications, and its calendar accounts, with a way to sign another in.

use egui::{Color32, CornerRadius, Rect, Sense, Ui, Vec2, pos2, vec2};
use exo_ui::{Dialog, Family, Font, TextStyle};
use jiff::Timestamp;

pub use super::options_cache::CacheChange;
use super::options_cache::{self, CacheInput};
use super::timezones::{self, ZoneInput};

/// The headings are 12px/20px bold, nudged `top: 3px`. The checkbox's label is 14px/21px weight
/// 500 (a 16px checkbox, the label 24px in). The radios' labels are 12px/20px, with the 12px radio
/// 1px in and 3px down and the label 20px in. Rows are 24px apart. Each account's address is
/// 12px/24px beside its 16px avatar.
const HEADING: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 20.0);
const CHECK_LABEL: TextStyle = TextStyle::new(Font::new(Family::Sans, 500), 14.0, 21.0);
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const ACCOUNT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 24.0);
const ROW: f32 = 24.0;
const LABEL_LEFT: f32 = 24.0;
const RADIO_LABEL_LEFT: f32 = 20.0;
/// Where each part starts from the body's top. `Options`' box and its first row come first, then a
/// row every 24px (20px rows, `gap: 4px`), and the time zone's input takes a row more.
/// `Meeting Links`' box is 20px after, with its radios 27px under it. After the last radio's 20px
/// row, it's 19.5px to `Notifications`' box (its button 27px under, the box 44px tall), then 20px
/// to `Accounts`' box. The first avatar is 32px under that box and its address 29px, with an
/// account every 28px (24px tall, 4px apart). `Sign in with Google` is 8px under the last account,
/// with its 16px icon and then its 12px/16px label 24px in, 1px down.
const OPTIONS_TOP: f32 = 20.0;
const CHECK_TOP: f32 = 47.5;
const LINKS_BELOW: f32 = 40.5;
const RADIOS_BELOW: f32 = 27.0;
const ACCOUNTS_GAP: f32 = 19.5;
const NOTIFICATIONS_BUTTON: f32 = 27.0;
const NOTIFICATIONS_HEIGHT: f32 = 64.0;
const ACCOUNT_ROW: f32 = 24.0;
const ACCOUNT_PITCH: f32 = 28.0;
const ACCOUNTS_LIST: f32 = 28.0;
const SIGN_IN_GAP: f32 = 8.0;
const SIGN_IN_BUTTON_GAP: f32 = 16.0;
const SIGN_IN: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
/// The avatar's initial: 10px, white.
const INITIAL: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 10.0, 12.0);
/// A hovered account's row (not the mail account's): `#f2f4fa` (Carbon's card `#eaeaea`) 8px
/// past its sides, 2px corners, and its `Remove Account` showing at its right, black at `.4`.
const ROW_HOVER: Color32 = Color32::from_rgb(0xf2, 0xf4, 0xfa);
const ROW_HOVER_CARBON: Color32 = Color32::from_rgb(0xea, 0xea, 0xea);
const ROW_OUTSET: f32 = 8.0;
const REMOVE_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 102);

/// The secondary time zone's input has `margin-left: 20px; width: 254px` and sits 25.5px under its
/// row's top. It's 12px/16px text over `padding-bottom: 2px` and a 2px `#aeb1dd` border (a literal
/// both themes keep). Its list is 286px wide, from 16px left of the input and 4px under it, and
/// shows five zones.
const ZONE_LEFT: f32 = 20.0;
const ZONE_BELOW: f32 = 25.5;
const ZONE_WIDTH: f32 = 254.0;
const ZONE_TEXT_HEIGHT: f32 = 16.0;
const ZONE_LIST_WIDTH: f32 = 286.0;
const ZONES: usize = 5;
/// The Notifications link: 12px/20px in the link colour, `#54acdc`.
const LINK_INK: Color32 = Color32::from_rgb(0x54, 0xac, 0xdc);
const NUDGE: f32 = 3.0;

/// The default meeting links Exo offers: Meet/Teams for Google/Outlook events, or none (Zoom
/// links need a server Exo doesn't have).
const MEETING_LINKS: [(Option<&str>, &str); 2] = [
    (
        Some("google"),
        "Add Meet links to Google events and Teams links to Outlook events",
    ),
    (None, "Do not automatically add meeting links"),
];

/// An option of the list, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// `Start week on Monday`.
    Monday,
    /// `Show weekends`.
    Weekends,
    Declined,
    /// `Show secondary timezone`.
    SecondaryZone,
}

const OPTIONS: [(Setting, &str); 4] = [
    (Setting::Monday, "Start week on Monday"),
    (Setting::Weekends, "Show weekends"),
    (Setting::Declined, "Show declined events"),
    (Setting::SecondaryZone, "Show secondary timezone"),
];

/// What the dialog changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptionsAction {
    Toggle(Setting),
    /// A zone picked in the secondary time zone's input, by IANA id.
    SecondaryZone(String),
    MeetingLink(Option<String>),
    /// A change in the Calendar Cache section.
    Cache(CacheChange),
    /// `Manage notification settings`, which puts the Notifications dialog in this one's place.
    Notifications,
    /// `Sign in with Google` / `Sign in with Microsoft`, for another calendar account.
    SignIn(exo_auth::Provider),
    /// `Remove Account`, by canonical account key.
    Remove(String),
    Close,
}

/// A calendar account the dialog lists.
pub struct ShownAccount<'a> {
    /// Canonical account key (`email` for Google, `microsoft {email}` for Microsoft).
    pub key: &'a str,
    pub email: &'a str,
    /// Its avatar's colour.
    pub color: Color32,
    /// The mail account's own, which can't be removed.
    pub primary: bool,
}

/// What the dialog shows.
pub struct Shown<'a> {
    /// Each option and whether it's on.
    pub options: [(Setting, bool); 4],
    /// The secondary time zone, by IANA id, once one's picked.
    pub zone: Option<&'a str>,
    pub now: Timestamp,
    pub meeting_link: Option<&'a str>,
    /// The calendar accounts, the mail account's first and the others by address.
    pub accounts: &'a [ShownAccount<'a>],
    /// The calendar cache's settings, and the address book its people are picked from.
    pub cache: &'a exo_store::CalendarCache,
    pub book: Option<&'a crate::compose::contacts::Index>,
    /// Where the cache stands: when each person was last cached and who's being read.
    pub status: options_cache::CacheStatus<'a>,
}

impl Shown<'_> {
    fn on(&self, option: Setting) -> bool {
        self.options.iter().any(|(each, on)| *each == option && *on)
    }
}

/// The body's height, down to `Sign in with Google`.
fn body_height(shown: &Shown<'_>, rows: usize) -> f32 {
    let accounts = ACCOUNTS_LIST + shown.accounts.len() as f32 * ACCOUNT_PITCH
        - (ACCOUNT_PITCH - ACCOUNT_ROW)
        + SIGN_IN_GAP
        + SIGN_IN.line_height;
    links_top(shown)
        + RADIOS_BELOW
        + (MEETING_LINKS.len() as f32 - 1.0) * ROW
        + 20.0
        + ACCOUNTS_GAP
        + options_cache::height(shown.cache, rows)
        + NOTIFICATIONS_HEIGHT
        + accounts
}

/// Where `Meeting Links`' box starts: after the options and, while that option is on, the
/// secondary time zone's input.
fn links_top(shown: &Shown<'_>) -> f32 {
    let zone = if shown.on(Setting::SecondaryZone) {
        ROW
    } else {
        0.0
    };
    CHECK_TOP + (OPTIONS.len() as f32 - 1.0) * ROW + zone + LINKS_BELOW
}

/// Paints the dialog over `area` while `open` (and as it fades), and returns what was changed.
pub fn show(
    ctx: &egui::Context,
    area: Rect,
    open: bool,
    shown: &Shown<'_>,
    zone: &mut ZoneInput,
    cache: &mut CacheInput,
) -> Vec<OptionsAction> {
    // The people shown fill what the window leaves.
    let rest = body_height(shown, 0);
    let rows = options_cache::people_rows(area.height(), rest);
    let response = Dialog::new("calendar-settings", "Calendar", body_height(shown, rows)).show(
        ctx,
        area,
        open,
        |ui, body| body_ui(ui, body, shown, zone, cache, rows),
    );
    let mut actions = response.inner.unwrap_or_default();
    if response.close {
        actions.push(OptionsAction::Close);
    }
    actions
}

fn body_ui(
    ui: &mut Ui,
    body: Rect,
    shown: &Shown<'_>,
    zone: &mut ZoneInput,
    cache: &mut CacheInput,
    rows: usize,
) -> Vec<OptionsAction> {
    let palette = exo_ui::palette(ui.ctx());
    let mut actions = Vec::new();
    let left = body.left();
    let text = palette.ink(0.8, 0.8);

    HEADING.paint_line(
        ui.painter(),
        pos2(left, body.top() + OPTIONS_TOP + NUDGE),
        "Options",
        palette.text_base_default,
        None,
    );
    let mut top = body.top() + CHECK_TOP;
    for (option, label) in OPTIONS {
        let row = Rect::from_min_size(pos2(left, top), vec2(body.width(), CHECK_LABEL.line_height));
        if ui
            .interact(
                row,
                exo_ui::id(ui.ctx(), ("calendar-settings-option", label)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            actions.push(OptionsAction::Toggle(option));
        }
        exo_ui::paint_checkbox(
            ui.painter(),
            pos2(left, top + 2.5),
            shown.on(option),
            palette,
        );
        CHECK_LABEL.paint_line(
            ui.painter(),
            pos2(left + LABEL_LEFT, top),
            label,
            text,
            None,
        );
        top += ROW;
    }
    if shown.on(Setting::SecondaryZone)
        && let Some(iana) = zone_input(
            ui,
            pos2(left + ZONE_LEFT, top - ROW + ZONE_BELOW),
            shown,
            zone,
        )
    {
        actions.push(OptionsAction::SecondaryZone(iana));
    }

    let links = body.top() + links_top(shown);
    HEADING.paint_line(
        ui.painter(),
        pos2(left, links + NUDGE),
        "Meeting Links",
        palette.text_base_default,
        None,
    );
    let mut top = links + RADIOS_BELOW;
    for (value, label) in MEETING_LINKS {
        let is_selected = match value {
            Some(_) => matches!(shown.meeting_link, Some("google" | "teamsForBusiness")),
            None => shown.meeting_link.is_none(),
        };
        let row = Rect::from_min_size(pos2(left, top), vec2(body.width(), TEXT.line_height));
        if ui
            .interact(
                row,
                exo_ui::id(ui.ctx(), ("calendar-settings-link", label)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
            && !is_selected
        {
            actions.push(OptionsAction::MeetingLink(value.map(str::to_owned)));
        }
        // Classic's radio.
        exo_ui::paint_radio(ui.painter(), pos2(left + 1.0, top + 3.0), is_selected);
        TEXT.paint_line(
            ui.painter(),
            pos2(left + RADIO_LABEL_LEFT, top),
            label,
            text,
            Some(body.width() - RADIO_LABEL_LEFT),
        );
        top += ROW;
    }
    let cache_top = top - ROW + TEXT.line_height + ACCOUNTS_GAP;
    actions.extend(
        options_cache::show(
            ui,
            (left, body.width(), cache_top),
            shown.cache,
            cache,
            shown.book,
            rows,
            &shown.status,
        )
        .into_iter()
        .map(OptionsAction::Cache),
    );
    let notifications = cache_top + options_cache::height(shown.cache, rows);

    // `Notifications`: a link to its dialog.
    HEADING.paint_line(
        ui.painter(),
        pos2(left, notifications + NUDGE),
        "Notifications",
        palette.text_base_default,
        None,
    );
    let button = pos2(left, notifications + NOTIFICATIONS_BUTTON);
    let width = TEXT.paint_line(
        ui.painter(),
        button,
        "Manage notification settings",
        LINK_INK,
        None,
    );
    if ui
        .interact(
            Rect::from_min_size(button, vec2(width, TEXT.line_height)),
            exo_ui::id(ui.ctx(), "calendar-settings-notifications"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        actions.push(OptionsAction::Notifications);
    }
    let accounts = notifications + NOTIFICATIONS_HEIGHT;

    // `Accounts`: each account with its initial on its colour, then the way to sign another in.
    HEADING.paint_line(
        ui.painter(),
        pos2(left, accounts + NUDGE),
        "Accounts",
        palette.text_base_default,
        None,
    );
    let mut row_top = accounts + ACCOUNTS_LIST;
    for account in shown.accounts {
        let row = Rect::from_min_size(pos2(left, row_top), vec2(body.width(), ACCOUNT_ROW));
        let hovered =
            !account.primary && ui.rect_contains_pointer(row.expand2(vec2(ROW_OUTSET, 0.0)));
        if hovered {
            let fill = if exo_ui::theme(ui.ctx()) == exo_ui::Theme::Carbon {
                ROW_HOVER_CARBON
            } else {
                ROW_HOVER
            };
            ui.painter().rect_filled(
                row.expand2(vec2(ROW_OUTSET, 0.0)),
                CornerRadius::same(2),
                fill,
            );
        }
        let avatar = Rect::from_min_size(pos2(left, row_top + 4.0), Vec2::splat(16.0));
        ui.painter()
            .circle_filled(avatar.center(), 8.0, account.color);
        let initial = crate::accounts::initial(account.email);
        let width = INITIAL.width(ui.ctx(), &initial);
        // A pixel under the circle's middle.
        INITIAL.paint_line(
            ui.painter(),
            pos2(
                avatar.center().x - width / 2.0,
                avatar.center().y - INITIAL.line_height / 2.0 + 1.0,
            ),
            &initial,
            Color32::WHITE,
            None,
        );
        ACCOUNT.paint_line(
            ui.painter(),
            pos2(left + LABEL_LEFT, row_top + 1.0),
            account.email,
            text,
            None,
        );
        if !account.primary {
            let label = "Remove Account";
            let width = ACCOUNT.width(ui.ctx(), label);
            let button = Rect::from_min_size(
                pos2(body.right() - width, row_top + 1.0),
                vec2(width, ACCOUNT_ROW),
            );
            let hit = ui
                .interact(
                    button,
                    exo_ui::id(ui.ctx(), ("calendar-settings-remove", account.key)),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if hovered {
                ACCOUNT.paint_line(ui.painter(), button.min, label, REMOVE_INK, None);
            }
            if hit.clicked() {
                actions.push(OptionsAction::Remove(account.key.to_owned()));
            }
        }
        row_top += ACCOUNT_PITCH;
    }
    // Signing another calendar account in, with Google or Microsoft (icon + label, 16px apart).
    let sign_in = pos2(left, row_top - (ACCOUNT_PITCH - ACCOUNT_ROW) + SIGN_IN_GAP);
    let (google_width, google_clicked) = sign_in_button(
        ui,
        sign_in,
        "legacy/DEPRECATED_google",
        "Sign in with Google",
        "calendar-settings-sign-in-google",
    );
    if google_clicked {
        actions.push(OptionsAction::SignIn(exo_auth::Provider::Google));
    }
    let (_, microsoft_clicked) = sign_in_button(
        ui,
        pos2(sign_in.x + google_width + SIGN_IN_BUTTON_GAP, sign_in.y),
        "legacy/DEPRECATED_microsoft",
        "Sign in with Microsoft",
        "calendar-settings-sign-in-microsoft",
    );
    if microsoft_clicked {
        actions.push(OptionsAction::SignIn(exo_auth::Provider::Microsoft));
    }
    actions
}

/// Paints a `Sign in with …` button (16px icon slot + label) at `origin` and returns
/// `(total_width, clicked)`.
fn sign_in_button(
    ui: &mut Ui,
    origin: egui::Pos2,
    icon: &str,
    label: &str,
    id_salt: &str,
) -> (f32, bool) {
    let icon_box = Rect::from_min_size(origin, Vec2::splat(16.0));
    exo_ui::paint_icon(
        ui,
        icon,
        exo_ui::icon_rect(ui.ctx(), icon, icon_box),
        Color32::WHITE,
    );
    let text_width = SIGN_IN.paint_line(
        ui.painter(),
        pos2(origin.x + LABEL_LEFT, origin.y + 1.0),
        label,
        LINK_INK,
        None,
    );
    let total = LABEL_LEFT + text_width;
    let clicked = ui
        .interact(
            Rect::from_min_size(origin, vec2(total, SIGN_IN.line_height)),
            exo_ui::id(ui.ctx(), id_salt),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked();
    (total, clicked)
}

/// The secondary time zone's input at `origin`, with its rule always showing. Returns the zone
/// picked.
fn zone_input(
    ui: &mut Ui,
    origin: egui::Pos2,
    shown: &Shown<'_>,
    zone: &mut ZoneInput,
) -> Option<String> {
    let palette = exo_ui::palette(ui.ctx());
    let field = timezones::ZoneField {
        id: exo_ui::id(ui.ctx(), "calendar-settings-zone"),
        line: Rect::from_min_size(origin, vec2(ZONE_WIDTH, ZONE_TEXT_HEIGHT)),
        zone: shown.zone,
        now: shown.now,
        limit: ZONES,
        ink: palette.ink(0.8, 0.8),
        placeholder: palette.ink(0.35, 0.35),
        show_rule: true,
        always_ruled: true,
        blur_on_pick: true,
        list_width: ZONE_LIST_WIDTH,
        list_offset: timezones::DEFAULT_LIST_OFFSET,
        rows: timezones::ZoneRows::SIDEBAR,
    };
    zone.show(ui, &field).picked
}
