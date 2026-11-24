//! Calendar Settings' Calendar Cache section: whether colleagues' calendars are kept up to date
//! in the store, whose they are (a contact picker over a list with a × each), the weekdays to
//! refresh them on, the hours between which, and how often.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use exo_store::CalendarCache;
use exo_ui::{DialogInput, Family, Font, Menu, TextStyle};

use crate::{compose::contacts, contact_picker::ContactPicker};

const HEADING: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 20.0);
const CHECK_LABEL: TextStyle = TextStyle::new(Font::new(Family::Sans, 500), 14.0, 21.0);
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
const DAY: TextStyle = TextStyle::new(Font::new(Family::Sans, 500), 12.0, 16.0);
const NUDGE: f32 = 3.0;
/// The checkbox row's top under the heading, its height and its label's left, as the dialog's
/// options have them.
const CHECK_BELOW: f32 = 27.0;
const CHECK_ROW: f32 = 24.0;
const LABEL_LEFT: f32 = 24.0;
/// The rows of settings under the checkbox: their labels, and the values 96px in. A row is 28px,
/// and a person under the field 24px.
const ROWS_BELOW: f32 = 4.0;
const ROW: f32 = 28.0;
const PERSON_ROW: f32 = 24.0;
const VALUE_LEFT: f32 = 96.0;
/// The day buttons: 24px squares 4px apart.
const DAY_SIZE: f32 = 24.0;
const DAY_PITCH: f32 = 28.0;
const DAYS: [&str; 5] = ["M", "T", "W", "T", "F"];
/// Where a time's menu opens from its button, and how many of its rows show at once (the wheel
/// scrolls the rest). Its rows are 32px.
const MENU_OFFSET: egui::Vec2 = vec2(-16.0, 26.0);
const MENU_ROW: f32 = 32.0;
const VISIBLE: usize = 9;
/// A list's padding above and below its rows, the room to keep under it, and the gap it leaves
/// over its button when it opens upward.
const MENU_PADDING: f32 = 11.0;
const MENU_EDGE: f32 = 8.0;
const MENU_ABOVE: f32 = 6.0;
/// The gap after the section, as every section of the dialog has.
const GAP_AFTER: f32 = 19.5;
const FREQUENCIES: [(u16, &str); 3] = [(15, "15 minutes"), (30, "30 minutes"), (60, "1 hour")];
/// The last time the window can end at, midnight at the day's end.
const DAY_END: u16 = 24 * 60;
const STEP: u16 = 30;

/// What the section changed in [`CalendarCache`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheChange {
    Enabled(bool),
    /// Somebody picked in People, by address.
    Add(String),
    Remove(String),
    /// A weekday (Monday is 0) turned on or off.
    Day(usize, bool),
    /// The minute of the day the window starts at.
    From(u16),
    /// The minute of the day the window ends at.
    To(u16),
    Every(u16),
}

impl CacheChange {
    /// Applies the change to `cache`. An address is added lower-cased, once, and only if it's an
    /// address. A start that reaches or passes the end moves the end to half an hour after it.
    pub fn apply(self, cache: &mut CalendarCache) {
        match self {
            Self::Enabled(on) => cache.enabled = on,
            Self::Add(address) => {
                let address = address.trim().to_lowercase();
                if crate::compose::recipients::is_valid(&address)
                    && !cache.people.contains(&address)
                {
                    cache.people.push(address);
                }
            }
            Self::Remove(address) => cache.people.retain(|each| *each != address),
            Self::Day(day, on) => {
                if let Some(each) = cache.days.get_mut(day) {
                    *each = on;
                }
            }
            Self::From(minute) => {
                cache.from_minute = minute.min(DAY_END - STEP);
                if cache.to_minute <= cache.from_minute {
                    cache.to_minute = cache.from_minute + STEP;
                }
            }
            Self::To(minute) => cache.to_minute = minute.clamp(cache.from_minute + STEP, DAY_END),
            Self::Every(minutes) => {
                if FREQUENCIES.iter().any(|(each, _)| *each == minutes) {
                    cache.every_minutes = minutes;
                }
            }
        }
    }
}

/// A time's name, as `5:30 PM`.
fn time(minute: u16) -> String {
    let hour = minute / 60 % 24;
    let clock = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!(
        "{clock}:{:02} {}",
        minute % 60,
        if hour < 12 { "AM" } else { "PM" }
    )
}

/// The times to start at (every half hour of the day), and those to end at (every one after the
/// start, to the day's end).
fn starts() -> Vec<u16> {
    (0..DAY_END / STEP).map(|step| step * STEP).collect()
}

fn ends(from: u16) -> Vec<u16> {
    (from / STEP + 1..=DAY_END / STEP)
        .map(|step| step * STEP)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    From,
    To,
    Every,
}

/// A menu that is open: whose, the first row it shows, and a wheel scroll short of a row.
#[derive(Debug)]
struct Open {
    which: Which,
    first: usize,
    carry: f32,
    /// The names it shows.
    shown: Vec<String>,
}

/// The section's state: People's picker and the open menu.
#[derive(Debug, Default)]
pub(crate) struct CacheInput {
    pub picker: ContactPicker,
    open: Option<Open>,
}

impl CacheInput {
    /// Closes the open menu.
    pub fn close_menu(&mut self) {
        self.open = None;
    }

    /// The rows the open menu shows.
    pub fn menu_rows(&self) -> Vec<String> {
        self.open
            .as_ref()
            .map_or_else(Vec::new, |open| open.shown.clone())
    }
}

/// The section's height from its top, down to where the next section's gap ends.
pub(crate) fn height(cache: &CalendarCache) -> f32 {
    let checkbox = CHECK_BELOW + CHECK_ROW;
    let settings = if cache.enabled {
        let people = cache.people.len() as f32;
        let list = if people > 0.0 {
            people * PERSON_ROW + (ROW - PERSON_ROW)
        } else {
            0.0
        };
        ROWS_BELOW + 3.0 * ROW + list + DAY_SIZE
    } else {
        0.0
    };
    checkbox + settings + GAP_AFTER
}

/// Draws the section with its top at `top`, `left` of the body and `width` wide. Returns what
/// changed.
pub(crate) fn show(
    ui: &mut Ui,
    (left, width, top): (f32, f32, f32),
    cache: &CalendarCache,
    input: &mut CacheInput,
    book: Option<&contacts::Index>,
) -> Vec<CacheChange> {
    let palette = exo_ui::palette(ui.ctx());
    let text = palette.ink(0.8, 0.8);
    let mut changes = Vec::new();
    HEADING.paint_line(
        ui.painter(),
        pos2(left, top + NUDGE),
        "Calendar Cache",
        palette.text_base_default,
        None,
    );

    let check = top + CHECK_BELOW;
    let row = Rect::from_min_size(pos2(left, check), vec2(width, CHECK_LABEL.line_height));
    if ui
        .interact(
            row,
            exo_ui::id(ui.ctx(), "calendar-cache-enabled"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        changes.push(CacheChange::Enabled(!cache.enabled));
    }
    exo_ui::paint_checkbox(
        ui.painter(),
        pos2(left, check + 2.5),
        cache.enabled,
        palette,
    );
    CHECK_LABEL.paint_line(
        ui.painter(),
        pos2(left + LABEL_LEFT, check),
        "Cache colleagues' calendars",
        text,
        None,
    );
    if !cache.enabled {
        input.open = None;
        return changes;
    }

    let label = |ui: &Ui, y: f32, name: &str| {
        TEXT.paint_line(ui.painter(), pos2(left, y + NUDGE), name, text, None);
    };
    let value_left = left + VALUE_LEFT;

    // People: the picker, then who's picked with a × each.
    let mut y = check + CHECK_ROW + ROWS_BELOW;
    label(ui, y, "People");
    let line = Rect::from_min_size(
        pos2(value_left, y + NUDGE),
        vec2(width - VALUE_LEFT, TEXT.line_height),
    );
    if let Some(address) = input.picker.show(
        ui,
        exo_ui::id(ui.ctx(), "calendar-cache-people"),
        DialogInput::new(TEXT, "Enter name or email"),
        line,
        book,
    ) {
        changes.push(CacheChange::Add(address));
    }
    y += ROW;
    for person in &cache.people {
        let name = book
            .and_then(|book| book.name_for(person))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| person.clone());
        TEXT.paint_line(
            ui.painter(),
            pos2(value_left, y + 2.0),
            &name,
            text,
            Some(width - VALUE_LEFT - 24.0),
        );
        let cross = Rect::from_min_size(pos2(left + width - 20.0, y), vec2(20.0, PERSON_ROW));
        let hit = ui
            .interact(
                cross,
                exo_ui::id(ui.ctx(), ("calendar-cache-remove", person.as_str())),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let ink = palette.ink(if hit.hovered() { 0.8 } else { 0.4 }, 0.8);
        let center = cross.center();
        let stroke = Stroke::new(1.0, ink);
        for (a, b) in [
            (vec2(-4.0, -4.0), vec2(4.0, 4.0)),
            (vec2(-4.0, 4.0), vec2(4.0, -4.0)),
        ] {
            ui.painter().line_segment([center + a, center + b], stroke);
        }
        if hit.clicked() {
            changes.push(CacheChange::Remove(person.clone()));
        }
        y += PERSON_ROW;
    }
    // The row after a list has its rows' 4px less.
    if !cache.people.is_empty() {
        y += ROW - PERSON_ROW;
    }

    // Refresh on: a button a weekday.
    label(ui, y, "Refresh on");
    for (day, name) in DAYS.iter().enumerate() {
        let button = Rect::from_min_size(
            pos2(value_left + day as f32 * DAY_PITCH, y),
            vec2(DAY_SIZE, DAY_SIZE),
        );
        let on = cache.days[day];
        let hit = ui
            .interact(
                button,
                exo_ui::id(ui.ctx(), ("calendar-cache-day", day)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if on {
            ui.painter()
                .rect_filled(button, CornerRadius::same(4), palette.fill_activated_accent);
        } else {
            ui.painter().rect_stroke(
                button,
                CornerRadius::same(4),
                Stroke::new(1.0, palette.ink(0.25, 0.25)),
                StrokeKind::Inside,
            );
        }
        let width = DAY.width(ui.ctx(), name);
        DAY.paint_line(
            ui.painter(),
            pos2(
                button.center().x - width / 2.0,
                button.center().y - DAY.line_height / 2.0,
            ),
            name,
            if on {
                Color32::WHITE
            } else {
                palette.ink(0.7, 0.7)
            },
            None,
        );
        if hit.clicked() {
            changes.push(CacheChange::Day(day, !on));
        }
    }
    y += ROW;

    // Between: two times, the second only after the first.
    label(ui, y, "Between");
    let from = dropdown(
        ui,
        pos2(value_left, y + NUDGE + 1.0),
        &time(cache.from_minute),
    );
    let from_hit = trigger(ui, from, "calendar-cache-from");
    let to_at = from.right() + 8.0;
    let joiner = TEXT.paint_line(ui.painter(), pos2(to_at, y + NUDGE), "and", text, None);
    let to = dropdown(
        ui,
        pos2(to_at + joiner + 8.0, y + NUDGE + 1.0),
        &time(cache.to_minute),
    );
    let to_hit = trigger(ui, to, "calendar-cache-to");
    y += ROW;

    // Every.
    label(ui, y, "Every");
    let every_name = FREQUENCIES
        .iter()
        .find(|(minutes, _)| *minutes == cache.every_minutes)
        .map_or("1 hour", |(_, name)| name);
    let every = dropdown(ui, pos2(value_left, y + NUDGE + 1.0), every_name);
    let every_hit = trigger(ui, every, "calendar-cache-every");

    // A button opens its menu, or closes it when that's the one open.
    let mut toggled = None;
    for (hit, which) in [
        (&from_hit, Which::From),
        (&to_hit, Which::To),
        (&every_hit, Which::Every),
    ] {
        if hit.clicked() {
            toggled = Some(which);
        }
    }
    if let Some(which) = toggled {
        let was = input.open.as_ref().map(|open| open.which);
        input.open = (was != Some(which)).then(|| Open {
            which,
            first: 0,
            carry: 0.0,
            shown: Vec::new(),
        });
        if let Some(open) = &mut input.open {
            let (all, current) = rows(which, cache);
            open.first = current
                .saturating_sub(VISIBLE / 2)
                .min(all.len().saturating_sub(VISIBLE));
        }
    }
    let anchor = |which| match which {
        Which::From => from,
        Which::To => to,
        Which::Every => every,
    };
    if let Some(open) = &mut input.open {
        let (all, current) = rows(open.which, cache);
        let window = all.len().min(VISIBLE);
        open.first = open.first.min(all.len() - window);
        let names: Vec<String> = all[open.first..open.first + window]
            .iter()
            .map(|(_, name)| name.clone())
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let list = Menu {
            items: &refs,
            highlighted: current
                .checked_sub(open.first)
                .filter(|at| *at < window)
                .unwrap_or(usize::MAX),
        }
        .show_in_dialog(
            ui,
            exo_ui::id(ui.ctx(), "calendar-cache-menu"),
            menu_at(ui, anchor(open.which), window),
        );
        open.shown = names;
        let mut close = false;
        if let Some(clicked) = list.clicked {
            let value = all[open.first + clicked].0;
            changes.push(match open.which {
                Which::From => CacheChange::From(value),
                Which::To => CacheChange::To(value),
                Which::Every => CacheChange::Every(value),
            });
            close = true;
        } else if ui
            .input(|input| input.pointer.hover_pos())
            .is_some_and(|at| list.rect.contains(at))
        {
            // The wheel moves the rows a whole row at a time.
            open.carry += wheel(ui);
            let rows = (open.carry / MENU_ROW).trunc();
            open.carry -= rows * MENU_ROW;
            let first = open.first as isize - rows as isize;
            open.first = first.clamp(0, (all.len() - window) as isize) as usize;
        } else if toggled.is_none()
            && ui.input(|input| input.pointer.any_released())
            && ![&from_hit, &to_hit, &every_hit]
                .iter()
                .any(|hit| hit.hovered())
        {
            close = true;
        }
        if close {
            input.open = None;
        }
    }
    changes
}

/// Where a menu of `rows` opens from `button`: under it, or over it when the window ends before
/// the list does.
fn menu_at(ui: &Ui, button: Rect, rows: usize) -> Pos2 {
    let height = rows as f32 * MENU_ROW + 2.0 * MENU_PADDING;
    let below = button.left_top() + MENU_OFFSET;
    if below.y + height > ui.ctx().content_rect().bottom() - MENU_EDGE {
        pos2(below.x, button.top() - MENU_ABOVE - height)
    } else {
        below
    }
}

/// The rows of `which`'s menu, each with its value, and the one that's current.
fn rows(which: Which, cache: &CalendarCache) -> (Vec<(u16, String)>, usize) {
    let (values, current): (Vec<u16>, u16) = match which {
        Which::From => (starts(), cache.from_minute),
        Which::To => (ends(cache.from_minute), cache.to_minute),
        Which::Every => (
            FREQUENCIES.iter().map(|(minutes, _)| *minutes).collect(),
            cache.every_minutes,
        ),
    };
    let at = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let named = values
        .into_iter()
        .map(|value| {
            let name = match which {
                Which::Every => FREQUENCIES
                    .iter()
                    .find(|(minutes, _)| *minutes == value)
                    .map_or_else(String::new, |(_, name)| (*name).to_owned()),
                Which::From | Which::To => time(value),
            };
            (value, name)
        })
        .collect();
    (named, at)
}

/// A dropdown's button showing `value`, its text's top left at `at`.
fn dropdown(ui: &Ui, at: Pos2, value: &str) -> Rect {
    exo_ui::paint_dialog_dropdown(ui, at, value)
}

fn trigger(ui: &Ui, button: Rect, id: &str) -> egui::Response {
    ui.interact(button, exo_ui::id(ui.ctx(), id), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// How far the wheel turned this frame, in points: a trackpad's points as they are, a wheel's
/// lines a row each, and its pages the menu's height.
fn wheel(ui: &Ui) -> f32 {
    ui.input(|input| {
        input
            .raw
            .events
            .iter()
            .map(|event| match event {
                egui::Event::MouseWheel { unit, delta, .. } => {
                    delta.y
                        * match unit {
                            egui::MouseWheelUnit::Point => 1.0,
                            egui::MouseWheelUnit::Line => MENU_ROW,
                            egui::MouseWheelUnit::Page => MENU_ROW * VISIBLE as f32,
                        }
                }
                _ => 0.0,
            })
            .sum()
    })
}
