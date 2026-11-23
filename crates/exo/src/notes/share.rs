//! `Share notes by email`: a thread's notes as the body of a new message, which the user
//! addresses and sends like any other. The notes' inline images go as the draft's inline images
//! and their files as its attachments ([`crate::Mail::compose_share_popped`]).

use std::collections::HashSet;

use exo_store::{Note, NoteFile, NoteThread};
use jiff::Zoned;

use super::{OnMessage, date, where_line};

/// The message that shares a thread's notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    /// `Notes: {subject}`.
    pub subject: String,
    /// The line naming the thread, then each note.
    pub html: String,
    /// The images the notes' text shows, each by the content id `html` names it by.
    pub images: Vec<NoteFile>,
    /// The files listed under the notes, in note order.
    pub files: Vec<NoteFile>,
}

/// The message sharing `notes` (a thread's live ones) on the thread `context` describes, for the
/// user `me`. Notes go in time order, each with its label, its date and what it's on (`on` says
/// whose its message is), dated in `now`'s zone. A context that knows nothing of the thread's
/// messages (a thread whose notes kept none) names the thread by its subject alone.
pub fn build(
    context: &NoteThread,
    notes: &[Note],
    me: &str,
    on: impl Fn(&Note) -> OnMessage,
    now: &Zoned,
) -> Shared {
    let subject = context.subject.trim();
    let mut html = format!(
        "<div>Notes on \u{201c}{}\u{201d}{}.</div>",
        escape(subject),
        escape(&about(context, me, now))
    );
    let mut ordered: Vec<&Note> = notes.iter().collect();
    ordered.sort_by_key(|note| (note.created_at, note.number));
    let mut images: Vec<NoteFile> = Vec::new();
    let mut files = Vec::new();
    let mut cids: HashSet<String> = HashSet::new();
    for note in ordered {
        let mut content = note.html.clone();
        for file in &note.files {
            let Some(cid) = &file.content_id else {
                files.push(file.clone());
                continue;
            };
            // An image taken out of the text isn't shown, so it isn't sent.
            if !note.html.contains(cid.as_str()) {
                continue;
            }
            let mut image = file.clone();
            // Two notes showing images by the same id would show one image twice.
            if !cids.insert(cid.to_ascii_lowercase()) {
                let fresh = exo_editor::uuid_v4();
                content = content.replace(cid.as_str(), &fresh);
                cids.insert(fresh.to_ascii_lowercase());
                image.content_id = Some(fresh);
            }
            images.push(image);
        }
        html.push_str(&format!(
            "<hr><div><b>{}</b></div><div>{} \u{b7} {}</div>{content}",
            escape(&note.label()),
            escape(&date(note.updated_at, now)),
            escape(&where_line(note, on(note), now)),
        ));
    }
    Shared {
        subject: format!("Notes: {subject}"),
        html,
        images,
        files,
    }
}

/// The intro's parenthetical: ` (3 emails with Dana Reyes and Sam, 3 Nov to 12 Nov)`, or nothing
/// when the context knows nothing of the thread's messages.
fn about(context: &NoteThread, me: &str, now: &Zoned) -> String {
    if context.messages == 0 && context.first_date.is_none() && context.last_date.is_none() {
        return String::new();
    }
    let mut seen: HashSet<String> = HashSet::new();
    let names: Vec<String> = context
        .participants
        .iter()
        .filter(|person| !person.email.eq_ignore_ascii_case(me))
        .filter(|person| seen.insert(person.email.to_ascii_lowercase()))
        .map(|person| {
            person
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&person.email)
                .to_owned()
        })
        .collect();
    let mut out = match context.messages {
        1 => "1 email".to_owned(),
        n => format!("{n} emails"),
    };
    if !names.is_empty() {
        out.push_str(" with ");
        out.push_str(&listed(&names));
    }
    let dates = match (context.first_date, context.last_date) {
        (Some(first), Some(last)) => {
            let (first, last) = (date(first, now), date(last, now));
            Some(if first == last {
                first
            } else {
                format!("{first} to {last}")
            })
        }
        (Some(one), None) | (None, Some(one)) => Some(date(one, now)),
        (None, None) => None,
    };
    if let Some(dates) = dates {
        out.push_str(", ");
        out.push_str(&dates);
    }
    format!(" ({out})")
}

/// `names` as a sentence lists them: `A`, `A and B`, `A, B and C`.
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `text` safe in HTML.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use exo_store::{NoteParticipant, ThreadId};
    use jiff::Timestamp;

    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a timestamp")
    }

    fn now() -> Zoned {
        "2026-11-23T10:00:00-05:00[America/New_York]"
            .parse()
            .expect("a time")
    }

    fn person(name: Option<&str>, email: &str) -> NoteParticipant {
        NoteParticipant {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    fn context() -> NoteThread {
        NoteThread {
            thread_id: ThreadId::new("thread".to_owned()),
            subject: "Contract renewal".to_owned(),
            participants: vec![
                person(Some("Maya Okafor"), "maya@example.com"),
                person(Some("Dana Reyes"), "dana@example.com"),
                person(None, "sam@example.com"),
                person(Some("Lee"), "lee@example.com"),
            ],
            first_date: Some(at("2026-11-03T15:00:00Z")),
            last_date: Some(at("2026-11-12T15:00:00Z")),
            messages: 3,
        }
    }

    /// A note made at `created`, on message `message` (its sender and date) or the thread.
    fn note(number: Option<u32>, created: &str, html: &str, message: Option<(&str, &str)>) -> Note {
        Note {
            id: format!("note-{created}"),
            thread_id: ThreadId::new("thread".to_owned()),
            message_id: message.map(|(from, _)| format!("message-{from}")),
            message_from: message.map(|(from, _)| from.to_owned()),
            message_date: message.map(|(_, date)| at(date)),
            number,
            kind: "note".to_owned(),
            html: html.to_owned(),
            text: String::new(),
            files: Vec::new(),
            created_at: at(created),
            updated_at: at(created),
            deleted_at: None,
            version: 1,
        }
    }

    fn file(uuid: &str, name: &str, cid: Option<&str>) -> NoteFile {
        NoteFile {
            uuid: uuid.to_owned(),
            name: name.to_owned(),
            mime_type: "image/png".to_owned(),
            size: 10,
            content_id: cid.map(str::to_owned),
        }
    }

    #[test]
    fn the_shared_body_names_the_thread_and_each_note() {
        let notes = [
            note(
                Some(1),
                "2026-11-06T15:00:00Z",
                "<div>Ask about the fee</div>",
                Some(("Dana Reyes", "2026-11-03T15:00:00Z")),
            ),
            note(
                None,
                "2026-11-04T15:00:00Z",
                "<div>Renew for two years</div>",
                None,
            ),
        ];
        let shared = build(
            &context(),
            &notes,
            "maya@example.com",
            |_| OnMessage::Theirs,
            &now(),
        );
        assert_eq!(shared.subject, "Notes: Contract renewal");
        assert_eq!(
            shared.html,
            "<div>Notes on \u{201c}Contract renewal\u{201d} (3 emails with Dana Reyes, \
             sam@example.com and Lee, 3 Nov to 12 Nov).</div>\
             <hr><div><b>Thread note</b></div><div>4 Nov \u{b7} whole thread</div>\
             <div>Renew for two years</div>\
             <hr><div><b>Note 1</b></div><div>6 Nov \u{b7} on Dana's email of 3 Nov</div>\
             <div>Ask about the fee</div>"
        );

        // One email, one day, and no one else.
        let mut alone = context();
        alone.participants = vec![person(Some("Me"), "MAYA@example.com")];
        alone.messages = 1;
        alone.last_date = alone.first_date;
        let shared = build(
            &alone,
            &[],
            "maya@example.com",
            |_| OnMessage::Theirs,
            &now(),
        );
        assert_eq!(
            shared.html,
            "<div>Notes on \u{201c}Contract renewal\u{201d} (1 email, 3 Nov).</div>"
        );

        // A thread its notes kept nothing of is named by its subject alone.
        let unknown = NoteThread {
            participants: Vec::new(),
            first_date: None,
            last_date: None,
            messages: 0,
            ..context()
        };
        let shared = build(
            &unknown,
            &[],
            "maya@example.com",
            |_| OnMessage::Theirs,
            &now(),
        );
        assert_eq!(
            shared.html,
            "<div>Notes on \u{201c}Contract renewal\u{201d}.</div>"
        );
    }

    #[test]
    fn note_text_in_labels_is_escaped() {
        let mut context = context();
        context.subject = "Q&A <draft>".to_owned();
        context.participants = vec![person(Some("Tom & \"Jerry\""), "tj@example.com")];
        let notes = [note(
            Some(2),
            "2026-11-06T15:00:00Z",
            "<div>Kept <b>as is</b></div>",
            Some(("<Dana>", "2026-11-03T15:00:00Z")),
        )];
        let shared = build(
            &context,
            &notes,
            "maya@example.com",
            |_| OnMessage::Theirs,
            &now(),
        );
        assert_eq!(shared.subject, "Notes: Q&A <draft>", "the subject is text");
        assert!(
            shared.html.contains(
                "\u{201c}Q&amp;A &lt;draft&gt;\u{201d} (3 emails with Tom &amp; &quot;Jerry&quot;"
            ),
            "{}",
            shared.html
        );
        assert!(
            shared.html.contains("on &lt;Dana&gt;'s email of 3 Nov"),
            "{}",
            shared.html
        );
        assert!(
            shared.html.ends_with("<div>Kept <b>as is</b></div>"),
            "the note's HTML goes in as it is: {}",
            shared.html
        );
    }

    #[test]
    fn where_lines_cover_each_case() {
        let message = |from| Some((from, "2026-11-03T15:00:00Z"));
        let notes = [
            note(None, "2026-11-04T15:00:00Z", "", None),
            note(Some(1), "2026-11-05T15:00:00Z", "", message("Dana Reyes")),
            note(Some(2), "2026-11-06T15:00:00Z", "", message("Maya Okafor")),
            note(Some(3), "2026-11-07T15:00:00Z", "", message("Sam Lee")),
        ];
        let on = |note: &Note| match note.number {
            Some(2) => OnMessage::Mine,
            Some(3) => OnMessage::Deleted,
            _ => OnMessage::Theirs,
        };
        let shared = build(&context(), &notes, "maya@example.com", on, &now());
        let lines: Vec<&str> = shared
            .html
            .split("<div>")
            .filter(|part| part.contains(" \u{b7} "))
            .filter_map(|part| part.split("</div>").next())
            .collect();
        assert_eq!(
            lines,
            [
                "4 Nov \u{b7} whole thread",
                "5 Nov \u{b7} on Dana's email of 3 Nov",
                "6 Nov \u{b7} on my reply of 3 Nov",
                "7 Nov \u{b7} on a deleted email from Sam of 3 Nov",
            ]
        );
    }

    #[test]
    fn images_shown_go_inline_and_a_shared_content_id_is_made_unique() {
        let mut first = note(
            Some(1),
            "2026-11-05T15:00:00Z",
            "<div><img class=\"Exo-inline-image\" src-cid=\"c1\"></div>",
            None,
        );
        first.files = vec![
            file("u1", "board.png", Some("c1")),
            file("u2", "agenda.pdf", None),
            file("u3", "dropped.png", Some("gone")),
        ];
        let mut second = note(
            Some(2),
            "2026-11-06T15:00:00Z",
            "<div><img class=\"Exo-inline-image\" src-cid=\"c1\"></div>",
            None,
        );
        second.files = vec![
            file("u4", "copy.png", Some("c1")),
            file("u5", "plan.txt", None),
        ];
        let shared = build(
            &context(),
            &[first, second],
            "maya@example.com",
            |_| OnMessage::Theirs,
            &now(),
        );
        let names = |files: &[NoteFile]| files.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&shared.files), ["agenda.pdf", "plan.txt"]);
        assert_eq!(names(&shared.images), ["board.png", "copy.png"]);
        assert_eq!(shared.images[0].content_id.as_deref(), Some("c1"));
        let fresh = shared.images[1].content_id.clone().expect("a content id");
        assert_ne!(fresh, "c1");
        assert!(shared.html.contains("src-cid=\"c1\""), "{}", shared.html);
        assert!(
            shared.html.contains(&format!("src-cid=\"{fresh}\"")),
            "the later note shows its own: {}",
            shared.html
        );
    }
}
