//! Dialog form controls: the footer buttons (which alerts' actions share) and the underlined text
//! input.

use egui::{Color32, CornerRadius, Id, Painter, Rect, Response, Sense, Ui, pos2, vec2};

use crate::{
    BoxShadow, Family, Font, TextStyle, Theme, app_theme, motion, palette, pixel_snap, text_field,
};

/// A dialog action: 14px/20px bold and uppercase in Classic, nudged down 3px,
/// `padding: 8px 12px 4px`, 2px corners.
const ACTION: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 20.0);
const ACTION_PADDING: [f32; 3] = [8.0, 12.0, 4.0];
const ACTION_RADIUS: u8 = 2;
/// An emoji image after an action's label: 14px, `top: 1px; vertical-align: top`.
const ACTION_EMOJI: f32 = 14.0;
/// [`DialogAction::origin_light`]'s label: Exo Sans Alt.
const ORIGIN_ACTION: TextStyle = TextStyle::new(Font::new(Family::SansAlt, 700), 14.0, 20.0);
/// [`DialogAction::origin_light`]'s label, fill and hovered fill.
const ORIGIN_LABEL: Color32 = Color32::from_rgb(0x73, 0x71, 0x6d);
const ORIGIN_FILL: Color32 = Color32::from_rgb(0x71, 0x4c, 0xb6);
const ORIGIN_FILL_HOVER: Color32 = Color32::from_rgb(0x53, 0x31, 0x92);
/// The height of a dialog action: its padding around a 20px line.
pub const DIALOG_ACTION_HEIGHT: f32 = 32.0;
/// An action's label: `rgba(0, 0, 0, .4)`, `.15` disabled, and on the accent
/// `hsla(0, 0%, 100%, .98)`. Dialogs and alerts are light in both themes.
const LABEL: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 102);
const LABEL_DISABLED: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 38);
const LABEL_FILLED: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 250);
/// [`DialogAction::dark`]'s plain, disabled and on-accent labels.
const DARK_LABELS: [Color32; 3] = [
    Color32::from_rgba_unmultiplied_const(255, 255, 255, 102),
    Color32::from_rgba_unmultiplied_const(255, 255, 255, 38),
    Color32::from_rgb(0x27, 0x29, 0x2d),
];
/// An [`DialogAction::unfilled`] label hovered or focused: `rgba(0, 0, 0, .7)`.
const LABEL_DARKENED: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 179);
/// `#aeb1dd`, and the glow selected, focused and hovered actions take: `0 0 6px 0 #aeb1dd,
/// 0 2px 12px 0 rgba(174, 177, 221, .5)`.
const ACCENT: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);
const GLOW: [BoxShadow; 2] = [
    BoxShadow::new([0.0, 0.0], 6.0, 0.0, ACCENT),
    BoxShadow::new(
        [0.0, 2.0],
        12.0,
        0.0,
        Color32::from_rgba_unmultiplied_const(0xae, 0xb1, 0xdd, 128),
    ),
];

/// How a dialog action is styled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    /// Filled.
    Primary,
    /// Subdued text that fills on hover.
    Plain,
}

/// A dialog's footer button.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub struct DialogAction<'a> {
    label: &'a str,
    emoji: Option<&'a str>,
    kind: ActionKind,
    enabled: bool,
    focused: bool,
    dark: bool,
    unfilled: bool,
    disabled_fill: Option<Color32>,
    origin_light: bool,
}

impl<'a> DialogAction<'a> {
    /// A button reading `label` (shown uppercase).
    pub fn new(label: &'a str, kind: ActionKind) -> Self {
        Self {
            label,
            emoji: None,
            kind,
            enabled: true,
            focused: false,
            dark: false,
            unfilled: false,
            disabled_fill: None,
            origin_light: false,
        }
    }

    /// Its label followed by a space and `emoji` drawn as an image (an `<img>` after the text),
    /// 14px square and 1px down from the top of the line, as Get Me To Zero's `Let's go! ✨` has
    /// it. The image is narrower than the emoji as a glyph, and keeps its colors.
    pub fn emoji(self, emoji: &'a str) -> Self {
        Self {
            emoji: Some(emoji),
            ..self
        }
    }

    /// Greyed out and inert.
    pub fn enabled(self, enabled: bool) -> Self {
        Self { enabled, ..self }
    }

    /// Has the keyboard, which fills it and makes it glow.
    pub fn focused(self, focused: bool) -> Self {
        Self { focused, ..self }
    }

    /// Never fills. Hovered, pressed or focused, its label darkens to `rgba(0, 0, 0, .7)` instead
    /// (it still glows), as Get Me To Zero's secondary does.
    pub fn unfilled(self) -> Self {
        Self {
            unfilled: true,
            ..self
        }
    }

    /// Fills with `fill` while disabled, in either theme, where a screen's rule outranks the
    /// button's own (the split inbox settings' primary).
    pub fn disabled_fill(self, fill: Color32) -> Self {
        Self {
            disabled_fill: Some(fill),
            ..self
        }
    }

    /// On a dark surface rather than a dialog's light card (the week view's sidebar in Carbon),
    /// with the label `hsla(0, 0%, 100%, .4)`, `.15` disabled, and `#27292d` on the accent.
    pub fn dark(self, dark: bool) -> Self {
        Self { dark, ..self }
    }

    /// In a dialog forced into the other style's light theme (Out of Office Reply's). The button
    /// keeps Classic's uppercase bold label and 2px corners, in Exo Sans Alt and `#73716d`. It
    /// fills with `#714cb6` (`#533192` while hovered) with white labels, and a disabled primary
    /// keeps its fill at `.4` opacity.
    pub fn origin_light(self) -> Self {
        Self {
            origin_light: true,
            ..self
        }
    }

    /// Its width with its padding.
    pub fn width(&self, ctx: &egui::Context) -> f32 {
        label_width(ctx, &self.style(), self.label, self.emoji) + 2.0 * ACTION_PADDING[1]
    }

    fn style(&self) -> TextStyle {
        if self.origin_light {
            ORIGIN_ACTION
        } else {
            ACTION
        }
    }

    /// Paints the button filling `rect` (its padding box, already nudged down), the label
    /// centered in it, and returns its response.
    pub fn show(&self, ui: &Ui, id: Id, rect: Rect) -> Response {
        let ctx = ui.ctx();
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = ui.interact(rect, id, sense);
        let response = if self.enabled {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        };
        let hover = motion::transition(
            ctx,
            id.with("hover"),
            self.enabled && response.hovered(),
            0.15,
            motion::CubicBezier::EASE,
        );
        let pressed = motion::transition(
            ctx,
            id.with("pressed"),
            self.enabled && response.is_pointer_button_down_on(),
            0.15,
            motion::CubicBezier::EASE,
        );
        let glow = if self.focused { 1.0 } else { hover };
        if self.origin_light {
            let filled = if self.kind == ActionKind::Primary || self.focused {
                1.0
            } else {
                hover.max(pressed)
            };
            let fill = motion::mix(ORIGIN_FILL.gamma_multiply(filled), ORIGIN_FILL_HOVER, hover);
            let ink = motion::mix(ORIGIN_LABEL, Color32::WHITE, filled);
            let opacity = if self.enabled { 1.0 } else { 0.4 };
            let glow = if self.enabled { glow } else { 0.0 };
            paint_box(ui.painter(), rect, fill.gamma_multiply(opacity), glow);
            paint_label(
                ui.painter(),
                rect,
                self.label,
                self.emoji,
                ink.gamma_multiply(opacity),
                &ORIGIN_ACTION,
            );
            return response;
        }
        let [plain, disabled, on_accent] = if self.dark {
            DARK_LABELS
        } else {
            [LABEL, LABEL_DISABLED, LABEL_FILLED]
        };
        let (fill, glow, ink) = if !self.enabled {
            // A disabled primary loses its fill, except in Carbon.
            let carbon_primary =
                self.kind == ActionKind::Primary && app_theme(ctx) == Theme::Carbon;
            let fill = self.disabled_fill.unwrap_or(if carbon_primary {
                ACCENT
            } else {
                Color32::TRANSPARENT
            });
            (fill, 0.0, disabled)
        } else if self.unfilled {
            let darkened = if self.focused {
                1.0
            } else {
                hover.max(pressed)
            };
            (
                Color32::TRANSPARENT,
                glow,
                motion::mix(plain, LABEL_DARKENED, darkened),
            )
        } else {
            let filled = if self.kind == ActionKind::Primary || self.focused {
                1.0
            } else {
                hover.max(pressed)
            };
            (
                ACCENT.gamma_multiply(filled),
                glow,
                motion::mix(plain, on_accent, filled),
            )
        };
        paint_box(ui.painter(), rect, fill, glow);
        paint_label(ui.painter(), rect, self.label, self.emoji, ink, &ACTION);
        response
    }
}

/// An action's width: its uppercase label and side padding.
pub(crate) fn action_width(ctx: &egui::Context, label: &str) -> f32 {
    label_width(ctx, &ACTION, label, None) + 2.0 * ACTION_PADDING[1]
}

/// The width of `label` uppercase in `style`, with a space and `emoji`'s image after it if there
/// is one.
fn label_width(ctx: &egui::Context, style: &TextStyle, label: &str, emoji: Option<&str>) -> f32 {
    match emoji {
        Some(_) => style.width(ctx, &format!("{} ", label.to_uppercase())) + ACTION_EMOJI,
        None => style.width(ctx, &label.to_uppercase()),
    }
}

/// An alert action's fill and label at `filled` of the way from plain to filled with the accent.
pub(crate) fn filled(filled: f32) -> (Color32, Color32) {
    (
        ACCENT.gamma_multiply(filled),
        motion::mix(LABEL, LABEL_FILLED, filled),
    )
}

/// Paints a dialog's or an alert's action over its padding box `rect`: its glow at `glow` (outside
/// the box, as CSS clips a `box-shadow`), `fill`, and the uppercase label centered in `ink`.
pub(crate) fn paint_action(
    painter: &Painter,
    rect: Rect,
    label: &str,
    fill: Color32,
    glow: f32,
    ink: Color32,
) {
    paint_box(painter, rect, fill, glow);
    paint_label(painter, rect, label, None, ink, &ACTION);
}

/// An action's padding box `rect`: its glow at `glow`, outside the box as CSS clips a
/// `box-shadow`, and `fill`.
fn paint_box(painter: &Painter, rect: Rect, fill: Color32, glow: f32) {
    let snapped = pixel_snap(rect);
    if glow > 0.0 {
        for shadow in GLOW {
            shadow.with_opacity(glow).paint_outside(painter, snapped);
        }
    }
    painter.rect_filled(snapped, CornerRadius::same(ACTION_RADIUS), fill);
}

/// An action's uppercase label in `style` and `ink`, centered in `rect`, with `emoji`'s image
/// after it if there is one ([`DialogAction::emoji`]).
fn paint_label(
    painter: &Painter,
    rect: Rect,
    label: &str,
    emoji: Option<&str>,
    ink: Color32,
    style: &TextStyle,
) {
    let width = label_width(painter.ctx(), style, label, emoji);
    // `text-align: center`, for buttons given a width.
    let at = pos2(
        rect.center().x - width / 2.0,
        rect.top() + ACTION_PADDING[0],
    );
    let mut text = label.to_uppercase();
    if emoji.is_some() {
        text.push(' ');
    }
    let text_width = style.paint_line(painter, at, &text, ink, None);
    if let Some(emoji) = emoji {
        let image =
            Rect::from_min_size(at + vec2(text_width, 1.0), vec2(ACTION_EMOJI, ACTION_EMOJI));
        crate::paint_emoji(painter, emoji, image);
    }
}

/// A dialog's text input, a borderless field over a 1px rule. The rule takes the active color
/// while the field has the keyboard (still 1px), and both take the critical color with
/// [`DialogInput::error`].
#[derive(Clone, Copy, Debug)]
#[must_use]
pub struct DialogInput<'a> {
    style: TextStyle,
    placeholder: &'a str,
    error: bool,
}

impl<'a> DialogInput<'a> {
    /// A field in `style` showing `placeholder` while empty (in the accent color, at regular
    /// weight).
    pub fn new(style: TextStyle, placeholder: &'a str) -> Self {
        Self {
            style,
            placeholder,
            error: false,
        }
    }

    /// Marked as wrong: critical text, placeholder and rule.
    pub fn error(self, error: bool) -> Self {
        Self { error, ..self }
    }

    /// Edits `text` on the line box `line` (its 20px line, already nudged down) and draws the
    /// rule under it. Returns the field's response.
    pub fn show(&self, ui: &mut Ui, id: Id, text: &mut String, line: Rect) -> Response {
        self.show_with(ui, id, text, line, text_field)
    }

    /// [`Self::show`] with the field drawn by `field` (as [`text_field`] takes its arguments), such
    /// as a spellchecked one.
    pub fn show_with(
        &self,
        ui: &mut Ui,
        id: Id,
        text: &mut String,
        line: Rect,
        field: impl FnOnce(&mut Ui, Id, &mut String, Rect, &TextStyle, Color32, Color32) -> Response,
    ) -> Response {
        let tokens = palette(ui.ctx());
        let color = if self.error {
            tokens.text_critical_default
        } else {
            tokens.text_base_default
        };
        if text.is_empty() {
            let placeholder = TextStyle {
                font: Font::new(Family::Sans, 400),
                ..self.style
            };
            // The placeholder: `rgba(0, 0, 0, .5)`.
            let placeholder_color = if self.error {
                tokens.text_critical_default
            } else {
                Color32::from_black_alpha(128)
            };
            placeholder.paint_line(
                ui.painter(),
                line.min,
                self.placeholder,
                placeholder_color,
                Some(line.width()),
            );
        }
        let response = field(
            ui,
            id,
            text,
            line,
            &self.style,
            color,
            tokens.text_selection_default,
        );
        let focused = response.has_focus();
        let rule: Color32 = match (self.error, focused) {
            (true, _) => tokens.text_critical_default,
            (false, true) => tokens.border_base_active,
            (false, false) => tokens.border_base_default,
        };
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(line.left(), line.bottom()), vec2(line.width(), 1.0)),
            0.0,
            rule,
        );
        response
    }
}
