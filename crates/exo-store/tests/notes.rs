//! Thread notes: saving, numbering, versions, deleting and restoring, unsaved notes, and what
//! keeps them: the thread's deletion in Gmail, sign-out and the standing backup.

mod support;

use std::path::{Path, PathBuf};

use exo_store::{
    ListId, NoteChange, NoteDraft, NoteFile, Options, SaveNote, Search, Store, ThreadDetail,
    ThreadId,
};
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
    store.backup_notes().await.expect("backs up");
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

/// The backups in `dir` written beside a standing backup that couldn't be overwritten, oldest
/// first.
fn session_backups(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("lists")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".notes-backup.session-"))
        })
        .collect();
    found.sort();
    found
}

/// The texts of the notes on `thread`, in order.
async fn note_texts(store: &Store, thread: &ThreadId) -> Vec<String> {
    let mut texts: Vec<String> = store
        .thread_notes(thread)
        .await
        .expect("reads")
        .notes
        .into_iter()
        .map(|note| note.text)
        .collect();
    texts.sort_unstable();
    texts
}

#[tokio::test]
async fn notes_written_while_the_backup_waits_are_kept_through_a_sign_out() {
    let harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    let Harness {
        server,
        dir,
        store,
        gmail: _gmail,
    } = harness;
    let dir = dir.path();
    let marker = dir.join(format!("{ACCOUNT}.notes-backup.pending"));
    // The backup can't be read just now (a folder stands where the file should be), so opening
    // leaves it for a later open and mustn't write over it.
    drop(store);
    Store::delete(dir, ACCOUNT).expect("signs out");
    std::fs::create_dir(backup(dir)).expect("stands in the backup's place");
    let store = support::open(&server, dir).await;
    saved(
        &store,
        save(&thread.id, None, "a", None, "Written meanwhile"),
        at(0),
    )
    .await;
    store.backup_notes().await.expect("backs up");
    assert!(backup(dir).is_dir(), "what waits isn't written over");
    assert!(marker.exists(), "the next open is asked to restore it");
    let sessions = session_backups(dir);
    assert_eq!(
        sessions.len(),
        1,
        "the session's notes are backed up beside it"
    );
    let text = std::fs::read_to_string(&sessions[0]).expect("reads");
    let session: serde_json::Value = serde_json::from_str(&text).expect("a whole JSON document");
    assert_eq!(backed_up_ids(&session, "notes", "id"), ["a"]);

    // Signed out and in again with the backup still unreadable: the session's notes come back,
    // and what is written next goes beside it again, not over it.
    drop(store);
    Store::delete(dir, ACCOUNT).expect("signs out");
    let store = support::open(&server, dir).await;
    assert_eq!(note_texts(&store, &thread.id).await, ["Written meanwhile"]);
    saved(
        &store,
        save(&thread.id, None, "b", None, "And another"),
        at(1),
    )
    .await;
    store.backup_notes().await.expect("backs up");
    assert!(backup(dir).is_dir());
    let sessions = session_backups(dir);
    assert_eq!(
        sessions.len(),
        1,
        "the restored one is gone, this session's is there"
    );
    let text = std::fs::read_to_string(&sessions[0]).expect("reads");
    let session: serde_json::Value = serde_json::from_str(&text).expect("a whole JSON document");
    assert_eq!(backed_up_ids(&session, "notes", "id"), ["a", "b"]);

    // Signed out and in once the backup can be read (there is none, here): everything is back,
    // nothing waits, and backups are the standing file's again.
    drop(store);
    Store::delete(dir, ACCOUNT).expect("signs out");
    std::fs::remove_dir(backup(dir)).expect("clears the way");
    let store = support::open(&server, dir).await;
    assert_eq!(
        note_texts(&store, &thread.id).await,
        ["And another", "Written meanwhile"]
    );
    assert!(!marker.exists(), "restored, so no longer waiting");
    assert_eq!(session_backups(dir), Vec::<PathBuf>::new());
    store.backup_notes().await.expect("backs up");
    assert_eq!(backed_up_ids(&backed_up(dir), "notes", "id"), ["a", "b"]);
}

#[tokio::test]
async fn a_session_backup_that_wont_restore_is_set_aside_and_others_are_left_alone() {
    let mut harness = Harness::synced().await;
    let dir = harness.dir.path().to_owned();
    let beside = |name: &str| dir.join(format!("{ACCOUNT}.notes-backup.{name}"));
    // One that isn't a backup, one already set aside, and one that isn't a session's at all.
    std::fs::write(beside("session-5.json"), "not a backup {").expect("writes");
    std::fs::write(beside("session-4.unrestored-9.json"), "kept").expect("writes");
    std::fs::write(beside("session-mine.json"), "mine").expect("writes");
    harness.reopen().await;

    assert!(!beside("session-5.json").exists(), "it isn't tried again");
    let name = harness
        .store
        .take_notes_backup_set_aside()
        .expect("the store says what it was kept as");
    assert!(
        name.starts_with(&format!("{ACCOUNT}.notes-backup.session-5.unrestored-")),
        "{name}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(&name)).expect("kept"),
        "not a backup {"
    );
    assert_eq!(
        std::fs::read_to_string(beside("session-4.unrestored-9.json")).expect("kept"),
        "kept"
    );
    assert_eq!(
        std::fs::read_to_string(beside("session-mine.json")).expect("kept"),
        "mine"
    );
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
    let name = store
        .take_notes_backup_set_aside()
        .expect("the store says what it was kept as");
    assert!(
        name.starts_with(&format!("{ACCOUNT}.notes-backup.unrestored-"))
            && dir.join(&name).exists(),
        "{name}"
    );
    assert_eq!(store.take_notes_backup_set_aside(), None, "said once");

    saved(&store, save(&thread.id, None, "a", None, "Fresh"), at(0)).await;
    assert_eq!(
        set_aside(dir),
        ["not a backup {"],
        "a save doesn't overwrite it"
    );
    store.backup_notes().await.expect("backs up");
    assert_eq!(
        set_aside(dir),
        ["not a backup {"],
        "nor does the backup after it"
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

#[tokio::test]
async fn every_noted_thread_comes_with_its_notes_and_versions() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let message = &thread.messages[0].id;
    let kept = saved(store, save(&thread.id, None, "a", None, "Renewal"), at(0)).await;
    let gone = saved(
        store,
        save(&thread.id, Some(message), "b", None, "Old"),
        at(1),
    )
    .await;
    saved(
        store,
        save(&thread.id, None, "a", Some("a"), "Renewal, signed"),
        at(2),
    )
    .await;
    store.delete_note(&gone.id, at(3)).await.expect("deletes");

    let all = store.all_notes().await.expect("reads");
    let [(context, notes, versions)] = all.as_slice() else {
        panic!("one noted thread: {all:?}")
    };
    assert_eq!(context.subject, thread.subject);
    assert_eq!(
        notes
            .iter()
            .map(|note| note.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "deleted notes too, oldest first"
    );
    assert!(notes[1].deleted_at.is_some());
    assert_eq!(
        versions
            .iter()
            .map(|(note, version)| (note.as_str(), version.version, version.change))
            .collect::<Vec<_>>(),
        [
            ("a", 1, NoteChange::Created),
            ("a", 2, NoteChange::Edited),
            ("b", 1, NoteChange::Created),
            ("b", 2, NoteChange::Deleted),
        ]
    );
    assert_eq!(store.note(&kept.id).await.expect("reads").version, 2);
    assert_eq!(
        store
            .noted_thread(&thread.id)
            .await
            .expect("reads")
            .as_ref(),
        Some(&all[0])
    );
    assert_eq!(
        store
            .noted_thread(&ThreadId::new("no-notes"))
            .await
            .expect("reads"),
        None
    );
    let missing = store
        .missing_messages(&[message.clone(), "not-a-message".to_owned()])
        .await
        .expect("reads");
    assert_eq!(missing.into_iter().collect::<Vec<_>>(), ["not-a-message"]);
    // More ids than one statement takes.
    let mut many: Vec<String> = (0..1200).map(|i| format!("gone-{i:04}")).collect();
    many.push(message.clone());
    let missing = store.missing_messages(&many).await.expect("reads");
    assert_eq!(missing.len(), 1200);
    assert!(!missing.contains(message));
}

/// The notes' backup in `dir`, read.
fn backed_up(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(backup(dir)).expect("a backup");
    serde_json::from_str(&text).expect("a whole JSON document")
}

/// The ids of `table`'s rows in `backup`, by `key`, sorted.
fn backed_up_ids(backup: &serde_json::Value, table: &str, key: &str) -> Vec<String> {
    let mut ids: Vec<String> = backup[table]
        .as_array()
        .unwrap_or_else(|| panic!("{table} is backed up"))
        .iter()
        .map(|row| row[key].as_str().expect("an id").to_owned())
        .collect();
    ids.sort_unstable();
    ids
}

/// An unsaved note of `thread`'s, from editor `id`.
fn unsaved(thread: &ThreadId, id: &str, text: &str) -> NoteDraft {
    NoteDraft {
        id: id.into(),
        thread_id: thread.clone(),
        message_id: None,
        note_id: None,
        html: format!("<p>{text}</p>"),
        text: text.into(),
        files: Vec::new(),
        updated_at: at(0),
    }
}

#[tokio::test]
async fn a_save_stands_when_its_backup_cant_be_written() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    let dir = harness.dir.path();
    // A folder where the backup goes, which no file can be renamed over.
    std::fs::create_dir_all(backup(dir).join("in the way")).expect("a folder");

    let note = saved(store, save(&thread.id, None, "a", None, "Renewal"), at(0)).await;
    assert_eq!(note.text, "Renewal", "the save is the save's own");
    store.delete_note("a", at(1)).await.expect("deletes");
    let restored = store
        .restore_note("a", None, at(2))
        .await
        .expect("restores");
    assert_eq!(restored.deleted_at, None);
    store
        .backup_notes()
        .await
        .expect_err("the backup can't be written");
    assert!(store.notes_backup_due(), "it stays due");
    assert!(
        !PathBuf::from(format!("{}.tmp", backup(dir).display())).exists(),
        "no temporary file is left"
    );

    // Once it can be written, the next backup holds everything.
    std::fs::remove_dir_all(backup(dir)).expect("out of the way");
    store.backup_notes().await.expect("backs up");
    assert!(!store.notes_backup_due());
    let backup = backed_up(dir);
    assert_eq!(backed_up_ids(&backup, "notes", "id"), ["a"]);
    assert_eq!(
        backup["note_versions"].as_array().expect("versions").len(),
        3
    );
}

#[tokio::test]
async fn note_changes_are_backed_up_on_their_own_a_burst_in_one_write() {
    let harness = Harness::start().await;
    harness.sync_all().await;
    let thread = conversation(&harness).await;
    let dir = harness.dir.path();
    let store = Store::open_with(
        dir,
        ACCOUNT,
        harness.server.endpoints(),
        gmail::StaticToken(gmail_mock::DEV_ACCESS_TOKEN.into()),
        Options {
            notes_backup_pace: std::time::Duration::from_millis(300),
            ..support::options()
        },
    )
    .await
    .expect("opens");
    saved(&store, save(&thread.id, None, "a", None, "First"), at(0)).await;
    saved(&store, save(&thread.id, None, "b", None, "Second"), at(1)).await;
    store
        .save_note_draft(unsaved(&thread.id, "c", "Typing"))
        .await
        .expect("keeps the draft");
    assert!(
        !backup(dir).exists(),
        "nothing is written until the burst is over"
    );
    assert!(store.notes_backup_due());
    for _ in 0..100 {
        if backup(dir).exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let first = backed_up(dir);
    assert_eq!(backed_up_ids(&first, "notes", "id"), ["a", "b"]);
    assert_eq!(backed_up_ids(&first, "note_drafts", "id"), ["c"]);

    // A draft dropped goes from the backup too, with no save, delete or restore to carry it.
    store.discard_note_draft("c").await.expect("discards");
    assert!(store.notes_backup_due());
    for _ in 0..100 {
        if backed_up_ids(&backed_up(dir), "note_drafts", "id").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        backed_up_ids(&backed_up(dir), "note_drafts", "id"),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn an_unchanged_save_takes_its_draft_out_of_the_backup() {
    let harness = Harness::synced().await;
    let store = &harness.store;
    let thread = conversation(&harness).await;
    saved(store, save(&thread.id, None, "a", None, "Renewal"), at(0)).await;
    store
        .save_note_draft(NoteDraft {
            note_id: Some("a".into()),
            ..unsaved(&thread.id, "a", "Renewal")
        })
        .await
        .expect("keeps the draft");
    store.backup_notes().await.expect("backs up");
    assert_eq!(
        backed_up_ids(&backed_up(harness.dir.path()), "note_drafts", "id"),
        ["a"]
    );
    // Closed unchanged: no version, and the draft goes.
    store
        .save_note(save(&thread.id, None, "a", Some("a"), "Renewal"), at(1))
        .await
        .expect("saves");
    assert!(store.notes_backup_due(), "the backup has a draft to lose");
    store.backup_notes().await.expect("backs up");
    assert_eq!(
        backed_up_ids(&backed_up(harness.dir.path()), "note_drafts", "id"),
        Vec::<String>::new()
    );
}

/// A backup of one thread note `id` on `thread`, as an Exo before the `gone` column wrote it,
/// with an unsaved note from editor `draft`.
fn old_backup(thread: &ThreadId, id: &str, draft: &str) -> String {
    let time = at(0).to_string();
    serde_json::json!({
        "note_threads": [{
            "thread_id": thread.as_str(),
            "subject": "Renewal",
            "participants": "[]",
            "first_date": null,
            "last_date": null,
            "messages": 1,
            "next_number": 4,
        }],
        "notes": [{
            "id": id,
            "thread_id": thread.as_str(),
            "message_id": null,
            "message_from": null,
            "message_date": null,
            "number": null,
            "kind": "note",
            "html": "<p>From the backup</p>",
            "text": "From the backup",
            "files": "[]",
            "created_at": time,
            "updated_at": time,
            "deleted_at": null,
            "version": 1,
        }],
        "note_versions": [{
            "note_id": id,
            "version": 1,
            "html": "<p>From the backup</p>",
            "text": "From the backup",
            "files": "[]",
            "saved_at": time,
            "change": "created",
        }],
        "note_files": [],
        "note_drafts": [{
            "id": draft,
            "thread_id": thread.as_str(),
            "message_id": null,
            "note_id": null,
            "html": "<p>The backup's draft</p>",
            "text": "The backup's draft",
            "files": "[]",
            "updated_at": time,
        }],
    })
    .to_string()
}

#[tokio::test]
async fn a_backup_restores_over_rows_left_with_its_keys() {
    let mut harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    // No notes, but a draft kept under the key the backup's draft has.
    harness
        .store
        .save_note_draft(unsaved(&thread.id, "d", "Typed before the notes came back"))
        .await
        .expect("keeps the draft");
    let text = old_backup(&thread.id, "a", "d");
    std::fs::write(backup(harness.dir.path()), &text).expect("writes the backup");
    harness.reopen().await;
    let store = &harness.store;

    let notes = store.thread_notes(&thread.id).await.expect("reads");
    assert_eq!(
        notes
            .notes
            .iter()
            .map(|note| note.text.as_str())
            .collect::<Vec<_>>(),
        ["From the backup"],
        "the notes are back"
    );
    assert_eq!(
        notes
            .drafts
            .iter()
            .map(|draft| draft.text.as_str())
            .collect::<Vec<_>>(),
        ["The backup's draft"],
        "the backup's row takes the key"
    );
    assert_eq!(set_aside(harness.dir.path()), Vec::<String>::new());
    assert_eq!(store.take_notes_backup_set_aside(), None);
    // A backup from before the thread's `gone` column restores, not gone.
    let page = store
        .search(&mut Search::new("notes:backup"), 50, false, 15)
        .await
        .expect("searches");
    assert_eq!(page.results.len(), 1);
    // Numbers go on from the backup's.
    let next = saved(
        store,
        save(
            &thread.id,
            Some(&thread.messages[0].id),
            "n",
            None,
            "Numbered",
        ),
        at(5),
    )
    .await;
    assert_eq!(next.number, Some(4));
}

#[tokio::test]
async fn a_backup_left_for_the_next_open_is_merged_with_notes_written_since() {
    let mut harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    let dir = harness.dir.path().to_owned();
    // The last open couldn't take the backup, and notes were written after.
    let message = thread.messages[0].id.clone();
    let since = saved(
        &harness.store,
        save(&thread.id, Some(&message), "s", None, "Written since"),
        at(9),
    )
    .await;
    assert_eq!(since.number, Some(1));
    let text = old_backup(&thread.id, "a", "d");
    std::fs::write(backup(&dir), &text).expect("writes the backup");
    std::fs::write(dir.join(format!("{ACCOUNT}.notes-backup.pending")), b"").expect("marks it");
    harness.reopen().await;
    let store = &harness.store;

    let notes = store.thread_notes(&thread.id).await.expect("reads");
    let mut texts: Vec<&str> = notes.notes.iter().map(|note| note.text.as_str()).collect();
    texts.sort_unstable();
    assert_eq!(texts, ["From the backup", "Written since"], "both are kept");
    assert_eq!(
        notes.context.expect("the thread's context").subject,
        thread.subject,
        "what the store had isn't replaced"
    );
    assert!(
        !dir.join(format!("{ACCOUNT}.notes-backup.pending")).exists(),
        "restored, so no longer waiting"
    );
    assert_eq!(set_aside(&dir), Vec::<String>::new());
    // The next number is past both sides' numbers.
    let next = saved(
        store,
        save(&thread.id, Some(&message), "n", None, "Numbered"),
        at(10),
    )
    .await;
    assert_eq!(next.number, Some(4));
    // And the backup written next holds them all.
    store.backup_notes().await.expect("backs up");
    assert_eq!(
        backed_up_ids(&backed_up(&dir), "notes", "id"),
        ["a", "n", "s"]
    );
}

#[tokio::test]
async fn a_noted_thread_that_isnt_stored_is_fetched_again_not_called_deleted() {
    let harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    let message = thread.messages[0].id.clone();
    saved(
        &harness.store,
        save(
            &thread.id,
            Some(&message),
            "a",
            None,
            "Ask about the quokka",
        ),
        at(0),
    )
    .await;
    harness.store.backup_notes().await.expect("backs up");
    // Signing out and in again brings the notes back from their backup, with no mail stored.
    let Harness {
        server,
        dir,
        store,
        gmail,
    } = harness;
    drop(store);
    Store::delete(dir.path(), ACCOUNT).expect("signs out");
    let store = support::open(&server, dir.path()).await;
    assert!(store.thread(&thread.id).await.expect("reads").is_none());
    let found = |query: &'static str| {
        let store = store.clone();
        async move {
            store
                .search(&mut Search::new(query), 50, false, 15)
                .await
                .expect("searches")
                .results
        }
    };
    let results = found("notes:quokka").await;
    assert_eq!(results.len(), 1, "found by its note");
    assert!(!results[0].deleted, "not stored isn't deleted");
    assert_eq!(results[0].thread.subject, thread.subject);
    assert_eq!(
        store
            .missing_messages(std::slice::from_ref(&message))
            .await
            .expect("reads")
            .len(),
        0,
        "nor is its message"
    );

    // Sync fetches it whole again, wherever the lists' backfill stops.
    let harness = Harness {
        server,
        dir,
        store: store.clone(),
        gmail,
    };
    harness.sync_all().await;
    assert!(
        store.thread(&thread.id).await.expect("reads").is_some(),
        "stored again"
    );
    let results = found("notes:quokka").await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].deleted);
    assert!(results[0].thread.messages > 0, "as a thread, with its mail");

    // One Gmail no longer has is recorded as gone, and only then called deleted.
    harness
        .gmail
        .gmail()
        .delete_thread(thread.id.as_str())
        .await
        .expect("deletes");
    harness.sync_all().await;
    let results = found("notes:quokka").await;
    assert_eq!(results.len(), 1);
    assert!(results[0].deleted, "Gmail said it's gone");
    assert_eq!(
        store
            .missing_messages(std::slice::from_ref(&message))
            .await
            .expect("reads")
            .into_iter()
            .collect::<Vec<_>>(),
        [message],
        "and so is its message"
    );
    // The flag is backed up, and comes back with the notes.
    store.backup_notes().await.expect("backs up");
    assert_eq!(
        backed_up(harness.dir.path())["note_threads"][0]["gone"],
        serde_json::json!(1)
    );
}

#[tokio::test]
async fn a_note_being_written_at_sign_out_comes_back_though_none_was_saved() {
    let harness = Harness::synced().await;
    let thread = conversation(&harness).await;
    harness
        .store
        .save_note_draft(unsaved(&thread.id, "d", "Typed, never saved"))
        .await
        .expect("keeps the draft");
    harness.store.backup_notes().await.expect("backs up");
    let Harness {
        server, dir, store, ..
    } = harness;
    drop(store);
    Store::delete(dir.path(), ACCOUNT).expect("signs out");
    let store = support::open(&server, dir.path()).await;
    let notes = store.thread_notes(&thread.id).await.expect("reads");
    assert_eq!(
        notes
            .drafts
            .iter()
            .map(|draft| draft.text.as_str())
            .collect::<Vec<_>>(),
        ["Typed, never saved"]
    );
    assert_eq!(set_aside(dir.path()), Vec::<String>::new());

    // A store that still has its drafts takes none from a backup without notes: a draft
    // dropped since the backup was written stays dropped.
    store.discard_note_draft("d").await.expect("discards");
    store
        .save_note_draft(unsaved(&thread.id, "e", "The newer one"))
        .await
        .expect("keeps the draft");
    drop(store);
    let stale = serde_json::json!({
        "note_threads": [], "notes": [], "note_versions": [], "note_files": [],
        "note_drafts": [{
            "id": "d",
            "thread_id": thread.id.as_str(),
            "message_id": null,
            "note_id": null,
            "html": "<p>Typed, never saved</p>",
            "text": "Typed, never saved",
            "files": "[]",
            "updated_at": at(0).to_string(),
        }],
    });
    std::fs::write(backup(dir.path()), stale.to_string()).expect("writes the backup");
    let store = support::open(&server, dir.path()).await;
    let notes = store.thread_notes(&thread.id).await.expect("reads");
    assert_eq!(
        notes
            .drafts
            .iter()
            .map(|draft| draft.id.as_str())
            .collect::<Vec<_>>(),
        ["e"]
    );
}
