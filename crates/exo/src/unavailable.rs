//! A conversation opened that the store doesn't have, from a link, a notification or the
//! history, is fetched whole. When it can't be had, Exo goes back to the list rather than showing
//! an empty conversation, and says so when Gmail can't be reached.

/// The alert's text.
const TEXT: &str =
    "This conversation is not available offline.\nPlease check your connection and try again.";

impl crate::Mail {
    /// Thread `id`, opened though the store didn't have it, couldn't be fetched: Gmail can't be
    /// reached (`offline`), or doesn't have it. If it's still open, it closes, back to its list,
    /// under the alert when offline.
    pub(crate) fn thread_unavailable(&mut self, id: &str, offline: bool) {
        // A thread whose mail is gone is shown for its notes, which Gmail can't take away.
        if self.thread.as_ref().is_none_or(|open| open.id() != id)
            || self
                .notes_alone
                .as_ref()
                .is_some_and(|alone| alone.id == id)
        {
            return;
        }
        self.close_thread();
        // Its list stands for it in the history, as if it had never opened.
        self.history_replace();
        if offline {
            self.pickers.inform("Sorry!", TEXT.to_owned());
        }
    }
}
