//! What a failed send says, in the header and at send time, for the reasons Exo can tell from
//! Gmail's or Outlook's answers.

use exo_store::SendFailure;

/// A failed send's copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Information {
    /// The list row's date column and the message header's date slot.
    pub title: &'static str,
    /// The header's tooltip. `\n` breaks a line.
    pub description: &'static str,
    /// Where `Learn more` in the tooltip goes.
    pub doc_link: Option<&'static str>,
    /// The list row's status tooltip.
    pub notification: &'static str,
}

/// What a send that failed for `reason` says, naming `Outlook` when `outlook` is set.
pub fn information(reason: SendFailure, outlook: bool) -> Information {
    let (title, description, doc_link, notification) = match reason {
        SendFailure::TooLarge if outlook => (
            "Send failed: too large",
            "This message, including its attachments, is larger than Outlook's limit. Please click to edit and remove some attachments.",
            None,
            "Sorry! Message failed to send because it was too large",
        ),
        SendFailure::TooLarge => (
            "Send failed: too large",
            "This message, including its attachments, is larger than Gmail's limit. Please click to edit and remove some attachments.",
            Some("https://support.google.com/mail/?p=MaxSizeError"),
            "Sorry! Message failed to send because it was too large",
        ),
        SendFailure::QuotaExceeded if outlook => (
            "Send failed: limit reached",
            "You have reached the Outlook limit for sending email. Please try again in a few hours.",
            None,
            "Sorry! Message failed to send as you have reached the Outlook limit for sending email",
        ),
        SendFailure::QuotaExceeded => (
            "Send failed: limit reached",
            "You have reached the Gmail limit for sending email. Please try again in a few hours.",
            Some("https://support.google.com/a/answer/166852?hl=en"),
            "Sorry! Message failed to send as you have reached the Gmail limit for sending email",
        ),
        SendFailure::LoggedOut if outlook => (
            "Send failed: logged out of Outlook",
            "Exo was logged out of Outlook. Please click to edit and try again.",
            None,
            "Sorry! Message failed to send as you were logged out of Outlook",
        ),
        SendFailure::LoggedOut => (
            "Send failed: logged out of Gmail",
            "Exo was logged out of Gmail. Please click to edit and try again.",
            None,
            "Sorry! Message failed to send as you were logged out of Gmail",
        ),
        SendFailure::InvalidRecipients => (
            "Send failed: invalid recipients",
            "Invalid recipients. Please click to edit the message and try again.",
            None,
            "Sorry! Message failed to send as some recipients were invalid",
        ),
        SendFailure::RateLimited if outlook => (
            "Send failed: rate limited",
            "Outlook is temporarily limiting how fast you can send.\nPlease try again in a few minutes.",
            None,
            "Sorry! Message failed to send as Outlook is temporarily limiting how fast you can send",
        ),
        SendFailure::RateLimited => (
            "Send failed: rate limited",
            "Gmail is temporarily limiting how fast you can send.\nPlease try again in a few minutes.",
            None,
            "Sorry! Message failed to send as Gmail is temporarily limiting how fast you can send",
        ),
        SendFailure::Temporary if outlook => (
            "Send failed: temporary problem",
            "Outlook had a temporary problem and could not send this message.\nPlease click to edit and try again.",
            None,
            "Sorry! Message failed to send because Outlook had a temporary problem",
        ),
        SendFailure::Temporary => (
            "Send failed: temporary problem",
            "Gmail had a temporary problem and could not send this message.\nPlease click to edit and try again.",
            None,
            "Sorry! Message failed to send because Gmail had a temporary problem",
        ),
        SendFailure::Other => (
            "Send failed",
            "Please click to edit and try again.",
            None,
            "Sorry! Message failed to send",
        ),
    };
    Information {
        title,
        description,
        doc_link,
        notification,
    }
}

/// A collapsed failed message's date slot, which names fewer reasons than the header.
pub fn collapsed_title(reason: SendFailure, outlook: bool) -> &'static str {
    match reason {
        SendFailure::QuotaExceeded => "Send failed: limit reached",
        SendFailure::TooLarge => "Send failed: too large",
        SendFailure::LoggedOut if outlook => "Send failed: logged out of Outlook",
        SendFailure::LoggedOut => "Send failed: logged out of Gmail",
        SendFailure::InvalidRecipients => "Send failed: invalid recipients",
        SendFailure::RateLimited | SendFailure::Temporary | SendFailure::Other => "Send failed",
    }
}

/// The `Sorry!` alert that shows when a send fails while its toast's observer runs. Logged out and
/// invalid recipients have their states but no alert, because only the failed draft shows them.
pub fn alert(reason: SendFailure, outlook: bool) -> Option<&'static str> {
    match reason {
        SendFailure::QuotaExceeded if outlook => {
            Some("Sending failed. You have hit the Outlook limit for sending email.")
        }
        SendFailure::QuotaExceeded => {
            Some("Sending failed. You have hit the Gmail limit for sending email.")
        }
        SendFailure::TooLarge => Some("Message failed to send because it was too large."),
        SendFailure::LoggedOut | SendFailure::InvalidRecipients => None,
        SendFailure::RateLimited | SendFailure::Temporary | SendFailure::Other => {
            Some("Sending failed. Please check your connection and try again shortly.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exo_names_itself_in_a_logged_out_failure() {
        let logged_out = information(SendFailure::LoggedOut, false);
        assert_eq!(logged_out.title, "Send failed: logged out of Gmail");
        assert_eq!(
            logged_out.description,
            "Exo was logged out of Gmail. Please click to edit and try again."
        );
        assert_eq!(
            alert(SendFailure::LoggedOut, false),
            None,
            "the failed draft says it, not an alert"
        );
        assert_eq!(
            information(SendFailure::LoggedOut, true).title,
            "Send failed: logged out of Outlook"
        );
    }

    #[test]
    fn a_collapsed_message_names_fewer_reasons() {
        assert_eq!(
            information(SendFailure::Temporary, false).title,
            "Send failed: temporary problem"
        );
        assert_eq!(
            collapsed_title(SendFailure::Temporary, false),
            "Send failed"
        );
        assert_eq!(
            collapsed_title(SendFailure::TooLarge, false),
            information(SendFailure::TooLarge, false).title
        );
    }
}
