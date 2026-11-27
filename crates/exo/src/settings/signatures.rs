//! The Signatures dialog shows a send-as alias's Gmail signature without editing it (Gmail's
//! settings do that), and sets whether replies and forwards carry it.
//!
//! - `Signature for:` and the aliases, the default first. They're a dropdown when there are
//!   several, with its caret showing while it has the keyboard or the pointer.
//! - The signature in Snow's email styles, laid out by the body renderer, after a 4px bar. Under
//!   the pointer, a dashed card says where to edit it, and `Gmail settings` opens Gmail's. For an
//!   alias without a signature, the card says where to add one.
//! - `Include signature on replies and forwards` (`includeSignatureOnReplies`), only for an alias
//!   with a signature. It's a row that takes the keyboard as the pointer comes onto it, and
//!   toggles.
//!
//! The keyboard starts on the dropdown with several aliases, else on the row. Tab goes on to
//! `Gmail settings` and the row.

use egui::{Color32, Context, CornerRadius, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use exo_commands::{Command, Commands, Region};
use exo_editor::{BodyText, Editor, EditorStyle};
use exo_store::{Alias, Contact, MessageDetail};
use exo_ui::{
    ActionKind, DIALOG_ACTION_HEIGHT, Dialog, DialogAction, DialogDropdown, DialogMenuKind,
    DialogResponse, DialogSelect, DialogSelectEvent, Family, Font, TextStyle, motion,
};

use crate::{
    account::Loader,
    body::{Bodies, Look},
};

/// `Signature for:` is 12px/20px at `.7`, 3px down its line. The dropdown's value (12px/16px)
/// sits 1px past its `margin-right: 4px` and 2px lower.
const LABEL: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const LABEL_TOP: f32 = 27.0;
const VALUE: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
const VALUE_LEFT: f32 = 5.0;
const VALUE_TOP: f32 = 29.0;
const LABEL_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 179);
/// The signature's block sits 60px down (`padding-top: 16px` under the 44px row) and is at least
/// 48px tall (`min-height: 48px`). Its bar is 4px wide, with the signature 12px past it.
const BLOCK_TOP: f32 = 60.0;
const BLOCK_MIN: f32 = 48.0;
const BAR: f32 = 4.0;
const CONTENT_LEFT: f32 = 16.0;
/// The signature's left bar and the card's `rgba(250, 250, 250, .95)`.
const PALE: Color32 = Color32::from_rgba_premultiplied(238, 238, 238, 242);
/// The card's `1px dashed #ddd` with 4px corners, and its text: 12px/16px at `.7`, 1px down,
/// centred, the link `#54acdc`. It fades in over 100ms.
const DASH: Color32 = Color32::from_rgb(0xdd, 0xdd, 0xdd);
const HELP: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
const HELP_INK: Color32 = LABEL_INK;
const LINK_INK: Color32 = Color32::from_rgb(0x54, 0xac, 0xdc);
const FADE: f32 = 0.1;
/// The rule: 16px under the block, a 1px `#ededed` line 3px down.
const RULE_GAP: f32 = 16.0;
const RULE_INSET: f32 = 3.0;
const RULE: Color32 = Color32::from_rgb(0xed, 0xed, 0xed);
/// The row sits 20px under the rule's 4px and is 44px tall. Its label is 12px/20px semibold at `.7`
/// spaced 0.2px, and its 14px checkbox sits at the right. They're 14px and 15.5px down (measured:
/// Chromium lands the label's 14.5px and the box's 16.5px on the device pixels above). Focused, the
/// row has a `#aeb1dd` bar 3px wide at the dialog's edge and the same colour at 15% across the
/// dialog, 4px down and 40px tall.
const ROW_GAP: f32 = 24.0;
const ROW_HEIGHT: f32 = 44.0;
const ROW_LABEL: TextStyle =
    TextStyle::new(Font::new(Family::Sans, 600), 12.0, 20.0).letter_spacing(0.2);
const ROW_LABEL_TOP: f32 = 14.0;
const CHECKBOX_TOP: f32 = 15.5;
const CHECKBOX: f32 = 14.0;
const FOCUS: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);
const FOCUS_TOP: f32 = 4.0;
const FOCUS_HEIGHT: f32 = 40.0;
const FOCUS_BAR: f32 = 3.0;
/// The dialog's `padding: 20px 32px`, which the focus colour spans.
const DIALOG_PADDING: f32 = 32.0;
/// The signature's width: the dialog's content, less the bar.
const CONTENT_WIDTH: f32 = exo_ui::DIALOG_WIDTH - 2.0 * DIALOG_PADDING - CONTENT_LEFT;
const ON_REPLIES: &str = "Include signature on replies and forwards";
/// Outlook signature dialog subheader and editor metrics.
const OUTLOOK_SUBHEADER: &str = "Your signature will be automatically included in all messages.";
const OUTLOOK_SUBHEADER_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 217);
const OUTLOOK_EDITOR_TOP: f32 = 60.0;
const OUTLOOK_EDITOR_HEIGHT: f32 = 164.0;
const OUTLOOK_EDITOR_GUTTER: f32 = 14.0;
const OUTLOOK_PLACEHOLDER: &str = "Paste your signature here";
const OUTLOOK_ACTIONS_GAP: f32 = 24.0;
const OUTLOOK_ACTIONS_BOTTOM: f32 = 4.0;
const OUTLOOK_ACTION_GAP: f32 = 8.0;
const OUTLOOK_ACTIONS_OVERHANG: f32 = 13.0;

/// What has the keyboard in Gmail's read-only dialog, in Tab's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The aliases' dropdown.
    Dropdown,
    /// `Gmail settings`, in the card, which takes the keyboard though the card only shows under
    /// the pointer.
    Link,
    /// `Include signature on replies and forwards`.
    OnReplies,
}

/// What has the keyboard in Outlook's editable signature dialog, in Tab's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalFocus {
    /// The rich-text signature editor.
    Editor,
    /// `Include signature on replies and forwards`.
    OnReplies,
    /// `Cancel`.
    Cancel,
    /// `Save`.
    Save,
}

/// Something done in the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Tab (1) or ⇧Tab (-1).
    Tab(isize),
    /// The pointer came onto something that takes the keyboard.
    Focus(Focus),
    /// Focus moved inside Outlook's signature dialog.
    FocusLocal(LocalFocus),
    /// Open or close the list (a click, Enter or an arrow on the dropdown).
    ToggleMenu,
    /// ↓ (1) or ↑ (-1) in the open list.
    MenuMove(isize),
    /// Escape with the list open.
    MenuClose,
    /// The pointer on a row of the list.
    Highlight(usize),
    /// Enter or Tab on the open list, which picks its highlighted alias.
    MenuPick,
    /// An alias, by address.
    Choose(String),
    /// Enter or Space on what has the keyboard.
    Activate,
    /// `Include signature on replies and forwards`, clicked.
    ToggleOnReplies,
    /// `Gmail settings`, clicked.
    OpenGmail,
    /// `Save` in Outlook's signature dialog.
    Save,
    /// `Cancel` in Outlook's signature dialog.
    Cancel,
}

/// Editable signature state for Outlook accounts.
struct LocalState {
    editor: Editor,
    focus: LocalFocus,
    focus_pending: bool,
}

/// The dialog's alias, keyboard and list (or local rich-text editor on Outlook).
#[derive(Default)]
pub struct State {
    /// The alias shown, which is the default one until another is chosen.
    selected: Option<String>,
    focus: Option<Focus>,
    /// The list is open, with this row highlighted.
    menu: Option<usize>,
    /// Present when editing a local Outlook signature instead of previewing Gmail's `sendAs`.
    local: Option<LocalState>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("selected", &self.selected)
            .field("focus", &self.focus)
            .field("menu", &self.menu)
            .field("local", &self.local.as_ref().map(|local| local.focus))
            .finish()
    }
}

/// The aliases as the dropdown lists them: active ones, the shown one first and the rest by
/// address.
fn listed<'a>(aliases: &'a [Alias], shown: &str) -> Vec<&'a Alias> {
    let mut listed: Vec<&Alias> = aliases.iter().filter(|alias| alias.active).collect();
    listed.sort_by_cached_key(|alias| {
        format!(
            "{}{}",
            if alias.email == shown { 'a' } else { 'b' },
            alias.email
        )
    });
    listed
}

/// The alias shown: the one chosen, else the default, else the first.
fn shown<'a>(selected: Option<&str>, aliases: &'a [Alias]) -> Option<&'a Alias> {
    let chosen = selected.and_then(|email| aliases.iter().find(|alias| alias.email == email));
    chosen
        .or_else(|| aliases.iter().find(|alias| alias.default))
        .or_else(|| aliases.first())
}

impl State {
    /// The dialog opening on `aliases` (Gmail read-only mode).
    pub fn open(aliases: &[Alias]) -> Self {
        let mut state = Self::default();
        state.focus = state.first_focus(aliases);
        state
    }

    /// The dialog opening for an Outlook account with its saved local signature HTML, if any.
    pub fn open_outlook(signature: Option<&str>) -> Self {
        let mut editor = Editor::new();
        editor.set_body_text(BodyText::SMALL);
        if let Some(html) = signature.filter(|html| !html.trim().is_empty()) {
            editor.set_html(html);
        }
        Self {
            local: Some(LocalState {
                editor,
                focus: LocalFocus::Editor,
                focus_pending: true,
            }),
            ..Self::default()
        }
    }

    /// The sanitized local signature HTML ready to save, or `None` when empty.
    pub fn cleaned_html(&self) -> Option<String> {
        let local = self.local.as_ref()?;
        clean_signature_html(&local.editor.html())
    }

    /// Where the keyboard starts: the dropdown with several aliases, else the row when there's one.
    fn first_focus(&self, aliases: &[Alias]) -> Option<Focus> {
        if listed(aliases, "").len() > 1 {
            Some(Focus::Dropdown)
        } else {
            shown(self.selected.as_deref(), aliases)
                .is_some_and(has_signature)
                .then_some(Focus::OnReplies)
        }
    }

    /// Whether the list is open, so that Escape closes it rather than the dialog.
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// What has the keyboard in Gmail mode.
    pub fn focus(&self) -> Option<Focus> {
        self.focus
    }

    /// Registers the dialog's keyboard bindings in `Region::Dialog`.
    pub fn register_keys<A: Clone + 'static>(
        &self,
        commands: &mut Commands<A>,
        wrap: &impl Fn(super::Action) -> A,
    ) {
        const DIALOG: &[Region] = &[Region::Dialog];
        let key = |id: &'static str, event: Event, keys: &[&'static str]| {
            Command::shortcut(id, wrap(super::Action::Signatures(event)))
                .keys(keys.iter().copied())
                .regions(DIALOG)
        };
        if let Some(local) = &self.local {
            commands.add(key("SIGNATURES_TAB", Event::Tab(1), &["tab"]));
            commands.add(key("SIGNATURES_SHIFT_TAB", Event::Tab(-1), &["shift+tab"]));
            commands.add(key("SIGNATURES_SAVE", Event::Save, &["mod+enter"]));
            // Leave Enter and Space to the rich-text editor while typing in it.
            if local.focus != LocalFocus::Editor {
                commands.add(key(
                    "SIGNATURES_ACTIVATE",
                    Event::Activate,
                    &["enter", "space"],
                ));
            }
            return;
        }
        if self.menu_open() {
            commands.add(key(
                "SIGNATURES_MENU_DOWN",
                Event::MenuMove(1),
                &["down", "j", "n"],
            ));
            commands.add(key(
                "SIGNATURES_MENU_UP",
                Event::MenuMove(-1),
                &["up", "k", "p"],
            ));
            commands.add(key(
                "SIGNATURES_MENU_PICK",
                Event::MenuPick,
                &["enter", "tab"],
            ));
            commands.add(key("SIGNATURES_MENU_ESCAPE", Event::MenuClose, &["esc"]));
        } else {
            if self.focus() == Some(Focus::Dropdown) {
                commands.add(key(
                    "SIGNATURES_MENU_OPEN",
                    Event::ToggleMenu,
                    &["down", "j", "n", "up", "k", "p"],
                ));
            }
            commands.add(key("SIGNATURES_TAB", Event::Tab(1), &["tab"]));
            commands.add(key("SIGNATURES_SHIFT_TAB", Event::Tab(-1), &["shift+tab"]));
            commands.add(key(
                "SIGNATURES_ACTIVATE",
                Event::Activate,
                &["enter", "space"],
            ));
        }
    }

    /// Applies `event` and returns what's left for the app, which is toggling the setting,
    /// opening Gmail, or saving/cancelling Outlook's local signature.
    pub fn handle(&mut self, event: Event, aliases: &[Alias]) -> Option<Event> {
        if let Some(local) = &mut self.local {
            const ORDER: [LocalFocus; 4] = [
                LocalFocus::Editor,
                LocalFocus::OnReplies,
                LocalFocus::Cancel,
                LocalFocus::Save,
            ];
            return match event {
                Event::Tab(step) => {
                    let at = ORDER
                        .iter()
                        .position(|candidate| *candidate == local.focus)
                        .unwrap_or(0);
                    let next =
                        ORDER[(at as isize + step).rem_euclid(ORDER.len() as isize) as usize];
                    local.focus = next;
                    local.focus_pending = next == LocalFocus::Editor;
                    None
                }
                Event::FocusLocal(focus) => {
                    local.focus = focus;
                    local.focus_pending = focus == LocalFocus::Editor;
                    None
                }
                Event::Activate => match local.focus {
                    LocalFocus::Editor => None,
                    LocalFocus::OnReplies => Some(Event::ToggleOnReplies),
                    LocalFocus::Cancel => Some(Event::Cancel),
                    LocalFocus::Save => Some(Event::Save),
                },
                Event::ToggleOnReplies | Event::Save | Event::Cancel => Some(event),
                _ => None,
            };
        }
        let shown_email = shown(self.selected.as_deref(), aliases)
            .map(|alias| alias.email.clone())
            .unwrap_or_default();
        let listed = listed(aliases, &shown_email);
        let signed = shown(self.selected.as_deref(), aliases).is_some_and(has_signature);
        let focusable: Vec<Focus> = [
            (listed.len() > 1).then_some(Focus::Dropdown),
            Some(Focus::Link),
            signed.then_some(Focus::OnReplies),
        ]
        .into_iter()
        .flatten()
        .collect();
        match event {
            Event::Tab(step) => {
                self.menu = None;
                let at = self
                    .focus
                    .and_then(|focus| focusable.iter().position(|candidate| *candidate == focus))
                    .map_or(0, |at| {
                        (at as isize + step).rem_euclid(focusable.len() as isize) as usize
                    });
                self.focus = Some(focusable[at]);
            }
            Event::Focus(focus) => self.focus = Some(focus),
            Event::ToggleMenu => {
                self.focus = Some(Focus::Dropdown);
                self.menu = if self.menu.is_some() { None } else { Some(0) };
            }
            Event::MenuMove(step) => {
                self.menu = self.menu.map(|at| {
                    (at as isize + step).rem_euclid(listed.len().max(1) as isize) as usize
                })
            }
            Event::MenuClose => self.menu = None,
            Event::Highlight(row) => self.menu = Some(row),
            Event::MenuPick => {
                let picked = self
                    .menu
                    .take()
                    .and_then(|at| listed.get(at))
                    .map(|alias| alias.email.clone());
                if let Some(email) = picked {
                    self.selected = Some(email);
                }
            }
            Event::Choose(email) => {
                self.menu = None;
                self.selected = Some(email);
            }
            Event::Activate => match self.focus {
                Some(Focus::Dropdown) => {
                    self.menu = if self.menu.is_some() { None } else { Some(0) }
                }
                Some(Focus::OnReplies) => return Some(Event::ToggleOnReplies),
                Some(Focus::Link) => return Some(Event::OpenGmail),
                None => {}
            },
            Event::ToggleOnReplies | Event::OpenGmail | Event::Save | Event::Cancel => {
                return Some(event);
            }
            Event::FocusLocal(_) => {}
        }
        // An alias without a signature has no row to keep the keyboard.
        if self.focus == Some(Focus::OnReplies)
            && !shown(self.selected.as_deref(), aliases).is_some_and(has_signature)
        {
            self.focus = self.first_focus(aliases);
        }
        None
    }
}

/// Sanitizes edited signature HTML, trims trailing contentless blocks, and returns `None` when no
/// visible text or image remains.
pub fn clean_signature_html(html: &str) -> Option<String> {
    let sanitized = exo_mail::sanitize_fragment(html);
    let mut trimmed = sanitized.trim();
    loop {
        let next = [
            "<div><br></div>",
            "<div><br/></div>",
            "<div><br /></div>",
            "<p><br></p>",
            "<p><br/></p>",
            "<p><br /></p>",
            "<div></div>",
            "<p></p>",
            "<br>",
            "<br/>",
            "<br />",
        ]
        .iter()
        .find_map(|suffix| trimmed.strip_suffix(suffix))
        .map(str::trim);
        match next {
            Some(shorter) => trimmed = shorter,
            None => break,
        }
    }
    if trimmed.is_empty() {
        return None;
    }
    let has_content =
        trimmed.contains("<img") || !crate::compose::plain::from_html(trimmed).trim().is_empty();
    has_content.then(|| trimmed.to_owned())
}

/// Whether `alias` has a signature to show.
fn has_signature(alias: &Alias) -> bool {
    alias
        .signature
        .as_deref()
        .is_some_and(|signature| !signature.is_empty())
}

/// What the dialog draws from.
pub struct Shown<'a> {
    pub aliases: &'a [Alias],
    /// `includeSignatureOnReplies`.
    pub on_replies: bool,
    /// The account's address, for the body renderer's own domain and Gmail's link.
    pub account: &'a str,
    pub loader: &'a Loader,
}

/// Draws the dialog over `area` while `open` (and as it fades), with its signature drawn by
/// `preview`, and returns what was done.
pub fn dialog(
    ctx: &Context,
    area: Rect,
    open: bool,
    state: &mut State,
    preview: &mut Preview,
    shown: &Shown<'_>,
) -> DialogResponse<Option<Event>> {
    if let Some(local) = &mut state.local {
        let rule = OUTLOOK_EDITOR_TOP + OUTLOOK_EDITOR_HEIGHT + RULE_GAP;
        let row_top = rule + ROW_GAP;
        let actions_top = row_top + ROW_HEIGHT + OUTLOOK_ACTIONS_GAP;
        let body_height = actions_top + DIALOG_ACTION_HEIGHT + OUTLOOK_ACTIONS_BOTTOM;
        return Dialog::new("signatures", "Signature", body_height).show(
            ctx,
            area,
            open,
            |ui, body| draw_outlook(ui, body, local, shown.on_replies),
        );
    }
    let alias = self::shown(state.selected.as_deref(), shown.aliases);
    let signature = alias.filter(|alias| has_signature(alias));
    // The signature's left edge (the card is centred on a device pixel), for its glyphs' phase.
    let ppp = ctx.pixels_per_point();
    let left = ((area.center().x - exo_ui::DIALOG_WIDTH / 2.0) * ppp).round() / ppp
        + DIALOG_PADDING
        + CONTENT_LEFT;
    let look = Look {
        width: CONTENT_WIDTH,
        scale: ppp,
        // The signature renders in Snow, in Carbon too, on the dialog's card.
        theme: exo_ui::Theme::Snow,
        rem: exo_ui::rem(area.width(), false),
        show_quoted: true,
        hide_original: false,
        phase: left - left.floor(),
        // On the dialog's card (`#fff`, `#f5f5f5` in Carbon), since the signature has no
        // background of its own.
        background: match exo_ui::theme(ctx) {
            exo_ui::Theme::Snow => Color32::WHITE,
            exo_ui::Theme::Carbon => Color32::from_rgb(0xf5, 0xf5, 0xf5),
        }
        .to_array(),
    };
    // The signature's height once laid out, or a line's until then.
    let content = signature.map_or(0.0, |alias| {
        preview.height(ctx, alias, shown, look).unwrap_or(24.0)
    });
    let block = content.max(BLOCK_MIN);
    let rule = BLOCK_TOP + block + RULE_GAP;
    let body_height = if signature.is_some() {
        rule + ROW_GAP + ROW_HEIGHT
    } else {
        rule + RULE_INSET + 1.0
    };
    Dialog::new("signatures", "Signatures", body_height).show(ctx, area, open, |ui, body| {
        draw(ui, body, state, preview, shown, block)
    })
}

fn draw(
    ui: &mut Ui,
    body: Rect,
    state: &State,
    preview: &mut Preview,
    shown: &Shown<'_>,
    block: f32,
) -> Option<Event> {
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let (left, right, top) = (body.left(), body.right(), body.top());
    let alias = self::shown(state.selected.as_deref(), shown.aliases);
    let email = alias.map_or(shown.account, |alias| alias.email.as_str());
    let listed = listed(shown.aliases, email);
    let mut event = None;

    // `Signature for:` and the aliases.
    let label = LABEL.paint_line(
        &painter,
        pos2(left, top + LABEL_TOP),
        "Signature for:",
        LABEL_INK,
        None,
    );
    let value = pos2(left + label + VALUE_LEFT, top + VALUE_TOP);
    if listed.len() > 1 {
        let id = exo_ui::id(&ctx, "signatures-alias");
        let hovered = ctx
            .read_response(id)
            .is_some_and(|response| response.hovered());
        // The dropdown shows its caret focused or hovered, at half opacity.
        let lit = motion::transition(
            &ctx,
            id.with("caret"),
            state.focus == Some(Focus::Dropdown) || hovered,
            0.15,
            motion::CubicBezier::EASE,
        );
        let style = DialogDropdown {
            value: LABEL_INK,
            caret: Color32::BLACK.gamma_multiply(0.5 * lit),
            gap: 10.0,
        };
        let items: Vec<&str> = listed.iter().map(|alias| alias.email.as_str()).collect();
        if let Some(select_event) = DialogSelect::new(id, value, email, &items, state.menu)
            .kind(DialogMenuKind::Addresses)
            .style(style)
            .focused(state.focus == Some(Focus::Dropdown))
            .show(ui)
        {
            event = Some(match select_event {
                DialogSelectEvent::Toggle => Event::ToggleMenu,
                DialogSelectEvent::Highlight(row) => Event::Highlight(row),
                DialogSelectEvent::Pick(row) => Event::Choose(items[row].to_owned()),
                DialogSelectEvent::Close => Event::MenuClose,
            });
        }
    } else {
        VALUE.paint_line(&painter, value, email, LABEL_INK, None);
    }

    // The signature, or where to add one.
    let card = Rect::from_min_size(pos2(left, top + BLOCK_TOP), vec2(right - left, block));
    let signature = alias.filter(|alias| has_signature(alias));
    let hovered = signature.is_some() && ui.rect_contains_pointer(card) && state.menu.is_none();
    if let Some(alias) = signature {
        painter.rect_filled(Rect::from_min_size(card.min, vec2(BAR, block)), 0.0, PALE);
        preview.paint(&painter, alias, pos2(left + CONTENT_LEFT, card.top()), card);
    }
    let shows = if signature.is_some() {
        motion::transition(
            &ctx,
            exo_ui::id(&ctx, "signatures-card"),
            hovered,
            FADE,
            motion::CubicBezier::EASE,
        )
    } else {
        1.0
    };
    if shows > 0.0 {
        let text = if signature.is_some() {
            (
                "To edit this signature, please visit ",
                " and then reload Exo.",
            )
        } else {
            ("To add a signature, please visit ", " and then reload.")
        };
        if help_card(ui, card, text, shows) {
            event = Some(Event::OpenGmail);
        }
    }

    painter.hline(
        left..=right,
        card.bottom() + RULE_GAP + RULE_INSET + 0.5,
        Stroke::new(1.0, RULE),
    );
    if signature.is_none() {
        return event;
    }

    // `Include signature on replies and forwards`.
    let row = Rect::from_min_size(
        pos2(left, card.bottom() + RULE_GAP + ROW_GAP),
        vec2(right - left, ROW_HEIGHT),
    );
    let response = ui
        .interact(
            row,
            exo_ui::id(ui.ctx(), "signatures-on-replies"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() && state.focus != Some(Focus::OnReplies) && state.menu.is_none() {
        event = event.or(Some(Event::Focus(Focus::OnReplies)));
    }
    if state.focus == Some(Focus::OnReplies) {
        let band = Rect::from_x_y_ranges(
            left - DIALOG_PADDING..=right + DIALOG_PADDING,
            row.top() + FOCUS_TOP..=row.top() + FOCUS_TOP + FOCUS_HEIGHT,
        );
        painter.rect_filled(band, 0.0, FOCUS.gamma_multiply(0.15));
        painter.rect_filled(
            Rect::from_min_size(band.min, vec2(FOCUS_BAR, FOCUS_HEIGHT)),
            0.0,
            FOCUS,
        );
    }
    ROW_LABEL.paint_line(
        &painter,
        pos2(left, row.top() + ROW_LABEL_TOP),
        ON_REPLIES,
        LABEL_INK,
        None,
    );
    exo_ui::paint_checkbox(
        &painter,
        pos2(right - CHECKBOX, row.top() + CHECKBOX_TOP),
        shown.on_replies,
        exo_ui::palette(&ctx),
    );
    if response.clicked() {
        event = Some(Event::ToggleOnReplies);
    }
    event
}

fn draw_outlook(
    ui: &mut Ui,
    body: Rect,
    local: &mut LocalState,
    on_replies: bool,
) -> Option<Event> {
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let (left, right, top) = (body.left(), body.right(), body.top());
    let mut event = None;

    LABEL.paint_line(
        &painter,
        pos2(left, top + LABEL_TOP),
        OUTLOOK_SUBHEADER,
        OUTLOOK_SUBHEADER_INK,
        None,
    );

    let editor_rect = Rect::from_min_size(
        pos2(left, top + OUTLOOK_EDITOR_TOP),
        vec2(
            (right - left - OUTLOOK_EDITOR_GUTTER).max(1.0),
            OUTLOOK_EDITOR_HEIGHT,
        ),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(editor_rect)
            .id_salt("outlook-signature-editor"),
    );
    child.set_clip_rect(editor_rect.intersect(ui.clip_rect()));
    let output = egui::ScrollArea::vertical()
        .id_salt("outlook-signature-scroll")
        .max_height(OUTLOOK_EDITOR_HEIGHT)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut child, |ui| {
            let style = EditorStyle {
                placeholder: Some(OUTLOOK_PLACEHOLDER.into()),
                min_height: OUTLOOK_EDITOR_HEIGHT,
            };
            let edited = local.editor.ui(ui, &style);
            (
                edited.response.id,
                edited.response.clicked() || edited.response.gained_focus(),
            )
        });
    let (editor_id, editor_clicked) = output.inner;
    if editor_clicked && local.focus != LocalFocus::Editor {
        event = Some(Event::FocusLocal(LocalFocus::Editor));
    }
    if local.focus == LocalFocus::Editor {
        if std::mem::take(&mut local.focus_pending) || !ctx.memory(|m| m.has_focus(editor_id)) {
            ctx.memory_mut(|memory| memory.request_focus(editor_id));
        }
    } else if ctx.memory(|m| m.has_focus(editor_id)) {
        ctx.memory_mut(|memory| memory.surrender_focus(editor_id));
    }

    let rule_y = top + OUTLOOK_EDITOR_TOP + OUTLOOK_EDITOR_HEIGHT + RULE_GAP;
    painter.hline(
        left..=right,
        rule_y + RULE_INSET + 0.5,
        Stroke::new(1.0, RULE),
    );

    let row = Rect::from_min_size(pos2(left, rule_y + ROW_GAP), vec2(right - left, ROW_HEIGHT));
    let response = ui
        .interact(
            row,
            exo_ui::id(&ctx, "outlook-signatures-on-replies"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() && local.focus != LocalFocus::OnReplies {
        event = event.or(Some(Event::FocusLocal(LocalFocus::OnReplies)));
    }
    if local.focus == LocalFocus::OnReplies {
        let band = Rect::from_x_y_ranges(
            left - DIALOG_PADDING..=right + DIALOG_PADDING,
            row.top() + FOCUS_TOP..=row.top() + FOCUS_TOP + FOCUS_HEIGHT,
        );
        painter.rect_filled(band, 0.0, FOCUS.gamma_multiply(0.15));
        painter.rect_filled(
            Rect::from_min_size(band.min, vec2(FOCUS_BAR, FOCUS_HEIGHT)),
            0.0,
            FOCUS,
        );
    }
    ROW_LABEL.paint_line(
        &painter,
        pos2(left, row.top() + ROW_LABEL_TOP),
        ON_REPLIES,
        LABEL_INK,
        None,
    );
    exo_ui::paint_checkbox(
        &painter,
        pos2(right - CHECKBOX, row.top() + CHECKBOX_TOP),
        on_replies,
        exo_ui::palette(&ctx),
    );
    if response.clicked() {
        event = Some(Event::ToggleOnReplies);
    }

    let actions_top = row.bottom() + OUTLOOK_ACTIONS_GAP;
    let save = DialogAction::new("Save", ActionKind::Primary)
        .focused(matches!(local.focus, LocalFocus::Save | LocalFocus::Editor));
    let cancel = DialogAction::new("Cancel", ActionKind::Plain)
        .unfilled()
        .focused(local.focus == LocalFocus::Cancel);
    let save_width = save.width(&ctx);
    let cancel_width = cancel.width(&ctx);
    let save_rect = Rect::from_min_size(
        pos2(right + OUTLOOK_ACTIONS_OVERHANG - save_width, actions_top),
        vec2(save_width, DIALOG_ACTION_HEIGHT),
    );
    let cancel_rect = Rect::from_min_size(
        pos2(
            save_rect.left() - OUTLOOK_ACTION_GAP - cancel_width,
            actions_top,
        ),
        vec2(cancel_width, DIALOG_ACTION_HEIGHT),
    );
    if save
        .show(ui, exo_ui::id(&ctx, "outlook-signatures-save"), save_rect)
        .clicked()
    {
        event = Some(Event::Save);
    }
    if cancel
        .show(
            ui,
            exo_ui::id(&ctx, "outlook-signatures-cancel"),
            cancel_rect,
        )
        .clicked()
    {
        event = Some(Event::Cancel);
    }
    event
}

/// The dashed card over `card` at `opacity`, saying `before` `Gmail settings` `after`, centred.
/// Returns whether the link was clicked.
fn help_card(ui: &Ui, card: Rect, (before, after): (&str, &str), opacity: f32) -> bool {
    let ctx = ui.ctx();
    let mut painter = ui.painter().clone();
    painter.multiply_opacity(opacity);
    let corner = CornerRadius::same(4);
    painter.rect_filled(card, corner, PALE);
    dashed_outline(&painter, card.shrink(0.5), Stroke::new(1.0, DASH));
    let link = "Gmail settings";
    let widths = [before, link, after].map(|text| HELP.width(ctx, text));
    let mut x = card.center().x - widths.iter().sum::<f32>() / 2.0;
    let y = card.center().y - HELP.line_height / 2.0 + 1.0;
    for (text, ink, width) in [
        (before, HELP_INK, widths[0]),
        (link, LINK_INK, widths[1]),
        (after, HELP_INK, widths[2]),
    ] {
        HELP.paint_line(&painter, pos2(x, y), text, ink, None);
        x += width;
    }
    let target = Rect::from_min_size(
        pos2(
            card.center().x - widths.iter().sum::<f32>() / 2.0 + widths[0],
            y,
        ),
        vec2(widths[1], HELP.line_height),
    );
    opacity >= 1.0
        && ui
            .interact(
                target,
                exo_ui::id(ui.ctx(), "signatures-gmail"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
}

/// A `1px dashed` border around `rect`: Chromium's 3px dashes and gaps.
fn dashed_outline(painter: &egui::Painter, rect: Rect, stroke: Stroke) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    painter.extend(Shape::dashed_line(&corners, stroke, 3.0, 3.0));
}

/// The signature laid out and painted by a body renderer of its own. Its worker lives as long as
/// it does, so the app keeps one.
pub struct Preview {
    bodies: Bodies,
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preview").finish_non_exhaustive()
    }
}

impl Preview {
    /// A renderer repainting `ctx`.
    pub fn new(ctx: &Context) -> Self {
        Self {
            bodies: Bodies::new(ctx),
        }
    }

    /// The body renderer's id for `alias`'s signature.
    fn id(alias: &Alias) -> String {
        format!("signature:{}", alias.email)
    }

    /// The height of `alias`'s signature laid out with `look`, once it has been. Call it every
    /// frame, since it hands the renderer the signature (unless the renderer has it) and takes what
    /// the renderer has done.
    fn height(
        &mut self,
        ctx: &Context,
        alias: &Alias,
        shown: &Shown<'_>,
        look: Look,
    ) -> Option<f32> {
        let id = Self::id(alias);
        self.bodies
            .set_thread(&[&message(&id, alias)], shown.account, shown.loader);
        self.bodies.poll(ctx);
        self.bodies.height(&id, look)
    }

    /// Paints `alias`'s signature (laid out by [`Self::height`]) with its top left at `at`.
    fn paint(&mut self, painter: &egui::Painter, alias: &Alias, at: egui::Pos2, clip: Rect) {
        self.bodies.paint(painter, &Self::id(alias), at, clip, 1.0);
    }
}

/// `alias`'s signature as a message from it, for the body renderer.
fn message(id: &str, alias: &Alias) -> MessageDetail {
    MessageDetail {
        id: id.to_owned(),
        labels: Vec::new(),
        date: jiff::Timestamp::UNIX_EPOCH,
        from: Some(Contact {
            name: alias.name.clone(),
            email: alias.email.clone(),
            is_me: true,
        }),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: String::new(),
        snippet: String::new(),
        rfc822_id: None,
        in_reply_to: None,
        references: Vec::new(),
        list_id: None,
        list_unsubscribe: None,
        list_unsubscribe_post: None,
        headers: Vec::new(),
        body: exo_store::Body {
            html: alias.signature.clone(),
            text: None,
        },
        attachments: Vec::new(),
        failure: None,
        scheduled: None,
        sending: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases() -> Vec<Alias> {
        let alias = |email: &str, signature: Option<&str>, default| Alias {
            email: email.into(),
            name: None,
            signature: signature.map(Into::into),
            default,
            primary: default,
            active: true,
        };
        vec![
            alias("maya@brightmoor.co", Some("<b>Maya</b>"), true),
            alias("bare@mayaokafor.com", None, false),
            alias("hello@mayaokafor.com", Some("Maya"), false),
        ]
    }

    #[test]
    fn the_default_alias_shows_first_and_the_rest_by_address() {
        let aliases = aliases();
        assert_eq!(
            shown(None, &aliases).map(|alias| alias.email.as_str()),
            Some("maya@brightmoor.co")
        );
        let names = |shown: &str| {
            listed(&aliases, shown)
                .iter()
                .map(|alias| alias.email.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("maya@brightmoor.co"),
            [
                "maya@brightmoor.co",
                "bare@mayaokafor.com",
                "hello@mayaokafor.com"
            ]
        );
        assert_eq!(
            names("hello@mayaokafor.com"),
            [
                "hello@mayaokafor.com",
                "bare@mayaokafor.com",
                "maya@brightmoor.co"
            ]
        );
    }

    #[test]
    fn the_keyboard_starts_on_the_dropdown_and_tabs_to_the_row() {
        let aliases = aliases();
        let mut state = State {
            focus: None,
            ..State::default()
        };
        state.focus = state.first_focus(&aliases);
        assert_eq!(state.focus(), Some(Focus::Dropdown));
        state.handle(Event::Tab(1), &aliases);
        assert_eq!(
            state.focus(),
            Some(Focus::Link),
            "`Gmail settings`, in the card"
        );
        assert_eq!(
            state.handle(Event::Activate, &aliases),
            Some(Event::OpenGmail)
        );
        state.handle(Event::Tab(1), &aliases);
        assert_eq!(state.focus(), Some(Focus::OnReplies));
        assert_eq!(
            state.handle(Event::Activate, &aliases),
            Some(Event::ToggleOnReplies),
            "Enter toggles it"
        );
        state.handle(Event::Tab(1), &aliases);
        assert_eq!(state.focus(), Some(Focus::Dropdown), "Tab wraps");
        let one = &aliases[..1];
        assert_eq!(
            State::default().first_focus(one),
            Some(Focus::OnReplies),
            "one alias: the row"
        );
    }

    #[test]
    fn choosing_an_alias_without_a_signature_leaves_no_row() {
        let aliases = aliases();
        let mut state = State {
            focus: Some(Focus::OnReplies),
            ..State::default()
        };
        state.handle(Event::ToggleMenu, &aliases);
        state.handle(Event::MenuMove(1), &aliases);
        state.handle(Event::MenuPick, &aliases);
        assert_eq!(state.selected.as_deref(), Some("bare@mayaokafor.com"));
        assert_eq!(state.focus(), Some(Focus::Dropdown));
        assert!(!state.menu_open());
    }

    #[test]
    fn clean_signature_html_strips_trailing_empty_blocks_and_empty_input() {
        assert_eq!(clean_signature_html(""), None);
        assert_eq!(
            clean_signature_html("<div><br></div><div><br/></div>"),
            None
        );
        assert_eq!(
            clean_signature_html("<div>Best,<br>Maya</div><div><br></div>"),
            Some("<div>Best,<br>Maya</div>".into())
        );
        assert_eq!(
            clean_signature_html("<img src=\"https://example.com/logo.png\">"),
            Some("<img src=\"https://example.com/logo.png\">".into())
        );
    }

    #[test]
    fn outlook_signature_dialog_tabs_across_editor_checkbox_and_buttons() {
        let mut state = State::open_outlook(Some("<div>Best,<br>Maya</div>"));
        assert_eq!(
            state.cleaned_html().as_deref(),
            Some("<div>Best,<br>Maya</div>")
        );
        assert_eq!(
            state.handle(Event::Activate, &[]),
            None,
            "Editor keeps Enter"
        );
        state.handle(Event::Tab(1), &[]);
        assert_eq!(
            state.handle(Event::Activate, &[]),
            Some(Event::ToggleOnReplies)
        );
        state.handle(Event::Tab(1), &[]);
        assert_eq!(state.handle(Event::Activate, &[]), Some(Event::Cancel));
        state.handle(Event::Tab(1), &[]);
        assert_eq!(state.handle(Event::Activate, &[]), Some(Event::Save));
        state.handle(Event::Tab(1), &[]);
        assert_eq!(
            state.handle(Event::Activate, &[]),
            None,
            "wrapped back to editor"
        );
    }
}
