//! Right-clicks on the page. What has a menu of its own (a text field, a message body, a draft, a
//! list row) claims the frame's right-click ([`claim_right_click`]), and the shell's menu takes
//! the rest of the page, made of the links painted there ([`link_box`]) and the selection.

use egui::{Context, Pos2, Rect};

/// This frame's claim and links.
#[derive(Clone, Default)]
struct Frame {
    pass: u64,
    claimed: bool,
    links: Vec<(Rect, String)>,
}

/// Runs `f` on this frame's record, starting it afresh on a new frame.
fn with<R>(ctx: &Context, f: impl FnOnce(&mut Frame) -> R) -> R {
    let id = crate::id(ctx, "exo_ui::right_click");
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|data| {
        let frame = data.get_temp_mut_or_default::<Frame>(id);
        if frame.pass != pass {
            *frame = Frame {
                pass,
                ..Frame::default()
            };
        }
        f(frame)
    })
}

/// Says that this frame's right-click opens a menu of its own.
pub fn claim_right_click(ctx: &Context) {
    with(ctx, |frame| frame.claimed = true);
}

/// Whether something claimed this frame's right-click.
pub fn right_click_claimed(ctx: &Context) -> bool {
    with(ctx, |frame| frame.claimed)
}

/// Says that a link to `href` is painted in `rect` this frame.
pub fn link_box(ctx: &Context, rect: Rect, href: &str) {
    with(ctx, |frame| frame.links.push((rect, href.to_owned())));
}

/// The link painted under `at` this frame, the last painted if several are, which is on top.
pub fn link_at(ctx: &Context, at: Pos2) -> Option<String> {
    with(ctx, |frame| {
        frame
            .links
            .iter()
            .rfind(|(rect, _)| rect.contains(at))
            .map(|(_, href)| href.clone())
    })
}

#[cfg(test)]
mod tests {
    use egui::{pos2, vec2};

    use super::*;

    #[test]
    fn a_frames_claim_and_links_last_the_frame() {
        let ctx = Context::default();
        let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(40.0, 16.0));
        let frame = |paint: &dyn Fn(&Context)| {
            ctx.run_ui(egui::RawInput::default(), |ui| paint(ui.ctx()))
                .drop_without_applying_deltas();
        };
        frame(&|ctx| {
            claim_right_click(ctx);
            link_box(ctx, rect, "https://exo.example");
            assert!(right_click_claimed(ctx));
            assert_eq!(
                link_at(ctx, pos2(20.0, 20.0)).as_deref(),
                Some("https://exo.example")
            );
            assert_eq!(link_at(ctx, pos2(60.0, 20.0)), None);
        });
        frame(&|ctx| {
            assert!(!right_click_claimed(ctx));
            assert_eq!(link_at(ctx, pos2(20.0, 20.0)), None);
        });
    }
}
