//! The hint under a message. Under the only message of a conversation that introduces you to
//! someone, it says `Is this an intro? Hit ⌘ shift I to instantly move {first name} to BCC!`.
//! Under the last message of one that settles on a day to come, it says `Tip: Hit b to create a
//! calendar event!`. Under a new message started in a conversation, it says `Referring to the last
//! conversation? Hit ⌘ shift P to pop out this draft!`. Its keys are inline blue caps.

use std::sync::LazyLock;

use egui::{Ui, pos2};
use exo_store::MessageDetail;
use exo_ui::{Family, Font, KeycapColors, Keycaps, TextStyle};
use jiff::Zoned;
use regex::Regex;

use super::Layout;
use crate::{body::Facts, format::names};

/// The hint under the conversation's last message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Hint {
    /// The intro hint, with the first name it moves to Bcc.
    Intro(String),
    /// The calendar tip.
    CalendarEvent,
}

/// The hint: 12px bold on a 24px line, `top: 1px`, `padding-bottom: 28px`.
const HINT: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 24.0);
pub(super) const HEIGHT: f32 = 24.0 + 28.0;
/// `margin-top: -16px` under the card's 28px margin, and -12px under a draft's.
pub(super) const MARGIN_TOP: f32 = -16.0;
pub(super) const MARGIN_TOP_UNDER_DRAFT: f32 = -12.0;
/// The caps' `margin-right: 4px` after them (each cap also has 4px before it), and their
/// `top: 3px` on the line. Their labels move down with them.
const KEYS_GAP: f32 = 4.0;
const KEYS_DROP: f32 = 3.0;

/// The first name the intro hint moves to Bcc, for `message`. The hint shows when the message is
/// to you and not from you, has two people or more on it besides its sender, and has no
/// invitation, and when its subject says it's an intro (`<>`, `->`, `meet `, `intro`) or, with
/// two others on it, names both.
pub(super) fn intro(message: &MessageDetail) -> Option<String> {
    static SAYS_INTRO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)<[ \-–—]*>|[-–—]+\s*>|\bmeet |\bintro").expect("valid regex")
    });
    let from = message.from.as_ref().filter(|from| !from.is_me)?;
    let recipients: Vec<_> = message
        .to
        .iter()
        .chain(&message.cc)
        .chain(&message.bcc)
        .collect();
    let others: Vec<_> = recipients
        .iter()
        .filter(|contact| !contact.email.eq_ignore_ascii_case(&from.email))
        .collect();
    let invite = super::invite::attachment(message).is_some();
    if !recipients.iter().any(|contact| contact.is_me) || others.len() < 2 || invite {
        return None;
    }
    let names_both = others.len() == 2
        && others.iter().all(|contact| {
            contact
                .name
                .as_deref()
                .is_some_and(|name| !name.is_empty() && message.subject.contains(name))
        });
    if !SAYS_INTRO.is_match(&message.subject) && !names_both {
        return None;
    }
    // From the name, else the address's local part.
    let name = from
        .name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| from.email.split('@').next().unwrap_or_default().to_owned());
    Some(names::first_name(&name, &from.email))
}

/// Whether the conversation's last message gets the calendar tip, for its `messages` (oldest
/// first, with their bodies' facts once rendered) at `now`: none of them is marketing mail, and one
/// of the last two mentions a day still to come and has a phrase that proposes or settles a time.
/// A date that can't be read counts as one to come. The caller leaves it out when the last
/// message has a draft under it.
pub(super) fn calendar_event(messages: &[(&MessageDetail, Option<&Facts>)], now: &Zoned) -> bool {
    if messages
        .iter()
        .any(|(_, facts)| facts.is_some_and(|facts| facts.unquoted_marketing))
    {
        return false;
    }
    let last_two = &messages[messages.len().saturating_sub(2)..];
    let to_come = last_two.iter().any(|(message, facts)| {
        let sent = message.date.to_zoned(now.time_zone().clone());
        facts.is_some_and(|facts| {
            facts
                .unquoted_dates
                .iter()
                .any(|text| super::dates::resolve(text, &sent).is_none_or(|day| day > now.date()))
        })
    });
    to_come
        && last_two
            .iter()
            .any(|(_, facts)| facts.is_some_and(|facts| facts.proposes_time))
}

/// Paints `hint`, its line box's top at `top`.
pub(super) fn paint_hint(ui: &Ui, layout: &Layout, top: f32, hint: &Hint) {
    match hint {
        Hint::Intro(first_name) => paint(
            ui,
            layout,
            top,
            "Is this an intro? Hit",
            &["⌘", "shift", "I"],
            &format!("to instantly move {first_name} to BCC!"),
        ),
        Hint::CalendarEvent => paint(
            ui,
            layout,
            top,
            "Tip: Hit",
            &["b"],
            "to create a calendar event!",
        ),
    }
}

/// Paints the pop-out hint, its line box's top at `top`.
pub(super) fn paint_pop_out(ui: &Ui, layout: &Layout, top: f32) {
    paint(
        ui,
        layout,
        top,
        "Referring to the last conversation? Hit",
        &["⌘", "shift", "P"],
        "to pop out this draft!",
    );
}

/// A hint's line: `lead`, the `keys`, then `rest`, cut at the column's end.
fn paint(ui: &Ui, layout: &Layout, top: f32, lead: &str, keys: &[&str], rest: &str) {
    let ctx = ui.ctx();
    let palette = exo_ui::palette(ctx);
    let painter = ui.painter();
    // `color: rgba(0,0,0,.35)`, white in Carbon.
    let ink = palette.ink(0.35, 0.35);
    let line = pos2(layout.inner.left(), top + 1.0);
    let lead = HINT.paint_line(painter, line, lead, ink, None);
    let keys = Keycaps::new(ctx, keys, KeycapColors::shortcuts_pane(palette));
    keys.paint(painter, line.x + lead, line.y + KEYS_DROP);
    let after = line.x + lead + keys.width() + KEYS_GAP;
    HINT.paint_line(
        painter,
        pos2(after, line.y),
        rest,
        ink,
        Some((layout.inner.right() - after).max(0.0)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(day: i8) -> MessageDetail {
        MessageDetail {
            id: format!("m{day}"),
            labels: Vec::new(),
            date: jiff::civil::date(2026, 11, day)
                .at(9, 0, 0, 0)
                .in_tz("America/New_York")
                .expect("a time")
                .timestamp(),
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: String::new(),
            snippet: String::new(),
            rfc822_id: None,
            in_reply_to: None,
            references: Vec::new(),
            list_id: None,
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            headers: Vec::new(),
            body: exo_store::Body::default(),
            attachments: Vec::new(),
            failure: None,
            scheduled: None,
            sending: false,
        }
    }

    fn facts(dates: &[&str], proposes_time: bool) -> Facts {
        Facts {
            unquoted_dates: dates.iter().map(|date| (*date).to_owned()).collect(),
            proposes_time,
            ..Facts::default()
        }
    }

    #[test]
    fn the_calendar_tip_wants_a_day_to_come_and_a_phrase_in_the_last_two_messages() {
        let now = jiff::civil::date(2026, 11, 17)
            .at(9, 0, 0, 0)
            .in_tz("America/New_York")
            .expect("a time");
        let (first, second, third) = (message(15), message(16), message(17));
        let asks = facts(&["Thursday"], true);
        let settles = facts(&[], true);
        let dated = facts(&["Thursday"], false);
        let tip = |messages: &[(&MessageDetail, Option<&Facts>)]| calendar_event(messages, &now);
        assert!(tip(&[(&first, None), (&second, Some(&asks))]));
        // The date and the phrase may come in different messages of the last two.
        assert!(tip(&[(&second, Some(&dated)), (&third, Some(&settles))]));
        // Not from further back.
        assert!(!tip(&[
            (&first, Some(&asks)),
            (&second, None),
            (&third, Some(&settles))
        ]));
        // A day gone by doesn't count, today included.
        assert!(!tip(&[(&third, Some(&facts(&["today"], true)))]));
        // Marketing mail anywhere in the conversation rules it out.
        let marketing = Facts {
            unquoted_marketing: true,
            ..Facts::default()
        };
        assert!(!tip(&[(&first, Some(&marketing)), (&third, Some(&asks))]));
    }
}
