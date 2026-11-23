//! The notes folder: a one-way copy of every thread's notes as Markdown, kept in a folder the user
//! chooses so other tools (a local AI among them) can read them. It's off until the user turns it
//! on, in the Notes dialog.
//!
//! Each noted thread has `{subject-slug}-{thread_id}.md`: YAML front matter about the thread,
//! then a section per live note. Beside it, `{same name}.history.jsonl` has one line per version
//! of every note, deleted ones included. The notes' files are copied to `files/{thread_id}/` and
//! linked relatively. A thread's files are written whole whenever its notes change, so edits made
//! to them are lost.
//!
//! Every path written is the folder joined with a name made only of the sanitized stem, the
//! thread's id, a file's uuid and its sanitized name, so nothing lands outside the folder.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use exo_store::{Note, NoteFile, NoteThread, NoteVersion};

use crate::copy::turndown::{self, Tree};

/// The longest a file name's slug of the subject gets, in characters.
const SLUG_LENGTH: usize = 60;
/// The longest a copied file's name gets, in bytes, past its uuid.
const NAME_LENGTH: usize = 150;
/// The folder the notes' files are copied into, by thread.
const FILES: &str = "files";

/// Where the notes folder goes unless the user chooses another:
/// `~/Documents/Email Notes/{account}`.
pub fn default_folder(account: &str) -> PathBuf {
    home()
        .join("Documents")
        .join("Email Notes")
        .join(safe_name(account))
}

/// `folder` as typed or chosen, with a leading `~` meaning the home folder.
pub fn expand(folder: &str) -> PathBuf {
    match folder.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(folder),
    }
}

/// The home folder, or `~` when it isn't known.
fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("~"), PathBuf::from)
}

/// The name a thread's files share, past their extensions: the subject lowercased, with every
/// run of anything but letters and digits made one `-`, trimmed, at most 60 characters, then
/// `-{thread_id}`. A subject that gives nothing makes `notes-{thread_id}`.
pub fn file_stem(context: &NoteThread) -> String {
    let mut slug = String::new();
    for c in context.subject.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(SLUG_LENGTH).collect();
    let slug = slug.trim_end_matches('-');
    let id = safe_id(context.thread_id.as_str());
    if slug.is_empty() {
        format!("notes-{id}")
    } else {
        format!("{slug}-{id}")
    }
}

/// A thread's `.md`: YAML front matter (`thread_id`, `subject`, `participants`, `first_date`,
/// `last_date`, `updated`), then for each live note `## {label}`, a list of its `id`, `kind`,
/// `created`, `edited` and what it's `on`, and its text as Markdown. Its images link to their
/// copies in `files/{thread_id}/`, and its other files are listed under `Files:`. `gone` names
/// the messages no longer stored, which a note on one says.
pub fn render(context: &NoteThread, notes: &[Note], gone: &BTreeSet<String>) -> String {
    let date =
        |at: Option<jiff::Timestamp>| at.map_or_else(|| "null".to_owned(), |at| at.to_string());
    let updated = notes
        .iter()
        .flat_map(|note| [Some(note.updated_at), note.deleted_at])
        .flatten()
        .max();
    let mut out = String::from("---\n");
    out += &format!("thread_id: {}\n", yaml(context.thread_id.as_str()));
    out += &format!("subject: {}\n", yaml(&context.subject));
    if context.participants.is_empty() {
        out += "participants: []\n";
    } else {
        out += "participants:\n";
        for participant in &context.participants {
            let person = match participant.name.as_deref().filter(|name| !name.is_empty()) {
                Some(name) => format!("{name} <{}>", participant.email),
                None => participant.email.clone(),
            };
            out += &format!("  - {}\n", yaml(&person));
        }
    }
    out += &format!("first_date: {}\n", date(context.first_date));
    out += &format!("last_date: {}\n", date(context.last_date));
    out += &format!("updated: {}\n", date(updated));
    out += "---\n";
    let id = safe_id(context.thread_id.as_str());
    for note in notes.iter().filter(|note| note.deleted_at.is_none()) {
        out += &format!("\n## {}\n\n", note.label());
        out += &format!("- id: {}\n", note.id);
        out += &format!("- kind: {}\n", note.kind);
        out += &format!("- created: {}\n", note.created_at);
        out += &format!("- edited: {}\n", note.updated_at);
        out += &format!("- on: {}\n", on(note, gone));
        let text = note_markdown(note, &id);
        if !text.is_empty() {
            out += &format!("\n{text}\n");
        }
        let listed: Vec<&NoteFile> = note
            .files
            .iter()
            .filter(|file| file.content_id.is_none())
            .collect();
        if !listed.is_empty() {
            out += "\nFiles:\n\n";
            for file in listed {
                out += &format!("- [{}]({})\n", label_text(&file.name), link(&id, file));
            }
        }
    }
    out
}

/// What a note is on: `thread`, or its message's sender and date as the note saved them, with
/// ` (deleted)` once the message is gone.
fn on(note: &Note, gone: &BTreeSet<String>) -> String {
    let Some(message) = &note.message_id else {
        return "thread".to_owned();
    };
    let parts: Vec<String> = note
        .message_from
        .iter()
        .cloned()
        .chain(note.message_date.map(|date| date.to_string()))
        .collect();
    let mut on = if parts.is_empty() {
        "a message".to_owned()
    } else {
        parts.join(" \u{b7} ")
    };
    if gone.contains(message) {
        on += " (deleted)";
    }
    on
}

/// A note's text as Markdown, its inline images linking to their copies of thread `id`'s.
fn note_markdown(note: &Note, id: &str) -> String {
    let mut tree = Tree::parse(&note.html);
    for image in tree.elements(&["IMG"]) {
        let Some(cid) = tree.attribute(image, "src-cid").map(str::to_owned) else {
            continue;
        };
        match note
            .files
            .iter()
            .find(|file| file.content_id.as_deref() == Some(cid.as_str()))
        {
            Some(file) => {
                tree.set_attribute(image, "src", &link(id, file));
                tree.set_attribute(image, "alt", &label_text(&file.name));
            }
            // An image whose file isn't the note's shows nothing.
            None => tree.set_attribute(image, "src", ""),
        }
    }
    turndown::turndown(tree)
}

/// One line of a thread's `.history.jsonl`: a JSON object of note `note_id`'s `version`.
pub fn history_line(note_id: &str, version: &NoteVersion) -> String {
    serde_json::json!({
        "note_id": note_id,
        "version": version.version,
        "saved_at": version.saved_at.to_string(),
        "change": version.change.as_str(),
        "text": version.text,
        "html": version.html,
        "files": version.files,
    })
    .to_string()
}

/// Writes a thread's notes to `folder`: `{stem}.md` (removed when no note is live) and
/// `{stem}.history.jsonl`, each whole, through a temporary file renamed over it. The live notes'
/// files are copied from `files` (the store's, by uuid) to `files/{thread_id}/` when missing
/// there. Files left from the thread's old subject go. `gone` names the messages no longer
/// stored.
///
/// # Errors
/// When the folder can't be made, read or written.
pub fn export(
    folder: &Path,
    files: &Path,
    context: &NoteThread,
    notes: &[Note],
    versions: &[(String, NoteVersion)],
    gone: &BTreeSet<String>,
) -> io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let id = safe_id(context.thread_id.as_str());
    let stem = file_stem(context);
    let markdown = format!("{stem}.md");
    let history = format!("{stem}.history.jsonl");
    let (old_markdown, old_history) = (format!("-{id}.md"), format!("-{id}.history.jsonl"));
    for entry in std::fs::read_dir(folder)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        let old = (name.ends_with(&old_markdown) && name != markdown)
            || (name.ends_with(&old_history) && name != history);
        if old {
            remove(&folder.join(name))?;
        }
    }
    if notes.iter().any(|note| note.deleted_at.is_none()) {
        // The files first, so the Markdown's links work once it's there.
        let copies = folder.join(FILES).join(&id);
        let live = notes.iter().filter(|note| note.deleted_at.is_none());
        for file in live.flat_map(|note| &note.files) {
            let copy = copies.join(file_name(file));
            if copy.exists() {
                continue;
            }
            match std::fs::read(files.join(&file.uuid)) {
                Ok(bytes) => {
                    std::fs::create_dir_all(&copies)?;
                    write_whole(&copy, &bytes)?;
                }
                // The note's file isn't kept on this device: its link goes nowhere.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    log::warn!("note file {} isn't kept, so isn't copied", file.uuid);
                }
                Err(error) => return Err(error),
            }
        }
        write_whole(
            &folder.join(&markdown),
            render(context, notes, gone).as_bytes(),
        )?;
    } else {
        remove(&folder.join(&markdown))?;
    }
    if !versions.is_empty() {
        let mut lines: String = versions
            .iter()
            .map(|(note, version)| history_line(note, version))
            .collect::<Vec<_>>()
            .join("\n");
        lines.push('\n');
        write_whole(&folder.join(&history), lines.as_bytes())?;
    }
    Ok(())
}

/// Removes the file at `path`, which may not be there.
fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Writes `bytes` to `path` whole: to a temporary file beside it, renamed over it, so a reader
/// never sees half of it.
fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let written =
        std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// A copied file's name: `{uuid}-{name}`, both made safe.
fn file_name(file: &NoteFile) -> String {
    format!("{}-{}", safe_name(&file.uuid), safe_name(&file.name))
}

/// A copied file's relative link from the thread's `.md`, percent-encoded as a Markdown link's
/// destination needs.
fn link(id: &str, file: &NoteFile) -> String {
    let path = format!("{FILES}/{id}/{}", file_name(file));
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~/".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out += &format!("%{byte:02X}");
        }
    }
    out
}

/// A file's name as a link's text, its brackets escaped.
fn label_text(name: &str) -> String {
    name.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// `name` as one part of a path: path separators and control characters become `_`, it never
/// starts with a dot, and it's at most [`NAME_LENGTH`] bytes. An empty name is `file`.
fn safe_name(name: &str) -> String {
    let mut safe = String::new();
    for c in name.trim().chars() {
        let c = if c == '/' || c == '\\' || c == ':' || c.is_control() {
            '_'
        } else {
            c
        };
        if safe.len() + c.len_utf8() > NAME_LENGTH {
            break;
        }
        safe.push(c);
    }
    if safe.starts_with('.') {
        safe.replace_range(..1, "_");
    }
    if safe.is_empty() {
        "file".to_owned()
    } else {
        safe
    }
}

/// A thread id as file names have it: letters, digits and `_`, anything else made `_`.
fn safe_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// `text` as a YAML double-quoted string, its `"`, `\` and control characters escaped.
fn yaml(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            '\n' => out += "\\n",
            '\r' => out += "\\r",
            '\t' => out += "\\t",
            c if c.is_control() => out += &format!("\\u{:04X}", u32::from(c)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_store::{NoteChange, NoteParticipant, ThreadId};

    fn at(text: &str) -> jiff::Timestamp {
        text.parse().expect("a valid time")
    }

    fn context(subject: &str) -> NoteThread {
        NoteThread {
            thread_id: ThreadId::new("18c2f0a9d3e4b5a6"),
            subject: subject.to_owned(),
            participants: vec![
                NoteParticipant {
                    name: Some("Dana Reyes".to_owned()),
                    email: "dana@calloway.co".to_owned(),
                },
                NoteParticipant {
                    name: None,
                    email: "ops@calloway.co".to_owned(),
                },
            ],
            first_date: Some(at("2026-11-03T10:00:00Z")),
            last_date: Some(at("2026-11-05T16:30:00Z")),
            messages: 2,
        }
    }

    fn note(id: &str, number: Option<u32>, html: &str) -> Note {
        Note {
            id: id.to_owned(),
            thread_id: ThreadId::new("18c2f0a9d3e4b5a6"),
            message_id: number.map(|_| "m1".to_owned()),
            message_from: number.map(|_| "Dana Reyes".to_owned()),
            message_date: number.map(|_| at("2026-11-03T10:00:00Z")),
            number,
            kind: "note".to_owned(),
            html: html.to_owned(),
            text: String::new(),
            files: Vec::new(),
            created_at: at("2026-11-06T09:00:00Z"),
            updated_at: at("2026-11-06T09:30:00Z"),
            deleted_at: None,
            version: 1,
        }
    }

    fn file(uuid: &str, name: &str, cid: Option<&str>) -> NoteFile {
        NoteFile {
            uuid: uuid.to_owned(),
            name: name.to_owned(),
            mime_type: "image/png".to_owned(),
            size: 3,
            content_id: cid.map(str::to_owned),
        }
    }

    #[test]
    fn the_stem_is_safe_for_any_subject() {
        assert_eq!(
            file_stem(&context("Re: Q4/Plan: \u{201c}final\u{201d}?")),
            "re-q4-plan-final-18c2f0a9d3e4b5a6"
        );
        assert_eq!(file_stem(&context("")), "notes-18c2f0a9d3e4b5a6");
        assert_eq!(file_stem(&context("../..")), "notes-18c2f0a9d3e4b5a6");
        let long = file_stem(&context(&"ab ".repeat(100)));
        let slug = long.strip_suffix("-18c2f0a9d3e4b5a6").expect("the id last");
        assert!(slug.chars().count() <= 60, "{slug}");
        assert!(!slug.ends_with('-') && slug.starts_with("ab-ab"), "{slug}");
        for subject in [
            "Re: Q4/Plan: \u{201c}final\u{201d}?",
            "",
            "../..",
            "a\\b\0c",
        ] {
            let stem = file_stem(&context(subject));
            assert!(
                stem.chars().all(|c| c.is_alphanumeric() || c == '-'),
                "{stem}"
            );
        }
        let mut odd = context("Plan");
        odd.thread_id = ThreadId::new("../x");
        assert_eq!(file_stem(&odd), "plan-___x");
    }

    #[test]
    fn rendering_has_front_matter_and_a_section_per_note() {
        let mut deleted = note("n3", Some(2), "<div>Gone</div>");
        deleted.deleted_at = Some(at("2026-11-07T08:00:00Z"));
        let mut on_message = note("n2", Some(1), "<div>Strong on systems</div>");
        on_message.files = vec![
            file("u1", "board.png", Some("c1")),
            file("u2", "agenda (v2).txt", None),
        ];
        on_message.html =
            "<div>Board <img class=\"Exo-inline-image\" src-cid=\"c1\"></div>".to_owned();
        let notes = [
            note("n1", None, "<div>Ask about the budget</div>"),
            on_message,
            deleted,
        ];
        let gone = BTreeSet::from(["m1".to_owned()]);
        let markdown = render(&context("Q4 plan"), &notes, &gone);
        assert_eq!(
            markdown,
            "---\n\
             thread_id: \"18c2f0a9d3e4b5a6\"\n\
             subject: \"Q4 plan\"\n\
             participants:\n  \
               - \"Dana Reyes <dana@calloway.co>\"\n  \
               - \"ops@calloway.co\"\n\
             first_date: 2026-11-03T10:00:00Z\n\
             last_date: 2026-11-05T16:30:00Z\n\
             updated: 2026-11-07T08:00:00Z\n\
             ---\n\
             \n\
             ## Thread note\n\
             \n\
             - id: n1\n\
             - kind: note\n\
             - created: 2026-11-06T09:00:00Z\n\
             - edited: 2026-11-06T09:30:00Z\n\
             - on: thread\n\
             \n\
             Ask about the budget\n\
             \n\
             ## Note 1\n\
             \n\
             - id: n2\n\
             - kind: note\n\
             - created: 2026-11-06T09:00:00Z\n\
             - edited: 2026-11-06T09:30:00Z\n\
             - on: Dana Reyes \u{b7} 2026-11-03T10:00:00Z (deleted)\n\
             \n\
             Board ![board.png](files/18c2f0a9d3e4b5a6/u1-board.png)\n\
             \n\
             Files:\n\
             \n\
             - [agenda (v2).txt](files/18c2f0a9d3e4b5a6/u2-agenda%20%28v2%29.txt)\n"
        );
        assert!(!markdown.contains("Gone"), "a deleted note has no section");
    }

    #[test]
    fn yaml_strings_are_escaped() {
        assert_eq!(
            yaml("Say \"hi\" \\ to\nall\u{7}"),
            "\"Say \\\"hi\\\" \\\\ to\\nall\\u0007\""
        );
        let markdown = render(&context("A \"quoted\" \\ subject"), &[], &BTreeSet::new());
        assert!(
            markdown.contains("subject: \"A \\\"quoted\\\" \\\\ subject\"\n"),
            "{markdown}"
        );
    }

    #[test]
    fn formatting_survives_as_markdown() {
        let html = "<div>Some <b>bold</b> words</div><ul><li>one</li><li>two</li></ul>\
                    <div>See <a href=\"https://calloway.co/q4\">the deck</a></div>";
        let markdown = note_markdown(&note("n1", None, html), "t");
        assert_eq!(
            markdown,
            "Some **bold** words\n\n*   one\n*   two\n\nSee [the deck](https://calloway.co/q4)"
        );
    }

    #[test]
    fn a_history_line_is_valid_json() {
        let version = NoteVersion {
            version: 2,
            html: "<div>Line \"one\"\nand two</div>".to_owned(),
            text: "Line \"one\"\nand two".to_owned(),
            files: vec![file("u1", "board.png", Some("c1"))],
            saved_at: at("2026-11-06T09:30:00Z"),
            change: NoteChange::Edited,
        };
        let line = history_line("n1", &version);
        assert!(!line.contains('\n'), "one line: {line}");
        let value: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(value["note_id"], "n1");
        assert_eq!(value["version"], 2);
        assert_eq!(value["saved_at"], "2026-11-06T09:30:00Z");
        assert_eq!(value["change"], "edited");
        assert_eq!(value["text"], "Line \"one\"\nand two");
        assert_eq!(value["html"], "<div>Line \"one\"\nand two</div>");
        assert_eq!(value["files"][0]["uuid"], "u1");
        assert_eq!(value["files"][0]["content_id"], "c1");
    }

    #[test]
    fn file_names_stay_in_their_folder() {
        assert_eq!(safe_name("../../etc/passwd"), "_._.._etc_passwd");
        assert_eq!(safe_name("a\nb\u{0}c"), "a_b_c");
        assert_eq!(safe_name(".hidden"), "_hidden");
        assert_eq!(safe_name(""), "file");
        assert!(safe_name(&"\u{e9}".repeat(400)).len() <= NAME_LENGTH);
    }

    #[test]
    fn a_folder_with_a_tilde_is_in_the_home_folder() {
        let home = home();
        assert_eq!(expand("~"), home);
        assert_eq!(expand("~/Notes"), home.join("Notes"));
        assert_eq!(expand("/tmp/Notes"), PathBuf::from("/tmp/Notes"));
        assert_eq!(
            default_folder("maya@brightmoor.co"),
            home.join("Documents/Email Notes/maya@brightmoor.co")
        );
    }

    #[test]
    fn an_export_writes_renames_and_removes() {
        let root = std::env::temp_dir().join(format!(
            "exo-notes-markdown-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let (folder, files) = (root.join("folder"), root.join("files"));
        std::fs::create_dir_all(&files).expect("a folder");
        std::fs::write(files.join("u2"), b"1. Budget").expect("written");
        let mut live = note("n1", None, "<div>Agenda</div>");
        live.files = vec![file("u2", "agenda.txt", None)];
        let version = NoteVersion {
            version: 1,
            html: live.html.clone(),
            text: "Agenda".to_owned(),
            files: live.files.clone(),
            saved_at: live.created_at,
            change: NoteChange::Created,
        };
        let versions = [("n1".to_owned(), version)];
        let none = BTreeSet::new();
        export(
            &folder,
            &files,
            &context("Old"),
            &[live.clone()],
            &versions,
            &none,
        )
        .expect("exported");
        assert!(folder.join("old-18c2f0a9d3e4b5a6.md").is_file());
        export(
            &folder,
            &files,
            &context("New"),
            &[live.clone()],
            &versions,
            &none,
        )
        .expect("exported");
        let mut names: Vec<String> = std::fs::read_dir(&folder)
            .expect("listed")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "files",
                "new-18c2f0a9d3e4b5a6.history.jsonl",
                "new-18c2f0a9d3e4b5a6.md"
            ]
        );
        assert_eq!(
            std::fs::read(folder.join("files/18c2f0a9d3e4b5a6/u2-agenda.txt")).expect("copied"),
            b"1. Budget"
        );
        live.deleted_at = Some(at("2026-11-07T08:00:00Z"));
        export(&folder, &files, &context("New"), &[live], &versions, &none).expect("exported");
        assert!(
            !folder.join("new-18c2f0a9d3e4b5a6.md").exists(),
            "no live notes"
        );
        assert!(
            folder.join("new-18c2f0a9d3e4b5a6.history.jsonl").is_file(),
            "the history stays"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
