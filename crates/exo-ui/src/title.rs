//! Titles: text the system shows in its own tooltip once the pointer rests on a box, as a web
//! page's `title` attribute does. Screens name their titled boxes as they draw each frame
//! ([`title`]), and the app hands that frame's boxes to the system ([`take_titles`]).

use egui::{Context, Rect};

/// A box with a title this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Title {
    /// The box, in points.
    pub rect: Rect,
    /// What its tooltip says.
    pub text: String,
    /// Whether the pointer is on the box with nothing in front of it.
    pub hovered: bool,
}

fn titles_id(ctx: &Context) -> egui::Id {
    crate::id(ctx, "exo_ui::titles")
}

/// Titles `rect` with `text` for this frame. `hovered` is whether the pointer is on it with
/// nothing in front, which a covered box's title needs to stay hidden.
pub fn title(ctx: &Context, rect: Rect, text: &str, hovered: bool) {
    let id = titles_id(ctx);
    let title = Title {
        rect,
        text: text.to_owned(),
        hovered,
    };
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Vec<Title>>(id).push(title));
}

/// This frame's titled boxes, in the order they were drawn. They're gone after.
pub fn take_titles(ctx: &Context) -> Vec<Title> {
    let id = titles_id(ctx);
    ctx.data_mut(|data| data.remove_temp::<Vec<Title>>(id))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frames_titles_are_taken_once() {
        let ctx = Context::default();
        let rect = Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(40.0, 16.0));
        title(&ctx, rect, "Clients/Calloway", true);
        assert_eq!(
            take_titles(&ctx),
            vec![Title {
                rect,
                text: "Clients/Calloway".into(),
                hovered: true,
            }]
        );
        assert!(take_titles(&ctx).is_empty());
    }
}
