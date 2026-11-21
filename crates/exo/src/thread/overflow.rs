//! A body wider than its column scrolls sideways (`overflow-x: auto`), by a sideways swipe or
//! Shift-wheel over it, by dragging the thumb of its scrollbar or by clicking its track. The
//! scrollbar is an 11px strip below the body, and its thumb shows only while the pointer moves
//! over the body.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};
use exo_mail::SCROLLBAR_HEIGHT;
use exo_ui::motion::{self, CubicBezier};

use crate::body::Bodies;

/// The thumb hides this long after the pointer last moved over the body.
const SHOWN_FOR: f64 = 1.0;
/// …unless it stopped this close above the body's bottom edge.
const NEAR_SCROLLBAR: f32 = 200.0;
/// `::-webkit-scrollbar-thumb { border: 2px solid transparent; background-clip: padding-box }`.
const THUMB_BORDER: f32 = 2.0;
/// The shortest thumb (its `width: 7px`).
const MIN_THUMB: f32 = 7.0;
/// The thumb is black at .5 while it shows, eased in and out over `.2s`.
const THUMB_OPACITY: f32 = 0.5;
const FADE: f32 = 0.2;
/// Clicking the track scrolls a page toward the click: 87.5% of the column (Chromium's
/// `PageStep`). Holding it pages again after macOS's scroller button delay, then at its
/// period, until the thumb reaches the pointer.
const PAGE: f32 = 0.875;
const REPEAT_AFTER: f64 = 0.5;
const REPEAT_EVERY: f64 = 0.05;
/// A page scrolls on `ease-in-out` over 200 ms for 120px or less, down to 100 ms for 480px or more
/// (Chromium's scroll animation for that distance).
const PAGE_SHORT: (f32, f32) = (120.0, 0.2);
const PAGE_LONG: (f32, f32) = (480.0, 0.1);
/// A wheel gesture is over once its events stop for this long (egui's own end of a scroll).
const GESTURE_GAP: f64 = 0.15;

/// Whether the thumb shows, and when it hides.
#[derive(Clone, Copy, Debug, Default)]
struct Shown {
    shown: bool,
    hide_at: Option<f64>,
}

/// The wheel gesture under way, and which body it scrolls sideways.
///
/// Chromium on macOS rails a trackpad gesture to the axis its first movement is larger along,
/// and latches all of it to the innermost scroller under the pointer that can move that way: a
/// sideways gesture over a body that can scroll in its direction scrolls only that body, and
/// any other gesture only the thread. Mouse wheel turns a moment apart are one gesture too.
#[derive(Clone, Debug, Default)]
pub(super) struct Gesture {
    /// It scrolls sideways (not down), and that body if any.
    sideways: bool,
    body: Option<String>,
    /// When its last event came.
    last: f64,
}

impl Gesture {
    /// Takes this frame's wheel `delta` at `now`, with `hovered`, the body under the pointer (its
    /// id, how far it's scrolled sideways and how far it can be), and returns the part of it
    /// that's for the thread: none of it while a body has the gesture, and only one axis of it.
    pub(super) fn update(
        &mut self,
        now: f64,
        delta: Vec2,
        hovered: Option<(&str, f32, f32)>,
    ) -> Vec2 {
        if delta == Vec2::ZERO {
            return Vec2::ZERO;
        }
        if now - self.last > GESTURE_GAP {
            self.sideways = delta.x.abs() > delta.y.abs();
            // Content moving left (a negative delta) scrolls the body right.
            self.body = hovered
                .filter(|(_, scroll_x, overflow)| {
                    self.sideways
                        && ((delta.x < 0.0 && scroll_x < overflow)
                            || (delta.x > 0.0 && *scroll_x > 0.0))
                })
                .map(|(id, _, _)| id.to_owned());
        }
        self.last = now;
        match (self.sideways, &self.body) {
            (_, Some(_)) => Vec2::ZERO,
            (true, None) => vec2(delta.x, 0.0),
            (false, None) => vec2(0.0, delta.y),
        }
    }

    /// The sideways scroll for body `id` this frame: the gesture's, when it has the gesture.
    fn delta_for(&self, ui: &Ui, id: &str) -> f32 {
        if self.body.as_deref() == Some(id) {
            ui.input(|input| input.smooth_scroll_delta.x)
        } else {
            0.0
        }
    }
}

/// A page under way: the scroll it started from and the one it's going to.
#[derive(Clone, Copy, Debug)]
struct Paging {
    from: f32,
    to: f32,
    started: f64,
    duration: f32,
}

impl Paging {
    fn at(&self, now: f64) -> f32 {
        let progress = ((now - self.started) as f32 / self.duration).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * CubicBezier::EASE_IN_OUT.at(progress)
    }

    fn over(&self, now: f64) -> bool {
        now - self.started >= f64::from(self.duration)
    }
}

/// How long a page of `distance` px takes.
fn page_duration(distance: f32) -> f32 {
    let (short, long) = (PAGE_SHORT, PAGE_LONG);
    let along = ((distance.abs() - short.0) / (long.0 - short.0)).clamp(0.0, 1.0);
    short.1 + (long.1 - short.1) * along
}

/// Scrolls message `id`'s body (on screen at `body`, scrollbar included) by the wheel gesture
/// it has, by dragging its thumb and by pressing its track. Call it before the body paints.
/// Returns the part of `body` that's content, which takes the pointer's hover, clicks and
/// selections (the scrollbar doesn't).
pub(super) fn scroll(
    ui: &Ui,
    bodies: &mut Bodies,
    gesture: &Gesture,
    id: &str,
    body: Rect,
) -> Rect {
    let overflow = bodies.overflow(id);
    if overflow <= 0.0 {
        return body;
    }
    let scale = ui.ctx().pixels_per_point();
    let now = ui.input(|input| input.time);
    let paging_key = ui.id().with(("body-paging", id));
    let mut paging: Option<Paging> = ui.data(|data| data.get_temp(paging_key));
    let delta = gesture.delta_for(ui, id);
    if delta != 0.0 {
        paging = None;
        bodies.set_scroll_x(id, bodies.scroll_x(id) - delta, scale);
    }
    let (track, thumb) = scrollbar_geometry(body, overflow, bodies.scroll_x(id));
    // The track first, so the thumb over it takes the pointer.
    let pressed = ui.interact(
        track,
        ui.id().with(("body-scrollbar-track", id)),
        Sense::drag(),
    );
    let response = ui.interact(thumb, drag_id(ui, id), Sense::drag());
    let from = ui.id().with(("body-scrollbar-drag", id));
    if response.drag_started()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        paging = None;
        ui.data_mut(|data| data.insert_temp(from, (pointer.x, bodies.scroll_x(id))));
    }
    if response.dragged()
        && let (Some(pointer), Some((start, scrolled))) = (
            response.interact_pointer_pos(),
            ui.data(|data| data.get_temp::<(f32, f32)>(from)),
        )
    {
        let per_px = overflow / (track.width() - thumb.width()).max(1.0);
        bodies.set_scroll_x(id, scrolled + (pointer.x - start) * per_px, scale);
    }
    // Pressing the track pages toward the press, and holding it keeps paging until the thumb
    // would be under the pointer.
    let held_key = ui.id().with(("body-scrollbar-held", id));
    if pressed.is_pointer_button_down_on()
        && let Some(pointer) = pressed.interact_pointer_pos()
    {
        let (direction, page_now, next) =
            match ui.data(|data| data.get_temp::<(f32, f64)>(held_key)) {
                None => {
                    let direction = if pointer.x < thumb.center().x {
                        -1.0
                    } else {
                        1.0
                    };
                    (direction, true, now + REPEAT_AFTER)
                }
                Some((direction, due)) if now >= due => (direction, true, now + REPEAT_EVERY),
                Some((direction, due)) => (direction, false, due),
            };
        let target = paging.map_or(bodies.scroll_x(id), |paging| paging.to);
        let (_, thumb_there) = scrollbar_geometry(body, overflow, target);
        if page_now && !(thumb_there.left()..thumb_there.right()).contains(&pointer.x) {
            let to = (target + (body.width() * PAGE).floor() * direction).clamp(0.0, overflow);
            paging = Some(Paging {
                from: bodies.scroll_x(id),
                to,
                started: now,
                duration: page_duration(to - target),
            });
        }
        ui.data_mut(|data| data.insert_temp(held_key, (direction, next)));
        ui.ctx().request_repaint_after_secs((next - now) as f32);
    } else {
        ui.data_mut(|data| data.remove::<(f32, f64)>(held_key));
    }
    if let Some(page) = paging {
        bodies.set_scroll_x(id, page.at(now), scale);
        if page.over(now) {
            paging = None;
        } else {
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|data| match paging {
        Some(paging) => {
            data.insert_temp(paging_key, paging);
        }
        None => data.remove::<Paging>(paging_key),
    });
    Rect::from_min_max(
        body.min,
        pos2(body.right(), body.bottom() - SCROLLBAR_HEIGHT),
    )
}

/// Paints message `id`'s scrollbar thumb over its body (on screen at `body`), when the body
/// scrolls sideways and the thumb shows.
pub(super) fn scrollbar(ui: &Ui, bodies: &Bodies, id: &str, body: Rect) {
    let overflow = bodies.overflow(id);
    if overflow <= 0.0 {
        return;
    }
    let now = ui.input(|input| input.time);
    let key = ui.id().with(("body-scrollbar", id));
    let mut shown: Shown = ui.data(|data| data.get_temp(key)).unwrap_or_default();
    let (pointer, moved) = ui.input(|input| {
        (
            input.pointer.hover_pos(),
            input.pointer.delta() != egui::Vec2::ZERO,
        )
    });
    if moved {
        shown = match pointer.filter(|at| body.contains(*at)) {
            Some(at) => Shown {
                shown: true,
                hide_at: (!near_scrollbar(at, body)).then_some(now + SHOWN_FOR),
            },
            // Moving anywhere else hides it at once.
            None => Shown::default(),
        };
    }
    if let Some(hide_at) = shown.hide_at {
        if now >= hide_at {
            shown = Shown::default();
        } else {
            ui.ctx().request_repaint_after_secs((hide_at - now) as f32);
        }
    }
    ui.data_mut(|data| data.insert_temp(key, shown));
    let (_, thumb) = scrollbar_geometry(body, overflow, bodies.scroll_x(id));
    // `::-webkit-scrollbar-thumb:hover` shows it too, as does holding it.
    let held = ui.ctx().is_being_dragged(drag_id(ui, id));
    let on_thumb = pointer.is_some_and(|at| thumb.contains(at));
    let opacity = motion::transition(
        ui.ctx(),
        key.with("opacity"),
        shown.shown || on_thumb || held,
        FADE,
        CubicBezier::EASE_IN_OUT,
    );
    if opacity > 0.0 {
        // `border-radius: 100px`, a capsule.
        let alpha = (THUMB_OPACITY * opacity * 255.0).round() as u8;
        ui.painter().with_clip_rect(ui.clip_rect()).rect_filled(
            thumb.shrink(THUMB_BORDER),
            CornerRadius::same(u8::MAX),
            Color32::from_black_alpha(alpha),
        );
    }
}

fn drag_id(ui: &Ui, id: &str) -> egui::Id {
    ui.id().with(("body-scrollbar-thumb", id))
}

/// The scrollbar's track (the 11px under the content) and its thumb, for a body `overflow` px
/// wider than its column and scrolled `scroll_x` px.
fn scrollbar_geometry(body: Rect, overflow: f32, scroll_x: f32) -> (Rect, Rect) {
    let track = Rect::from_min_max(
        pos2(body.left(), body.bottom() - SCROLLBAR_HEIGHT),
        body.max,
    );
    let column = body.width();
    let length = (track.width() * column / (column + overflow))
        .round()
        .max(MIN_THUMB);
    let left = track.left() + scroll_x / overflow * (track.width() - length);
    (
        track,
        Rect::from_min_size(pos2(left, track.top()), vec2(length, SCROLLBAR_HEIGHT)),
    )
}

/// Within 200px above the body's bottom edge.
fn near_scrollbar(at: Pos2, body: Rect) -> bool {
    at.y <= body.bottom()
        && at.y > body.bottom() - NEAR_SCROLLBAR
        && (body.left()..=body.right()).contains(&at.x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wheel_gesture_keeps_to_one_axis_and_one_scroller() {
        let body = Some(("m1", 0.0, 560.0));
        // A swipe that starts sideways over a body that can scroll that way is the body's to
        // its end: the thread gets none of it, even where it turns downward.
        let mut gesture = Gesture::default();
        assert_eq!(gesture.update(1.0, vec2(-30.0, 4.0), body), Vec2::ZERO);
        assert_eq!(gesture.body.as_deref(), Some("m1"));
        assert_eq!(gesture.update(1.1, vec2(-2.0, -40.0), body), Vec2::ZERO);
        // A pause ends it. One that starts downward is the thread's, down only.
        assert_eq!(
            gesture.update(1.5, vec2(3.0, -20.0), body),
            vec2(0.0, -20.0)
        );
        assert_eq!(gesture.body, None);
        assert_eq!(
            gesture.update(1.6, vec2(-50.0, -5.0), body),
            vec2(0.0, -5.0)
        );
        // Sideways toward an edge the body is already at, it's the thread's (the widened
        // column's), sideways only.
        assert_eq!(gesture.update(2.0, vec2(30.0, 4.0), body), vec2(30.0, 0.0));
        assert_eq!(gesture.body, None);
    }

    #[test]
    fn a_page_takes_longer_the_shorter_it_is() {
        assert_eq!(page_duration(560.0), 0.1);
        assert_eq!(page_duration(-480.0), 0.1);
        assert_eq!(page_duration(300.0), 0.15);
        assert_eq!(page_duration(50.0), 0.2);
    }

    #[test]
    fn the_thumb_spans_the_part_in_view() {
        // A 1200px body in a 600px column, scrolled halfway, has a thumb half the track long, in
        // the track's middle.
        let body = Rect::from_min_size(pos2(100.0, 0.0), vec2(600.0, 311.0));
        let (track, thumb) = scrollbar_geometry(body, 600.0, 300.0);
        assert_eq!(
            track,
            Rect::from_min_max(pos2(100.0, 300.0), pos2(700.0, 311.0))
        );
        assert_eq!(
            thumb,
            Rect::from_min_size(pos2(250.0, 300.0), vec2(300.0, 11.0))
        );
        let (_, end) = scrollbar_geometry(body, 600.0, 600.0);
        assert_eq!(end.right(), track.right());
        assert!(
            near_scrollbar(pos2(300.0, 200.0), body) && !near_scrollbar(pos2(300.0, 100.0), body)
        );
    }
}
