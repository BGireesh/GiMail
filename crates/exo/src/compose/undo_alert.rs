//! The Undo Send alert (`Congratulations 🙌`), which a send shows until it has been dismissed
//! once. While it's open, its send waits for it, and the send's toast stays up without Instant
//! Send. `Z` in it undoes the send. `Got it` (or Escape) holds the send ten seconds more and lets
//! the toast go.

use jiff::{SignedDuration, Timestamp};

use super::{Alert, AlertThen, send_toast};

pub(super) const TITLE: &str = "Congratulations 🙌";
/// The text: `Your message is sent!`, then `You can hit` `Z` `within 10 seconds to undo send.`.
pub(super) const BEFORE: &str = "Your message is sent!\u{2029}You can hit";
pub(super) const KEY: &str = "Z";
pub(super) const AFTER: &str = "within 10 seconds to undo send.";
/// While the alert is open, the send is held to [`HOLD_BY`] past now every [`HOLD_EVERY`]
/// seconds, and once more as it closes.
const HOLD_BY: SignedDuration = SignedDuration::from_secs(10);
const HOLD_EVERY: f64 = 5.0;

/// The send the open alert holds.
pub(super) struct Held {
    /// The send's toast tag.
    pub tag: u64,
    /// When the send was last held longer.
    at: f64,
}

impl crate::Mail {
    /// Whether the Undo Send alert is still to show for the account.
    pub(super) fn undo_alert_pending(&self) -> bool {
        self.account.settings().do_show_undo_send_ftue
    }

    /// Shows the alert for the send `tag` of draft `id`, unless the account has dismissed it or
    /// another alert is up. A scheduled send isn't sent yet, so it shows none.
    pub(super) fn show_undo_alert(&mut self, id: &str, tag: u64, scheduled: bool, now: f64) {
        if scheduled || self.compose.alert.is_some() || !self.undo_alert_pending() {
            return;
        }
        let text = format!("{BEFORE} {KEY} {AFTER}");
        self.compose.alert = Some(Alert::new(
            Some(TITLE),
            text,
            vec!["Got it"],
            id,
            AlertThen::UndoSend(tag),
        ));
        self.compose.undo_alert = Some(Held { tag, at: now });
    }

    /// While the alert is open, its send waits: every five seconds, it's held to ten seconds
    /// past now.
    pub(super) fn hold_for_undo_alert(&mut self, now: f64) {
        let open = self
            .compose
            .alert
            .as_ref()
            .is_some_and(|alert| matches!(alert.then, AlertThen::UndoSend(_)));
        let Some(held) = self.compose.undo_alert.as_mut().filter(|_| open) else {
            self.compose.undo_alert = None;
            return;
        };
        if now - held.at >= HOLD_EVERY {
            held.at = now;
            let tag = held.tag;
            self.hold_send(tag);
        }
        self.ctx.request_repaint_after_secs(HOLD_EVERY as f32);
    }

    /// `Got it`: the send is held ten seconds more, its toast (once Gmail has the message) goes
    /// as a sent toast does, and the alert won't show again.
    pub(super) fn dismiss_undo_alert(&mut self, tag: u64) {
        self.finish_undo_alert();
        self.hold_send(tag);
        let showing = self.commands.toast_tag();
        if let Some(sending) = self
            .compose
            .sends
            .iter()
            .find(|sending| sending.tag == tag && sending.acked && !sending.undoing)
            .filter(|_| showing == Some(tag))
        {
            self.commands
                .toast(send_toast(sending.sent_text(), tag, false));
        }
    }

    /// The alert is done with, for good: dismissed, or its send undone.
    pub(super) fn finish_undo_alert(&mut self) {
        self.compose.undo_alert = None;
        if self
            .compose
            .alert
            .as_ref()
            .is_some_and(|alert| matches!(alert.then, AlertThen::UndoSend(_)))
        {
            self.compose.alert = None;
        }
        self.account
            .update_settings(|settings| settings.do_show_undo_send_ftue = false);
    }

    /// Holds the send `tag` to ten seconds past now, unless it's held longer already.
    fn hold_send(&mut self, tag: u64) {
        let wanted = Timestamp::now()
            .checked_add(HOLD_BY)
            .unwrap_or(Timestamp::MAX);
        if let Some(sending) = self
            .compose
            .sends
            .iter_mut()
            .find(|sending| sending.tag == tag && !sending.undoing && wanted > sending.until)
        {
            sending.until = wanted;
            self.account.send_draft(&sending.open.id, wanted);
        }
    }
}
