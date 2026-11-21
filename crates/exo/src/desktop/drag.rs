//! The window's side of the Mac's drag and drop (`NSDraggingSession`), which egui-winit doesn't
//! provide: text dragged over the window from another app, and an editor's selection dragged out of
//! it. Each frame starts with what's over the window and what was let go on it ([`begin`]), and
//! ends with where its editors and fields take text and what they drag out ([`end`]), both through
//! [`exo_ui::text_drag`]. A drag of files is still winit's (compose's attachments).
//!
//! Dragging a selection carries it as HTML (with a UTF-8 `<meta>`) and as text. Within Exo the drag
//! can move the selection (a drop elsewhere in the window takes it out of its editor, and ⌥ copies
//! it instead). Outside Exo it copies. AppKit's drag loop takes the button's release, so once a
//! drag that Exo started is over, the window hears the button go up ([`release`]).

use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
mod macos;

/// A drag that Exo started ended inside AppKit's own loop, which took the button's release. egui
/// thinks the button is still held until it's told otherwise.
#[cfg(target_os = "macos")]
static RELEASED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Text let go without the Mac's drag ([`simulate_drop`]).
    static SIMULATED: RefCell<Option<exo_ui::text_drag::Dropped>> = const { RefCell::new(None) };
}

/// Drops `dropped` on the next frame drawn, without the Mac's drag, as the drag would. For tests,
/// which have no drag.
pub fn simulate_drop(dropped: exo_ui::text_drag::Dropped) {
    SIMULATED.with(|simulated| *simulated.borrow_mut() = Some(dropped));
}

/// The Mac's drag's part as the window's frame starts: what's over the window, what was let go on
/// it, and the end of a drag out of one of its editors.
pub(crate) fn begin(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    if macos::begin(ctx) {
        return;
    }
    let dropped = SIMULATED.with(|simulated| simulated.borrow_mut().take());
    exo_ui::text_drag::begin(ctx, false, None, dropped, None);
}

/// Its part as the frame ends: where text can be let go in the window, and a selection dragged out
/// of it.
pub(crate) fn end(ctx: &egui::Context) {
    let report = exo_ui::text_drag::end(ctx);
    #[cfg(target_os = "macos")]
    macos::end(ctx, report);
    #[cfg(not(target_os = "macos"))]
    drop(report);
}

/// A drag that Exo started (of a selection or a card) ended. AppKit's drag loop took the button's
/// release, so the window's next frame hears the button go up ([`take_release`]).
#[cfg(target_os = "macos")]
pub(crate) fn release() {
    RELEASED.store(true, Ordering::SeqCst);
}

/// Whether a drag Exo started ended since the last call, so egui should see the button let go.
#[cfg(target_os = "macos")]
pub(crate) fn take_release() -> bool {
    RELEASED.swap(false, Ordering::SeqCst)
}
