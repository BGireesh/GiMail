//! Thread notes: saving, numbering, versions, deleting and restoring, unsaved notes, and what
//! keeps them: the thread's deletion in Gmail, sign-out and the standing backup.

mod support;

use std::path::{Path, PathBuf};

use exo_store::{ListId, NoteChange, NoteDraft, NoteFile, SaveNote, Store, ThreadDetail, ThreadId};
use jiff::{SignedDuration, Timestamp};
use support::{ACCOUNT, Harness};

/// A time `minutes` after a fixed start, so notes sort by when they were written.
fn at(minutes: i64) -> Timestamp {
    "2026-11-23T10:00:00Z".parse::<Timestamp>().expect("a time")
        + SignedDuration::from_mins(minutes)
}

/// An inbox conversation without drafts, as the thread view reads it.
async fn conversation(harness: &Harness) -> ThreadDetail {
    let id = harness
        .all(&ListId::Inbox)
        .await
        .into_iter()
        .find(|thread| thread.drafts == 0)
        .expect("an inbox thread")
        .id;
    harness
        .store
        .thread(&id)
        .await
        .expect("reads")
        .expect("stored")
}

/// A save of `text` from the editor `draft`, on `message` of `thread` (or the whole thread),
/// editing `note` when it's saved already.
fn save(
    thread: &ThreadId,
    message: Option<&str>,
    draft: &str,
    note: Option<&str>,
    text: &str,
) -> SaveNote {
    SaveNote {
        draft_id: draft.into(),
        note_id: note.map(str::to_owned),
        thread_id: thread.clone(),
        message_id: message.map(str::to_owned),
        html: format!("<p>{text}</p>"),
        text: text.into(),
        files: Vec::new(),
    }
}

fn file(uuid: &str, name: &str) -> NoteFile {
    NoteFile {
        uuid: uuid.into(),
        name: name.into(),
        mime_type: "application/pdf".into(),
        size: 2048,
        content_id: None,
    }
}

async fn saved(store: &Store, save: SaveNote, now: Timestamp) -> exo_store::Note {
    store
        .save_note(save, now)
        .await
        .expect("saves")
        .expect("a note")
}

fn backup(dir: &Path) -> PathBuf {
    dir.join(format!("{ACCOUNT}.notes-backup.json"))
}

/// How many notes the notes' search index finds for `word`, read through a second connection.
fn indexed(dir: &Path, word: &str) -> i64 {
    rusqlite::Connection::open(dir.join(format!("{ACCOUNT}.sqlite3")))
        .expect("opens")
        .query_row(
            "SELECT count(*) FROM notes_fts WHERE notes_fts MATCH ?1",
            [word],
            |row| row.get(0),
        )
        .expect("searches")
}

#[tokio::test]
async fn a_saved_note_comes_back_with_its_number_and_context() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let first = &thread.messages[0];

    let whole = saved(store, save(&thread.id, None, "a", None, "Renewal"), at(0)).await;
    let one = saved(
        store,
        save(&thread.id, Some(&first.id), "b", None, "Last contract"),
        at(1),
    )
    .await;
    let two = saved(
        store,
        save(&thread.id, Some(&first.id), "c", None, "Team findings"),
        at(2),
    )
    .await;
    assert_eq!(
        (whole.number, whole.label().as_str()),
        (None, "Thread note")
    );
    assert_eq!((one.number, one.label().as_str()), (Some(1), "Note 1"));
    assert_eq!((two.number, two.label().as_str()), (Some(2), "Note 2"));
    assert_eq!(one.id, "b", "a new note takes its editor's id");
    assert_eq!(one.message_date, Some(first.date));
    assert!(one.message_from.is_some(), "the message's sender is kept");
    assert_eq!((one.version, one.kind.as_str()), (1, "note"));

    let notes = store.thread_notes(&thread.id).await.expect("reads");
    let context = notes.context.expect("the thread's context");
    assert_eq!(context.subject, thread.subject);
    assert_eq!(context.thread_id, thread.id);
    assert!(!context.participants.is_empty());
    assert!(context.first_date.is_some() && context.last_date >= context.first_date);
    assert_eq!(notes.notes, vec![whole, one, two], "in the order written");
    assert!(notes.deleted.is_empty() && notes.drafts.is_empty());
    assert_eq!(
        harness
            .summary(&ListId::Inbox, &thread.id)
            .await
            .expect("listed")
            .notes,
        3,
        "the row counts them"
    );
}

#[tokio::test]
async fn numbers_are_never_reused() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let message = thread.messages[0].id.as_str();

    let one = saved(
        store,
        save(&thread.id, Some(message), "a", None, "One"),
        at(0),
    )
    .await;
    saved(
        store,
        save(&thread.id, Some(message), "b", None, "Two"),
        at(1),
    )
    .await;
    store.delete_note(&one.id, at(2)).await.expect("deletes");
    let three = saved(
        store,
        save(&thread.id, Some(message), "c", None, "Three"),
        at(3),
    )
    .await;
    assert_eq!(three.number, Some(3));
    assert_eq!(
        store
            .thread_notes(&thread.id)
            .await
            .expect("reads")
            .notes
            .iter()
            .map(|note| note.number)
            .collect::<Vec<_>>(),
        [Some(2), Some(3)]
    );
}

#[tokio::test]
async fn notes_written_in_the_same_instant_come_back_by_number() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let message = thread.messages[0].id.as_str();
    // Their ids sort the other way round from their numbers.
    for (draft, text) in [("z", "First"), ("a", "Second")] {
        saved(
            store,
            save(&thread.id, Some(message), draft, None, text),
            at(0),
        )
        .await;
    }
    assert_eq!(
        store
            .thread_notes(&thread.id)
            .await
            .expect("reads")
            .notes
            .iter()
            .map(exo_store::Note::label)
            .collect::<Vec<_>>(),
        ["Note 1", "Note 2"]
    );
}

#[tokio::test]
async fn an_edit_adds_a_version_and_an_unchanged_save_does_not() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;

    let note = saved(
        store,
        save(&thread.id, None, "a", None, "Draft terms"),
        at(0),
    )
    .await;
    let edited = saved(
        store,
        save(&thread.id, None, "a", Some(&note.id), "Final terms"),
        at(5),
    )
    .await;
    assert_eq!((edited.version, edited.text.as_str()), (2, "Final terms"));
    assert_eq!((edited.created_at, edited.updated_at), (at(0), at(5)));
    let again = saved(
        store,
        save(&thread.id, None, "a", Some(&note.id), "Final terms"),
        at(9),
    )
    .await;
    assert_eq!(again, edited, "nothing changed");
    let versions = store.note_versions(&note.id).await.expect("reads");
    assert_eq!(
        versions
            .iter()
            .map(|version| (
                version.version,
                version.change,
                version.text.as_str(),
                version.saved_at
            ))
            .collect::<Vec<_>>(),
        [
            (2, NoteChange::Edited, "Final terms", at(5)),
            (1, NoteChange::Created, "Draft terms", at(0)),
        ],
        "newest first"
    );
}

#[tokio::test]
async fn deleting_keeps_the_note_in_history_and_restore_brings_it_back() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let note = saved(store, save(&thread.id, None, "a", None, "Keep me"), at(0)).await;

    store.delete_note(&note.id, at(1)).await.expect("deletes");
    let notes = store.thread_notes(&thread.id).await.expect("reads");
    assert!(notes.notes.is_empty());
    assert_eq!(notes.deleted.len(), 1);
    assert_eq!(notes.deleted[0].deleted_at, Some(at(1)));
    assert_eq!(notes.deleted[0].text, "Keep me");
    assert_eq!(
        harness
            .summary(&ListId::Inbox, &thread.id)
            .await
            .expect("listed")
            .notes,
        0,
        "a deleted note isn't counted"
    );

    let restored = store
        .restore_note(&note.id, None, at(2))
        .await
        .expect("restores");
    assert_eq!(restored.deleted_at, None);
    assert_eq!((restored.version, restored.text.as_str()), (3, "Keep me"));
    let notes = store.thread_notes(&thread.id).await.expect("reads");
    assert_eq!(notes.notes, vec![restored]);
    assert!(notes.deleted.is_empty());
    assert_eq!(
        store
            .note_versions(&note.id)
            .await
            .expect("reads")
            .iter()
            .map(|version| version.change)
            .collect::<Vec<_>>(),
        [
            NoteChange::Restored,
            NoteChange::Deleted,
            NoteChange::Created
        ]
    );
}

#[tokio::test]
async fn restoring_an_old_version_saves_it_as_a_new_one() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let note = saved(store, save(&thread.id, None, "a", None, "First"), at(0)).await;
    saved(
        store,
        save(&thread.id, None, "a", Some(&note.id), "Second"),
        at(1),
    )
    .await;

    let restored = store
        .restore_note(&note.id, Some(1), at(2))
        .await
        .expect("restores");
    assert_eq!(
        (
            restored.version,
            restored.html.as_str(),
            restored.text.as_str()
        ),
        (3, "<p>First</p>", "First")
    );
    assert_eq!(restored.updated_at, at(2));
    let versions = store.note_versions(&note.id).await.expect("reads");
    assert_eq!(versions.len(), 3, "the edit is kept");
    assert_eq!(
        (
            versions[0].version,
            versions[0].change,
            versions[0].text.as_str()
        ),
        (3, NoteChange::Restored, "First")
    );
    assert_eq!(
        store.thread_notes(&thread.id).await.expect("reads").notes,
        vec![restored]
    );
}

#[tokio::test]
async fn an_empty_new_note_is_dropped() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let message = thread.messages[0].id.as_str();
    store
        .save_note_draft(NoteDraft {
            id: "a".into(),
            thread_id: thread.id.clone(),
            message_id: Some(message.into()),
            note_id: None,
            html: "<p> </p>".into(),
            text: " ".into(),
            files: Vec::new(),
            updated_at: at(0),
        })
        .await
        .expect("saves the draft");

    let dropped = store
        .save_note(save(&thread.id, Some(message), "a", None, " \n"), at(1))
        .await
        .expect("saves");
    assert_eq!(dropped, None);
    let notes = store.thread_notes(&thread.id).await.expect("reads");
    assert!(
        notes.notes.is_empty() && notes.drafts.is_empty(),
        "nor its draft"
    );
    let next = saved(
        store,
        save(&thread.id, Some(message), "b", None, "Real"),
        at(2),
    )
    .await;
    assert_eq!(next.number, Some(1), "it took no number");
}

#[tokio::test]
async fn a_draft_survives_reopening_and_is_gone_after_the_save() {
    let mut harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    let draft = NoteDraft {
        id: "a".into(),
        thread_id: thread.id.clone(),
        message_id: None,
        note_id: None,
        html: "<p>Half a thought</p>".into(),
        text: "Half a thought".into(),
        files: vec![file("f1", "quote.pdf")],
        updated_at: at(0),
    };
    harness
        .store
        .save_note_draft(draft.clone())
        .await
        .expect("saves the draft");
    harness.reopen().await;

    let notes = harness.store.thread_notes(&thread.id).await.expect("reads");
    assert_eq!(notes.drafts, vec![draft]);
    assert!(notes.notes.is_empty(), "a draft isn't a note");
    saved(
        &harness.store,
        save(&thread.id, None, "a", None, "Half a thought"),
        at(1),
    )
    .await;
    let notes = harness.store.thread_notes(&thread.id).await.expect("reads");
    assert!(notes.drafts.is_empty());
    assert_eq!(notes.notes.len(), 1);
}

#[tokio::test]
async fn notes_survive_the_threads_deletion() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let note = saved(
        store,
        save(
            &thread.id,
            Some(&thread.messages[0].id),
            "a",
            None,
            "Still here",
        ),
        at(0),
    )
    .await;

    harness
        .gmail
        .gmail()
        .delete_thread(thread.id.as_str())
        .await
        .expect("deletes");
    store.sync().await.expect("syncs");
    assert!(store.thread(&thread.id).await.expect("reads").is_none());
    let notes = store.thread_notes(&thread.id).await.expect("reads");
    assert_eq!(notes.notes, vec![note]);
    assert_eq!(
        notes.context.expect("the thread's context").subject,
        thread.subject
    );
}

#[tokio::test]
async fn the_backup_restores_into_an_empty_store() {
    let harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    let store = &harness.store;
    let note = saved(
        store,
        SaveNote {
            files: vec![file("f1", "quote.pdf")],
            ..save(
                &thread.id,
                Some(&thread.messages[0].id),
                "a",
                None,
                "The quokka deal",
            )
        },
        at(0),
    )
    .await;
    saved(
        store,
        save(&thread.id, None, "b", None, "Whole thread"),
        at(1),
    )
    .await;
    store.delete_note("b", at(2)).await.expect("deletes");
    store
        .save_note_draft(NoteDraft {
            id: note.id.clone(),
            thread_id: thread.id.clone(),
            message_id: note.message_id.clone(),
            note_id: Some(note.id.clone()),
            html: "<p>The quokka deal, signed</p>".into(),
            text: "The quokka deal, signed".into(),
            files: note.files.clone(),
            updated_at: at(3),
        })
        .await
        .expect("saves the draft");
    // Drafts are backed up with the next save, delete or restore.
    store.backup_notes().await.expect("backs up");
    let before = store.thread_notes(&thread.id).await.expect("reads");
    let versions = store.note_versions(&note.id).await.expect("reads");

    let Harness {
        server, dir, store, ..
    } = harness;
    drop(store);
    Store::delete(dir.path(), ACCOUNT).expect("signs out");
    assert!(backup(dir.path()).exists(), "signing out keeps the backup");
    let store = support::open(&server, dir.path()).await;
    let mut backfilling = true;
    for _ in 0..50 {
        backfilling = store.sync().await.expect("syncs").backfilling;
        if !backfilling {
            break;
        }
    }
    assert!(!backfilling);

    assert_eq!(store.thread_notes(&thread.id).await.expect("reads"), before);
    assert_eq!(
        store.note_versions(&note.id).await.expect("reads"),
        versions
    );
    assert_eq!(
        indexed(dir.path(), "quokka"),
        1,
        "the live note is searchable"
    );
    assert_eq!(indexed(dir.path(), "whole"), 0, "the deleted one isn't");
}

#[tokio::test]
async fn concurrent_saves_leave_one_version_each_and_a_whole_backup() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let message = thread.messages[0].id.as_str();

    let (first, second, draft) = tokio::join!(
        store.save_note(save(&thread.id, Some(message), "a", None, "First"), at(0)),
        store.save_note(save(&thread.id, Some(message), "b", None, "Second"), at(0)),
        store.save_note_draft(NoteDraft {
            id: "c".into(),
            thread_id: thread.id.clone(),
            message_id: None,
            note_id: None,
            html: "<p>Typing</p>".into(),
            text: "Typing".into(),
            files: Vec::new(),
            updated_at: at(0),
        }),
    );
    let (first, second) = (
        first.expect("saves").expect("a note"),
        second.expect("saves").expect("a note"),
    );
    draft.expect("saves the draft");
    let mut numbers = [first.number, second.number];
    numbers.sort();
    assert_eq!(numbers, [Some(1), Some(2)], "one number each");
    for id in ["a", "b"] {
        assert_eq!(store.note_versions(id).await.expect("reads").len(), 1);
    }

    let text = std::fs::read_to_string(backup(harness.dir.path())).expect("a backup");
    let backup: serde_json::Value = serde_json::from_str(&text).expect("a whole JSON document");
    let mut ids: Vec<&str> = backup["notes"]
        .as_array()
        .expect("the notes")
        .iter()
        .map(|note| note["id"].as_str().expect("an id"))
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(
        backup["note_versions"].as_array().expect("versions").len(),
        2
    );
    let left: Vec<String> = std::fs::read_dir(harness.dir.path())
        .expect("lists")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains("notes-backup"))
        .collect();
    assert_eq!(
        left,
        [format!("{ACCOUNT}.notes-backup.json")],
        "no temp file is left"
    );
}

/// The backups beside the store in `dir` that couldn't be restored, with what each holds.
fn set_aside(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("lists")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".notes-backup.unrestored-"))
        })
        .map(|path| std::fs::read_to_string(path).expect("reads"))
        .collect()
}

/// Signs out of `harness`'s store, leaves `backup` as its notes' backup, and opens it again.
async fn reopen_with_backup(harness: Harness, backup_text: &str) -> (Harness, Store) {
    let Harness {
        server,
        dir,
        store,
        gmail,
    } = harness;
    drop(store);
    Store::delete(dir.path(), ACCOUNT).expect("signs out");
    std::fs::write(backup(dir.path()), backup_text).expect("writes the backup");
    let store = support::open(&server, dir.path()).await;
    (
        Harness {
            server,
            dir,
            store: store.clone(),
            gmail,
        },
        store,
    )
}

#[tokio::test]
async fn a_backup_that_wont_restore_is_set_aside_not_overwritten() {
    let harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    let (harness, store) = reopen_with_backup(harness, "not a backup {").await;
    let dir = harness.dir.path();
    assert!(
        store
            .thread_notes(&thread.id)
            .await
            .expect("reads")
            .notes
            .is_empty(),
        "the store opens without notes"
    );
    assert_eq!(
        set_aside(dir),
        ["not a backup {"],
        "the backup is set aside"
    );

    saved(&store, save(&thread.id, None, "a", None, "Fresh"), at(0)).await;
    assert_eq!(
        set_aside(dir),
        ["not a backup {"],
        "a save doesn't overwrite it"
    );
    let text = std::fs::read_to_string(backup(dir)).expect("a new backup");
    let new: serde_json::Value = serde_json::from_str(&text).expect("a whole JSON document");
    assert_eq!(new["notes"].as_array().expect("the notes").len(), 1);
}

#[tokio::test]
async fn a_backup_that_fails_halfway_restores_nothing() {
    let harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    // The thread's context is fine, but the note lacks columns that can't be empty.
    let broken = serde_json::json!({
        "note_threads": [{
            "thread_id": thread.id.as_str(),
            "subject": "Renewal",
            "participants": "[]",
            "first_date": null,
            "last_date": null,
            "messages": 1,
            "next_number": 2,
        }],
        "notes": [{ "id": "a", "thread_id": thread.id.as_str() }],
        "note_versions": [],
        "note_files": [],
        "note_drafts": [],
    })
    .to_string();
    let (harness, store) = reopen_with_backup(harness, &broken).await;
    assert_eq!(
        store.thread_notes(&thread.id).await.expect("reads"),
        exo_store::ThreadNotes {
            context: None,
            notes: Vec::new(),
            deleted: Vec::new(),
            drafts: Vec::new(),
        },
        "not even the thread's context is restored"
    );
    assert_eq!(
        set_aside(harness.dir.path()),
        [broken],
        "the backup is set aside"
    );
}
