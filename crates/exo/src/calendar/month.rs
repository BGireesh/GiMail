//! The week view's mini month: the month's name with chevrons to the months around it, the
//! weekday initials, and six weeks of days, today on a fill and the selected day on the accent.

use egui::{Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};
use exo_ui::{Family, Font, IconButton, TextStyle};
use jiff::{ToSpan, civil::Date};

/// The month is 280px wide. Its caption has `padding-left: 14px` around 16px/24px text, whose
/// line box lands on the month's top.
pub const WIDTH: f32 = 280.0;
const CAPTION: TextStyle =
    TextStyle::new(Font::new(Family::Sans, 400), 16.0, 24.0).letter_spacing(0.18);
const CAPTION_LEFT: f32 = 14.0;
/// The chevrons (`padding-top: 8px; padding-right: 6px`): two 24px buttons, 16px chevrons, 4px
/// apart, 2px above the caption's box.
const NAV: f32 = 24.0;
const NAV_TOP: f32 = -2.0;
const NAV_RIGHT: f32 = 6.0;
const NAV_GAP: f32 = 4.0;
/// The weekday row starts 48px down (the grid, under the 24px caption) and is 32px tall. Each
/// week is 28px tall, and each column 40px wide.
const WEEKDAYS_TOP: f32 = 48.0;
const WEEKDAY_HEIGHT: f32 = 32.0;
const WEEK: f32 = 28.0;
const COLUMN: f32 = 40.0;
/// 12px/16px text. The weekdays have `padding-top: 8px; top: 1px`. Each day sits in a 24px circle
/// at (8, 2) (`top: 2px`) with `padding-top: 3px`, and its line is centred in the 21px left, 2.5px
/// down, which Chromium paints on the pixel below.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
const WEEKDAY_TEXT_TOP: f32 = 8.0 + 1.0;
const DAY_BUTTON: f32 = 24.0;
const DAY_TEXT_TOP: f32 = 3.0 + 3.0;
/// Its full height, less its `margin-bottom: -8px`.
pub const HEIGHT: f32 = WEEKDAYS_TOP + WEEKDAY_HEIGHT + 6.0 * WEEK - 8.0;
/// `opacity: .4` on the other months' days.
const OUTSIDE_OPACITY: f32 = 0.4;
/// How far the band of shown days stays inside its first and last columns.
const BAND_INSET: f32 = 4.0;

/// What a click in the mini month asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonthAction {
    /// The month before (-1) or after (1).
    Month(i8),
    /// A day.
    Select(Date),
}

/// Paints the month holding `month` with its top left at `origin`, `selected` on the accent and
/// `today` on its fill (today's fill wins). Weeks start on Sunday, or with `monday` on Monday.
/// `band` is the first and last day the calendar shows (the week, in Week), which sit on a light
/// band, as in Google Calendar.
pub fn show(
    ui: &Ui,
    origin: Pos2,
    month: Date,
    selected: Date,
    today: Date,
    monday: bool,
    band: Option<(Date, Date)>,
) -> Option<MonthAction> {
    let palette = exo_ui::palette(ui.ctx());
    // `color: rgba(0,0,0,.9)`, Carbon's `hsla(0,0%,100%,.75)`.
    let ink = palette.ink(0.9, 0.75);
    let painter = ui.painter();
    let first = month.first_of_month();
    let mut action = None;
    CAPTION.paint_line(
        painter,
        origin + vec2(CAPTION_LEFT, 0.0),
        &first.strftime("%B %Y").to_string(),
        ink,
        None,
    );
    for (step, icon) in [
        (-1i8, "legacy/DEPRECATED_leftChevron"),
        (1, "legacy/DEPRECATED_rightChevron"),
    ] {
        let right = origin.x + WIDTH - NAV_RIGHT - if step < 0 { NAV + NAV_GAP } else { 0.0 };
        let hit = Rect::from_min_size(pos2(right - NAV, origin.y + NAV_TOP), Vec2::splat(NAV));
        // The chevrons in the default icon colour.
        let response = IconButton::new(icon).show(
            ui,
            ui.id().with(("calendar-month-nav", step)),
            hit,
            exo_ui::icon_rect(ui.ctx(), icon, hit),
        );
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            action = Some(MonthAction::Month(step));
        }
    }
    let initials = if monday {
        ["M", "T", "W", "T", "F", "S", "S"]
    } else {
        ["S", "M", "T", "W", "T", "F", "S"]
    };
    for (column, initial) in initials.iter().enumerate() {
        let width = TEXT.width(ui.ctx(), initial);
        let x = origin.x + column as f32 * COLUMN + (COLUMN - width) / 2.0;
        TEXT.paint_line(
            painter,
            pos2(x, origin.y + WEEKDAYS_TOP + WEEKDAY_TEXT_TOP),
            initial,
            ink,
            None,
        );
    }
    let offset = if monday {
        first.weekday().to_monday_zero_offset()
    } else {
        first.weekday().to_sunday_zero_offset()
    };
    let start = first - i64::from(offset).days();
    for (row, from, to) in band.map(|band| band_cells(start, band)).unwrap_or_default() {
        let top = origin.y + WEEKDAYS_TOP + WEEKDAY_HEIGHT + row as f32 * WEEK + 2.0;
        let rect = Rect::from_min_max(
            pos2(origin.x + from as f32 * COLUMN + BAND_INSET, top),
            pos2(
                origin.x + (to + 1) as f32 * COLUMN - BAND_INSET,
                top + DAY_BUTTON,
            ),
        );
        painter.rect_filled(rect, DAY_BUTTON / 2.0, palette.ink(0.06, 0.1));
    }
    for index in 0..42 {
        let day = start + i64::from(index).days();
        let (row, column) = (index / 7, index % 7);
        let cell = Rect::from_min_size(
            origin
                + vec2(
                    column as f32 * COLUMN,
                    WEEKDAYS_TOP + WEEKDAY_HEIGHT + row as f32 * WEEK,
                ),
            vec2(COLUMN, WEEK),
        );
        let button = Rect::from_min_size(
            cell.min + vec2((COLUMN - DAY_BUTTON) / 2.0, 2.0),
            Vec2::splat(DAY_BUTTON),
        );
        let response = ui
            .interact(
                button,
                ui.id().with(("calendar-month-day", day)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        // Today and the selected day have white text (Carbon `#27292d`) on `#aeb1dd` (Carbon
        // `#a0a4d9`) and `#929dac`, which are Classic literals.
        let (fill, mut color) = if day == today {
            let fill = match palette.theme {
                exo_ui::Theme::Snow => egui::Color32::from_rgb(0xae, 0xb1, 0xdd),
                exo_ui::Theme::Carbon => egui::Color32::from_rgb(0xa0, 0xa4, 0xd9),
            };
            (Some(fill), palette.text_base_inverse)
        } else if day == selected {
            (
                Some(egui::Color32::from_rgb(0x92, 0x9d, 0xac)),
                palette.text_base_inverse,
            )
        } else if response.hovered() {
            // Any other day, hovered.
            (Some(palette.ink(0.2, 0.2)), ink)
        } else {
            (None, ink)
        };
        let outside = if day.month() == first.month() {
            1.0
        } else {
            OUTSIDE_OPACITY
        };
        color = color.gamma_multiply(outside);
        if let Some(fill) = fill {
            painter.circle_filled(
                button.center(),
                DAY_BUTTON / 2.0,
                fill.gamma_multiply(outside),
            );
        }
        let label = day.day().to_string();
        let width = TEXT.width(ui.ctx(), &label);
        TEXT.paint_line(
            painter,
            pos2(button.center().x - width / 2.0, button.top() + DAY_TEXT_TOP),
            &label,
            color,
            None,
        );
        if response.clicked() {
            action = Some(MonthAction::Select(day));
        }
    }
    action
}

/// The cells `band` (first and last day, inclusive) covers in the six weeks from `start`: one
/// `(row, first column, last column)` for each week it reaches.
fn band_cells(start: Date, (first, last): (Date, Date)) -> Vec<(usize, usize, usize)> {
    let mut cells: Vec<(usize, usize, usize)> = Vec::new();
    for index in 0..42usize {
        let day = start + i64::try_from(index).unwrap_or_default().days();
        if day < first || day > last {
            continue;
        }
        let (row, column) = (index / 7, index % 7);
        match cells.last_mut() {
            Some(cell) if cell.0 == row => cell.2 = column,
            _ => cells.push((row, column, column)),
        }
    }
    cells
}

/// The first of the month `step` months from `month`'s.
pub fn shift(month: Date, step: i8) -> Date {
    month
        .first_of_month()
        .checked_add(i64::from(step).months())
        .unwrap_or(month)
}

#[cfg(test)]
mod band_tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn the_band_covers_the_shown_days_in_each_row() {
        // November 2026's grid starts on Sunday, November 1.
        let start = date(2026, 11, 1);
        assert_eq!(
            band_cells(start, (date(2026, 11, 15), date(2026, 11, 21))),
            [(2, 0, 6)],
            "a Sunday week is the third row"
        );
        assert_eq!(
            band_cells(start, (date(2026, 11, 16), date(2026, 11, 20))),
            [(2, 1, 5)],
            "without weekends, Monday to Friday"
        );
        assert_eq!(
            band_cells(start, (date(2026, 11, 28), date(2026, 12, 4))),
            [(3, 6, 6), (4, 0, 5)],
            "a range across rows is a piece in each"
        );
        assert_eq!(
            band_cells(start, (date(2027, 1, 3), date(2027, 1, 9))),
            [],
            "a week off the grid has no band"
        );
    }
}
