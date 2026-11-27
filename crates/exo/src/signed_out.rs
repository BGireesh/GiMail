//! The "Almost there…" teardown alert, for an account whose provider signed it out.
//!
//! It's the full-screen alert ([`teardown::paint`]), and Sign In, Enter, Escape or five seconds
//! lead to the sign-in screen.

use std::path::Path;

use egui::{Key, Modifiers, Ui};

use jiff::Zoned;

use crate::{backdrop::Backdrop, log_in::LogIn, teardown};

/// The sign-in screen follows by itself after 5 s.
const RELOAD_AFTER: f64 = 5.0;

/// The alert's copy.
const TITLE: &str = "Almost there…";
const GOOGLE_TEXT: &str = "It looks like you've been signed out of Google. Please sign in again.";
const MICROSOFT_TEXT: &str =
    "It looks like you've been signed out of Microsoft. Please sign in again.";
const ACTION: &str = "Sign In";

/// The screen for an account whose grant its provider refused.
pub struct SignedOut {
    key: String,
    email: String,
    provider: exo_auth::Provider,
    backdrop: Backdrop,
    /// The sign-in screen it leads to, its photo decoding meanwhile.
    log_in: Option<LogIn>,
    /// When the screen first drew.
    since: Option<f64>,
}

impl SignedOut {
    /// The screen for the account keyed `key` (display address `email`, `provider`), over the
    /// photo for `now`, leading to the sign-in screen with its photo from `images`. It's ready to
    /// draw.
    pub(crate) fn new(
        ctx: &egui::Context,
        images: &Path,
        now: &Zoned,
        key: String,
        email: String,
        provider: exo_auth::Provider,
    ) -> Self {
        let mut backdrop = Backdrop::new(ctx, now);
        let log_in = LogIn::new(ctx, images, None);
        backdrop.wait();
        Self {
            key,
            email,
            provider,
            backdrop,
            log_in: Some(log_in),
            since: None,
        }
    }

    /// The signed-out account's display address.
    pub fn email(&self) -> &str {
        &self.email
    }

    /// The signed-out account's key (`email` for Google, `microsoft {email}` for Microsoft).
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// The provider whose copy the alert shows.
    pub fn provider(&self) -> exo_auth::Provider {
        self.provider
    }

    /// Whether the photo is decoded. For screenshots and tests.
    pub fn is_idle(&self) -> bool {
        self.backdrop.is_loaded()
    }

    /// Draws a frame and handles its input. Returns the sign-in screen once it's time for it.
    pub(crate) fn show(&mut self, ui: &mut Ui) -> Option<LogIn> {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|input| input.time);
        let elapsed = now - *self.since.get_or_insert(now);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(
            (RELOAD_AFTER - elapsed).max(0.0),
        ));
        // Enter and Escape perform the first action.
        let key = ctx.input_mut(|input| {
            input.consume_key(Modifiers::NONE, Key::Enter)
                | input.consume_key(Modifiers::NONE, Key::Escape)
        });

        let text = match self.provider {
            exo_auth::Provider::Google => GOOGLE_TEXT,
            exo_auth::Provider::Microsoft => MICROSOFT_TEXT,
        };
        let clicked = teardown::paint(
            ui,
            &self.backdrop,
            exo_ui::Alert {
                title: TITLE,
                text,
                actions: &[ACTION],
                selected: 0,
            },
        )
        .is_some();
        if !(clicked || key || elapsed >= RELOAD_AFTER) {
            return None;
        }
        let mut log_in = self.log_in.take()?;
        log_in.wait();
        Some(log_in)
    }
}
