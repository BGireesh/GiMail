//! The bar that slides from one message to the next as the keyboard moves between them. It shows
//! for a moment after each move, over the focused card's own bar, and slides off to the left as
//! the keyboard leaves for the list, the back gutter, a search or the folders.

use egui::{CornerRadius, Rect, Ui, pos2, vec2};
use exo_commands::Region;
use exo_ui::motion::CubicBezier;

use super::message;

/// Moves take 254ms on `cubic-bezier(0.19, 1, 0.22, 1)`.
const SLIDE: f32 = 0.254;
const CURVE: CubicBezier = CubicBezier::new(0.19, 1.0, 0.22, 1.0);
/// The bar fades in and out in 128ms, starting 32ms into a slide away.
const FADE: f32 = 0.128;
const FADE_DELAY: f32 = 0.032;
/// It shows for 300ms after the focused message or the keyboard's region changes.
const SHOWN: f64 = 0.3;
/// How far left it slides as the keyboard leaves.
const AWAY: f32 = -20.0;
/// It's 3px wide and 1.5px shorter than the card at each end, with 1px corners on its left.
const WIDTH: f32 = 3.0;
const INSET: f32 = 1.5;
const RADIUS: u8 = 1;

/// A value moving to its latest target like a CSS transition: from wherever it was when the
/// target changed.
#[derive(Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    since: f64,
    delay: f32,
    duration: f32,
}

impl Tween {
    fn at(&self, now: f64) -> f32 {
        let elapsed = now - self.since - f64::from(self.delay);
        // A move that takes no time is at its target once it starts. Dividing by its zero
        // duration would give NaN at that very moment.
        if self.duration <= 0.0 {
            return if elapsed < 0.0 { self.from } else { self.to };
        }
        let progress = (elapsed / f64::from(self.duration)).clamp(0.0, 1.0) as f32;
        self.from + (self.to - self.from) * CURVE.at(progress)
    }

    /// Aims at `to` from now, unless it already is. Returns when it gets there.
    fn aim(&mut self, to: f32, now: f64, duration: f32, delay: f32) -> f64 {
        if to != self.to {
            *self = Self {
                from: self.at(now),
                to,
                since: now,
                delay,
                duration,
            };
        }
        self.since + f64::from(self.delay + self.duration)
    }
}

/// The sliding bar's state, for one open thread. Its first position is taken at once.
#[derive(Default)]
pub(super) struct Highlight {
    /// The message and the keyboard's region it last moved for.
    last: Option<(String, Region)>,
    /// When it hides.
    until: f64,
    /// Its top and height, in the column's coordinates; its offset to the left; its opacity.
    moving: Option<[Tween; 4]>,
}

impl Highlight {
    /// Paints the bar over `card`, the focused message's card, its top in the column's coordinates
    /// (`origin` being where the column's top is drawn), while the keyboard is in `region`.
    pub(super) fn paint(
        &mut self,
        ui: &Ui,
        focused: &str,
        card: Rect,
        origin: f32,
        region: Region,
    ) {
        let now = ui.input(|input| input.time);
        let away = matches!(
            region,
            Region::SearchBar | Region::LabelSwitcher | Region::ActionPane | Region::ThreadList
        );
        let shown = matches!(
            region,
            Region::ThreadMessages | Region::ThreadMessagesNewMessages
        );
        let targets = [
            card.top() + INSET,
            card.height() - 2.0 * INSET,
            if away { AWAY } else { 0.0 },
            if shown { 1.0 } else { 0.0 },
        ];
        let moved = self
            .last
            .as_ref()
            .is_none_or(|(id, last)| id != focused || *last != region);
        if moved {
            self.last = Some((focused.to_owned(), region));
            self.until = now + SHOWN;
        }
        let moving = self.moving.get_or_insert_with(|| {
            targets.map(|at| Tween {
                from: at,
                to: at,
                since: now,
                delay: 0.0,
                duration: 0.0,
            })
        });
        let mut settled = now;
        for (at, (tween, target)) in moving.iter_mut().zip(targets).enumerate() {
            let (duration, delay) = match at {
                3 if away => (FADE, FADE_DELAY),
                3 => (FADE, 0.0),
                _ => (SLIDE, 0.0),
            };
            settled = settled.max(tween.aim(target, now, duration, delay));
        }
        if now >= self.until {
            return;
        }
        if now < settled {
            ui.ctx().request_repaint();
        } else {
            ui.ctx()
                .request_repaint_after_secs((self.until - now) as f32);
        }
        let [top, height, left, opacity] = moving.map(|tween| tween.at(now));
        if opacity <= 0.0 {
            return;
        }
        let bar = Rect::from_min_size(
            pos2(card.left() + left, origin + top),
            vec2(WIDTH, height.max(0.0)),
        );
        ui.painter().rect_filled(
            exo_ui::pixel_snap(bar),
            CornerRadius {
                nw: RADIUS,
                sw: RADIUS,
                ne: 0,
                se: 0,
            },
            message::FOCUS.gamma_multiply(opacity),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Tween;

    #[test]
    fn a_move_that_takes_no_time_is_at_its_target_from_its_start() {
        let tween = Tween {
            from: 0.0,
            to: 1.0,
            since: 10.0,
            delay: 0.0,
            duration: 0.0,
        };
        assert_eq!(tween.at(10.0), 1.0, "at its start, not NaN");
        assert_eq!(tween.at(11.0), 1.0);
        assert_eq!(tween.at(9.0), 0.0, "before it starts");
    }
}
