//! Titled boxes ([`exo_ui::title`]) as macOS's own tooltips. Each box becomes a tooltip rect of
//! the window's view (`addToolTipRect:owner:userData:`), so AppKit shows the title in its style,
//! after its delay, once the pointer rests on the box, and hides it as the pointer leaves. The
//! rects are registered again only when the boxes change, since AppKit only notices the pointer
//! entering a rect that was there before it. A box with something in front of it says nothing.

/// Hands this frame's titled boxes to macOS. Call once a frame, after the screen has drawn.
pub(crate) fn end(ctx: &egui::Context) {
    let titles = exo_ui::take_titles(ctx);
    #[cfg(target_os = "macos")]
    macos::show(titles, ctx.zoom_factor());
    #[cfg(not(target_os = "macos"))]
    drop(titles);
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{cell::RefCell, ffi::c_void};

    use exo_ui::Title;
    use objc2::{
        MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::AnyClass,
    };
    use objc2_app_kit::{NSApplication, NSToolTipTag, NSView, NSViewToolTipOwner};
    use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

    /// What macOS has: the view the rects are on, the boxes as registered (in the view's points),
    /// and the owner AppKit asks for their text.
    #[derive(Default)]
    struct Shown {
        view: Option<Retained<NSView>>,
        titles: Vec<Title>,
        owner: Option<Retained<Owner>>,
    }

    thread_local! {
        static SHOWN: RefCell<Shown> = RefCell::default();
    }

    define_class!(
        // SAFETY: `NSObject` has no subclassing requirements, and `Owner` doesn't implement `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "ExoTitles"]
        struct Owner;

        unsafe impl NSObjectProtocol for Owner {}

        unsafe impl NSViewToolTipOwner for Owner {
            /// The text of box `data` (its index), asked as the pointer has rested on it: its
            /// title while nothing is in front of it, and nothing otherwise.
            #[unsafe(method_id(view:stringForToolTip:point:userData:))]
            fn string_for_tool_tip(
                &self,
                _view: &NSView,
                _tag: NSToolTipTag,
                _point: NSPoint,
                data: *mut c_void,
            ) -> Retained<NSString> {
                let text = SHOWN.with_borrow(|shown| {
                    shown
                        .titles
                        .get(data.addr())
                        .filter(|title| title.hovered)
                        .map(|title| title.text.clone())
                });
                NSString::from_str(text.as_deref().unwrap_or_default())
            }
        }
    );

    impl Owner {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            // SAFETY: `NSObject`'s designated initializer.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Makes `titles` the tooltips of the view egui draws in. They're in egui's points, each
    /// `zoom` of the view's.
    pub(super) fn show(mut titles: Vec<Title>, zoom: f32) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(class) = AnyClass::get(crate::desktop::macos::WINIT_VIEW) else {
            return;
        };
        for title in &mut titles {
            title.rect = title.rect * zoom;
        }
        // winit's view, whichever window is key (a Quick Look panel, say).
        let view = NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .filter_map(|window| window.contentView())
            .find(|view| view.isKindOfClass(class));
        SHOWN.with_borrow_mut(|shown| {
            let same = shown.view == view
                && shown.titles.len() == titles.len()
                && shown
                    .titles
                    .iter()
                    .zip(&titles)
                    .all(|(old, new)| old.rect == new.rect && old.text == new.text);
            if !same {
                if let Some(old) = &shown.view {
                    old.removeAllToolTips();
                }
                if let Some(view) = &view {
                    view.removeAllToolTips();
                    let owner = shown.owner.get_or_insert_with(|| Owner::new(mtm));
                    for (index, title) in titles.iter().enumerate() {
                        let rect = NSRect::new(
                            NSPoint::new(title.rect.left().into(), title.rect.top().into()),
                            NSSize::new(title.rect.width().into(), title.rect.height().into()),
                        );
                        // SAFETY: `owner` answers `view:stringForToolTip:point:userData:`, and
                        // `SHOWN` keeps it for good, which the view doesn't. `userData` is the
                        // box's index, which AppKit hands back without reading it.
                        unsafe {
                            view.addToolTipRect_owner_userData(
                                rect,
                                owner,
                                std::ptr::without_provenance_mut(index),
                            );
                        }
                    }
                }
                shown.view = view;
            }
            // The boxes' hover changes without registering them again.
            shown.titles = titles;
        });
    }
}
