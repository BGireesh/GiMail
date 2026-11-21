//! Tab and ⇧Tab through a surface's fields in an order of its own, as a browser walks a region's
//! tabbable elements: on from the field with the keyboard, round from the last to the first.
//! egui's own Tab goes through every focusable widget it has drawn, so a surface lists its fields
//! each frame, before drawing them, and this takes the key first.

use egui::{Context, EventFilter, FocusDirection, Id, Key, Modifiers};

/// Moves the keyboard along `order` on Tab (⇧Tab goes back) when one of `order` has it, and
/// returns the field it moved to. The key is taken, and egui's own move with it, so call this
/// before any of the fields is drawn this frame.
pub fn tab_order(ctx: &Context, order: &[Id]) -> Option<Id> {
    let focused = ctx.memory(|memory| memory.focused())?;
    let at = order.iter().position(|id| *id == focused)?;
    step(ctx, order, Some(at))
}

/// [`tab_order`] for a modal surface, which has Tab whatever has the keyboard: from outside
/// `order`, Tab goes to its first field and ⇧Tab to its last.
pub fn tab_within(ctx: &Context, order: &[Id]) -> Option<Id> {
    let focused = ctx.memory(|memory| memory.focused());
    let at = focused.and_then(|focused| order.iter().position(|id| *id == focused));
    step(ctx, order, at)
}

/// Takes a Tab or ⇧Tab and moves the keyboard from `order[at]` to the field after or before it,
/// round the ends, or from outside (`None`) to the first or the last.
fn step(ctx: &Context, order: &[Id], at: Option<usize>) -> Option<Id> {
    if order.is_empty() {
        return None;
    }
    // ⇧Tab first: a plain Tab's pattern also matches with Shift held.
    let step = ctx.input_mut(|input| {
        if input.consume_key(Modifiers::SHIFT, Key::Tab) {
            Some(-1)
        } else if input.consume_key(Modifiers::NONE, Key::Tab) {
            Some(1)
        } else {
            None
        }
    })?;
    let next = match at {
        Some(at) => order[(at as isize + step).rem_euclid(order.len() as isize) as usize],
        None if step > 0 => order[0],
        None => order[order.len() - 1],
    };
    ctx.memory_mut(|memory| {
        memory.move_focus(FocusDirection::None);
        memory.request_focus(next);
    });
    Some(next)
}

/// Keeps the arrow keys from moving egui's focus off `id` while it has it, for a control whose
/// arrows are its own or do nothing (a button, a list's trigger). Call it each frame the control
/// is drawn.
pub fn hold_arrows(ctx: &Context, id: Id) {
    let filter = EventFilter {
        horizontal_arrows: true,
        vertical_arrows: true,
        ..EventFilter::default()
    };
    ctx.memory_mut(|memory| memory.set_focus_lock_filter(id, filter));
}

/// The keys of list trigger `id` (a dropdown's button) while it has the keyboard: ↑ and ↓ open
/// its list on row `current`, then move `highlighted`, and Enter takes the highlighted row, which
/// this returns. Space and Enter otherwise press the trigger, as a click does. Call it before the
/// trigger is drawn this frame, so Enter doesn't also press it.
pub fn list_keys(
    ctx: &Context,
    id: Id,
    highlighted: &mut Option<usize>,
    rows: usize,
    current: usize,
) -> Option<usize> {
    if !ctx.memory(|memory| memory.has_focus(id)) || rows == 0 {
        return None;
    }
    hold_arrows(ctx, id);
    let (up, down) = ctx.input_mut(|input| {
        (
            input.consume_key(Modifiers::NONE, Key::ArrowUp),
            input.consume_key(Modifiers::NONE, Key::ArrowDown),
        )
    });
    match *highlighted {
        Some(row) if up => *highlighted = Some(row.saturating_sub(1)),
        Some(row) if down => *highlighted = Some((row + 1).min(rows - 1)),
        Some(row) if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter)) => {
            *highlighted = None;
            return Some(row);
        }
        None if up || down => *highlighted = Some(current.min(rows - 1)),
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use egui::{Event, RawInput};

    use super::*;

    fn tab(ctx: &Context, shift: bool, order: &[Id]) -> Option<Id> {
        let modifiers = if shift {
            Modifiers::SHIFT
        } else {
            Modifiers::NONE
        };
        let input = RawInput {
            events: vec![Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..RawInput::default()
        };
        let mut moved = None;
        let mut output = ctx.run_ui(input, |ui| {
            moved = tab_order(ui.ctx(), order);
            for id in order {
                ui.interact(egui::Rect::EVERYTHING, *id, egui::Sense::click());
            }
        });
        output.textures_delta.clear();
        moved
    }

    #[test]
    fn tab_goes_round_the_fields_and_shift_tab_back() {
        let ctx = Context::default();
        let order = [Id::new("a"), Id::new("b"), Id::new("c")];
        assert_eq!(tab(&ctx, false, &order), None, "nothing has the keyboard");
        ctx.memory_mut(|memory| memory.request_focus(order[2]));
        assert_eq!(
            tab(&ctx, false, &order),
            Some(order[0]),
            "round to the first"
        );
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(order[0]));
        assert_eq!(tab(&ctx, false, &order), Some(order[1]));
        assert_eq!(tab(&ctx, true, &order), Some(order[0]));
        assert_eq!(tab(&ctx, true, &order), Some(order[2]), "round to the last");
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(order[2]));
    }

    #[test]
    fn a_modal_order_takes_tab_from_outside() {
        let ctx = Context::default();
        let order = [Id::new("a"), Id::new("b")];
        let within = |shift| {
            let mut moved = None;
            let modifiers = if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            };
            let input = RawInput {
                events: vec![Event::Key {
                    key: Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..RawInput::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                moved = tab_within(ui.ctx(), &order);
                for id in order {
                    ui.interact(egui::Rect::EVERYTHING, id, egui::Sense::click());
                }
            });
            output.textures_delta.clear();
            moved
        };
        assert_eq!(within(false), Some(order[0]), "Tab enters at the first");
        ctx.memory_mut(|memory| memory.surrender_focus(order[0]));
        assert_eq!(within(true), Some(order[1]), "⇧Tab at the last");
    }
}
