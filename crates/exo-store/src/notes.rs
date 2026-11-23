//! Thread notes: the user's own notes on a conversation or one of its messages, kept on this
//! device only. They never reach Gmail.
//!
//! Every save, delete and restore is kept as a version. After each, and after an editor's draft
//! is kept or dropped, the notes' tables are written to a standing backup beside the store
//! (`{account}.notes-backup.json`), which signing out leaves alone. The backup is written off the
//! change that asked for it, [`BACKUP_PACE`] after it at the latest, so a burst of changes is
//! one write and a write that fails never fails the change. A store opened with no notes and a
//! backup that has some takes them from it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use toasty::stmt::Value;
use toasty::{Db, Executor, Json};

use crate::db::{
    MessageRow, NoteDraftRow, NoteFileRow, NoteRow, NoteThreadRow, NoteVersionRow, query_i64,
    strings,
};
use crate::error::{Error, Result};
use crate::ids::ThreadId;
use crate::query::NoteTerm;
use crate::search::{Highlighted, NoteHit, index_note, unindex_note};
use crate::store::{Store, load_view};

/// The tables the backup holds, in the order they're restored.
const TABLES: [&str; 5] = [
    "note_threads",
    "notes",
    "note_versions",
    "note_files",
    "note_drafts",
];

/// How many message ids [`Store::missing_messages`] looks up in one statement.
const MESSAGE_CHUNK: usize = 500;

/// How long after a note change its backup is written, at the latest. Changes within it share
/// one write.
pub(crate) const BACKUP_PACE: Duration = Duration::from_secs(2);

/// How many noted threads that aren't stored one round of sync asks Gmail for.
const NOTED_AT_ONCE: usize = 25;

/// The notes' standing backup: where it is, and what's left to write of it.
pub(crate) struct Backup {
    /// The backup's file.
    path: PathBuf,
    /// How long after a change it's written.
    pace: Duration,
    /// Whether the notes changed since the backup last held them.
    dirty: AtomicBool,
    /// Whether a write is waiting out the pace.
    scheduled: AtomicBool,
    /// Whether the file holds notes that couldn't be restored and must not be overwritten, so
    /// nothing is written this session.
    disabled: AtomicBool,
    /// One backup written at a time, so a later one is never overwritten by an earlier one.
    writing: tokio::sync::Mutex<()>,
    /// The file a backup that couldn't be restored was kept as, until the app has said so.
    set_aside: std::sync::Mutex<Option<String>>,
}

impl Backup {
    /// The backup at `path`, written `pace` after a change, as opening the store left it.
    pub(crate) fn new(path: PathBuf, pace: Duration, recovered: Recovered) -> Self {
        Self {
            path,
            pace,
            dirty: AtomicBool::new(false),
            scheduled: AtomicBool::new(false),
            disabled: AtomicBool::new(recovered.disabled),
            writing: tokio::sync::Mutex::new(()),
            set_aside: std::sync::Mutex::new(recovered.set_aside),
        }
    }
}

/// What opening the store did with the notes' backup.
#[derive(Debug, Default)]
pub(crate) struct Recovered {
    /// The file name a backup that couldn't be restored was kept as.
    set_aside: Option<String>,
    /// Whether the backup is left where it is, unrestored, so nothing may overwrite it.
    disabled: bool,
}

/// A name and address of one of a noted thread's participants, as the thread had them when a
/// note last changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteParticipant {
    /// Their display name, when a header gave one.
    pub name: Option<String>,
    /// Their address, as written in the header.
    pub email: String,
}

/// A file attached to a note. Its bytes are kept under its `uuid` in [`Store::note_files`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteFile {
    /// Its id, which names its bytes.
    pub uuid: String,
    /// Its file name.
    pub name: String,
    /// Its MIME type.
    pub mime_type: String,
    /// Its size in bytes.
    pub size: u64,
    /// Set for an inline image, which the note's HTML shows by `cid:`.
    pub content_id: Option<String>,
}

/// A note on a thread, or on one of its messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// A UUID v4: the id of the editor it was first saved from.
    pub id: String,
    /// The thread it's on.
    pub thread_id: ThreadId,
    /// The message it's on, or `None` for a thread note.
    pub message_id: Option<String>,
    /// That message's sender (their name, or else their address) and date, as they were when the
    /// note was last saved, so the note makes sense once the message is gone.
    pub message_from: Option<String>,
    pub message_date: Option<Timestamp>,
    /// Its number in the thread, or `None` for a thread note. Numbers are never reused.
    pub number: Option<u32>,
    /// What it is: `note` for now.
    pub kind: String,
    /// The editor's HTML.
    pub html: String,
    /// The plain text of `html`.
    pub text: String,
    /// Its files, attached and inline.
    pub files: Vec<NoteFile>,
    pub created_at: Timestamp,
    /// When its content last changed.
    pub updated_at: Timestamp,
    /// When it was deleted, or `None` while it's live.
    pub deleted_at: Option<Timestamp>,
    /// How many versions it has.
    pub version: u32,
}

impl Note {
    /// What the UI calls it: `Thread note`, or `Note 3`.
    #[must_use]
    pub fn label(&self) -> String {
        label(self.number)
    }
}

/// What the UI calls a note numbered `number`: `Thread note`, or `Note 3`.
fn label(number: Option<u32>) -> String {
    match number {
        Some(number) => format!("Note {number}"),
        None => "Thread note".to_owned(),
    }
}

/// What's typed in an open note editor and not saved yet, kept as it changes so a crash loses
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDraft {
    /// The editor's id: the note's, or a new UUID for a note not saved yet, which it gets.
    pub id: String,
    pub thread_id: ThreadId,
    /// The message it's on, or `None` for a thread note.
    pub message_id: Option<String>,
    /// The note it edits, or `None` for a new one.
    pub note_id: Option<String>,
    pub html: String,
    pub text: String,
    pub files: Vec<NoteFile>,
    pub updated_at: Timestamp,
}

/// A note as one of its versions left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteVersion {
    /// Its number, from 1.
    pub version: u32,
    pub html: String,
    pub text: String,
    pub files: Vec<NoteFile>,
    pub saved_at: Timestamp,
    /// What made it.
    pub change: NoteChange,
}

/// What made a note's version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteChange {
    /// The note's first save.
    Created,
    /// A save that changed it.
    Edited,
    /// Deleting it.
    Deleted,
    /// Undeleting it, or bringing back one of its versions.
    Restored,
}

impl NoteChange {
    /// How it's stored, and how the notes folder's history names it: `created`, `edited`,
    /// `deleted` or `restored`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Edited => "edited",
            Self::Deleted => "deleted",
            Self::Restored => "restored",
        }
    }

    /// The change stored as `stored`. Anything unknown reads as an edit.
    fn parse(stored: &str) -> Self {
        match stored {
            "created" => Self::Created,
            "deleted" => Self::Deleted,
            "restored" => Self::Restored,
            _ => Self::Edited,
        }
    }
}

/// A noted thread as it was when one of its notes last changed, so its notes make sense when its
/// mail isn't stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteThread {
    pub thread_id: ThreadId,
    pub subject: String,
    /// Everyone who sent or received its messages.
    pub participants: Vec<NoteParticipant>,
    /// The dates of its first and last messages.
    pub first_date: Option<Timestamp>,
    pub last_date: Option<Timestamp>,
    /// How many messages it had, drafts aside.
    pub messages: u32,
}

/// A thread's notes, as its view and notes pane show them. The default has none, for a view with
/// no thread open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadNotes {
    /// The thread as it was when a note last changed, or `None` if it never had one.
    pub context: Option<NoteThread>,
    /// Its live notes, oldest first.
    pub notes: Vec<Note>,
    /// Its deleted notes, oldest first.
    pub deleted: Vec<Note>,
    /// Its editors' unsaved text, oldest first, to open again.
    pub drafts: Vec<NoteDraft>,
}

/// A noted thread's context, all its notes (deleted ones too), and every version of each, as the
/// notes folder writes them. Notes come oldest first, and versions by note in that order, oldest
/// version first, each with its note's id.
pub type NotedThread = (NoteThread, Vec<Note>, Vec<(String, NoteVersion)>);

/// A note's save, as the editor makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveNote {
    /// The editor's id. A new note takes it as its own.
    pub draft_id: String,
    /// The note it edits, or `None` for a new one.
    pub note_id: Option<String>,
    pub thread_id: ThreadId,
    /// The message it's on, or `None` for a thread note.
    pub message_id: Option<String>,
    pub html: String,
    pub text: String,
    pub files: Vec<NoteFile>,
}

impl Store {
    /// `thread`'s notes, deleted ones and unsaved ones, with the thread's context.
    ///
    /// # Errors
    /// On database failures.
    pub async fn thread_notes(&self, thread: &ThreadId) -> Result<ThreadNotes> {
        let mut db = self.inner.db.clone();
        let context = NoteThreadRow::filter_by_thread_id(thread.as_str())
            .first()
            .exec(&mut db)
            .await?
            .map(|row| NoteThread {
                thread_id: ThreadId::new(row.thread_id),
                subject: row.subject,
                participants: row.participants.0,
                first_date: row.first_date,
                last_date: row.last_date,
                messages: row.messages,
            });
        let mut rows = NoteRow::filter(NoteRow::fields().thread_id().eq(thread.as_str()))
            .exec(&mut db)
            .await?;
        // Written in the same instant, `Note 1` still comes before `Note 2`.
        rows.sort_by(|a, b| (a.created_at, a.number, &a.id).cmp(&(b.created_at, b.number, &b.id)));
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        let files = files_of(&mut db, &ids).await?;
        let (notes, deleted): (Vec<Note>, Vec<Note>) = rows
            .into_iter()
            .map(|row| note(row, &files))
            .partition(|note| note.deleted_at.is_none());
        let mut drafts =
            NoteDraftRow::filter(NoteDraftRow::fields().thread_id().eq(thread.as_str()))
                .exec(&mut db)
                .await?;
        drafts.sort_by(|a, b| (a.updated_at, &a.id).cmp(&(b.updated_at, &b.id)));
        Ok(ThreadNotes {
            context,
            notes,
            deleted,
            drafts: drafts
                .into_iter()
                .map(|row| NoteDraft {
                    id: row.id,
                    thread_id: ThreadId::new(row.thread_id),
                    message_id: row.message_id,
                    note_id: row.note_id,
                    html: row.html,
                    text: row.text,
                    files: row.files.0,
                    updated_at: row.updated_at,
                })
                .collect(),
        })
    }

    /// Saves a note from its editor at `now`, and drops the editor's draft. A new note with no
    /// text and no files isn't saved, and `None` comes back. A save that changes neither the
    /// HTML nor the files of a saved note writes no version. A new note on a message takes the
    /// thread's next number. The thread's context is saved with it as the thread is stored now
    /// (or left as it was when it isn't). The notes' backup is written after, on its own: a
    /// backup that can't be written is tried again, and the save stands.
    ///
    /// The file's bytes must be in [`note_files`](Self::note_files) already.
    ///
    /// # Errors
    /// On database failures.
    pub async fn save_note(&self, save: SaveNote, now: Timestamp) -> Result<Option<Note>> {
        let id = save
            .note_id
            .clone()
            .unwrap_or_else(|| save.draft_id.clone());
        let uuids: Vec<String> = save.files.iter().map(|file| file.uuid.clone()).collect();
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let stored = NoteRow::filter_by_id(&id)
            .first()
            .exec(write.exec())
            .await?;
        let unchanged = match &stored {
            None => save.text.trim().is_empty() && save.files.is_empty(),
            Some(row) => row.html == save.html && row.files.0 == uuids,
        };
        if unchanged {
            NoteDraftRow::filter_by_id(&save.draft_id)
                .delete()
                .exec(write.exec())
                .await?;
            write.publish().await?;
            // The draft is gone from the tables, so it goes from the backup.
            self.notes_changed();
            return match stored {
                None => Ok(None),
                Some(_) => self.note(&id).await.map(Some),
            };
        }
        let number = save_context(write.exec(), &save, stored.is_none()).await?;
        let (message_from, message_date) = match &save.message_id {
            Some(message) => message_context(write.exec(), message).await?,
            None => (None, None),
        };
        let version = match stored {
            Some(row) => {
                let version = row.version + 1;
                NoteRow::update_by_id(&id)
                    .html(&save.html)
                    .text(&save.text)
                    .files(Json(uuids.clone()))
                    .updated_at(now)
                    .version(version)
                    .message_from(message_from.or(row.message_from))
                    .message_date(message_date.or(row.message_date))
                    .exec(write.exec())
                    .await?;
                version
            }
            None => {
                NoteRow::create()
                    .id(&id)
                    .thread_id(save.thread_id.as_str())
                    .message_id(save.message_id.clone())
                    .message_from(message_from)
                    .message_date(message_date)
                    .number(number)
                    .kind("note")
                    .html(&save.html)
                    .text(&save.text)
                    .files(Json(uuids.clone()))
                    .created_at(now)
                    .updated_at(now)
                    .deleted_at(None::<Timestamp>)
                    .version(1_u32)
                    .exec(write.exec())
                    .await?;
                1
            }
        };
        let change = if version == 1 {
            NoteChange::Created
        } else {
            NoteChange::Edited
        };
        add_version(
            write.exec(),
            &id,
            version,
            &save.html,
            &save.text,
            uuids,
            now,
            change,
        )
        .await?;
        for file in &save.files {
            if NoteFileRow::filter_by_uuid(&file.uuid)
                .first()
                .exec(write.exec())
                .await?
                .is_none()
            {
                NoteFileRow::create()
                    .uuid(&file.uuid)
                    .note_id(&id)
                    .name(&file.name)
                    .mime_type(&file.mime_type)
                    .size(file.size)
                    .content_id(file.content_id.clone())
                    .exec(write.exec())
                    .await?;
            }
        }
        NoteDraftRow::filter_by_id(&save.draft_id)
            .delete()
            .exec(write.exec())
            .await?;
        index_note(write.exec(), &id).await?;
        write.publish().await?;
        self.notes_changed();
        self.note(&id).await.map(Some)
    }

    /// Deletes note `id` at `now`. It stays in the thread's deleted notes and its history, and
    /// [`restore_note`](Self::restore_note) brings it back. Deleting a deleted note does nothing.
    ///
    /// # Errors
    /// [`Error::UnknownNote`] for a note that isn't stored, or on database failures.
    pub async fn delete_note(&self, id: &str, now: Timestamp) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let row = NoteRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
            .ok_or_else(|| Error::UnknownNote(id.to_owned()))?;
        if row.deleted_at.is_some() {
            return write.commit().await;
        }
        let version = row.version + 1;
        NoteRow::update_by_id(id)
            .deleted_at(Some(now))
            .version(version)
            .exec(write.exec())
            .await?;
        add_version(
            write.exec(),
            id,
            version,
            &row.html,
            &row.text,
            row.files.0,
            now,
            NoteChange::Deleted,
        )
        .await?;
        unindex_note(write.exec(), id).await?;
        write.publish().await?;
        self.notes_changed();
        Ok(())
    }

    /// Brings note `id` back at `now`: undeletes it, or with `version`, saves that version of it
    /// as its newest (and undeletes it). Undeleting a live note does nothing.
    ///
    /// # Errors
    /// [`Error::UnknownNote`] for a note or version that isn't stored, or on database failures.
    pub async fn restore_note(
        &self,
        id: &str,
        version: Option<u32>,
        now: Timestamp,
    ) -> Result<Note> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let row = NoteRow::filter_by_id(id)
            .first()
            .exec(write.exec())
            .await?
            .ok_or_else(|| Error::UnknownNote(id.to_owned()))?;
        let (html, text, files, updated_at) = match version {
            None if row.deleted_at.is_none() => {
                write.commit().await?;
                return self.note(id).await;
            }
            None => (row.html, row.text, row.files.0, row.updated_at),
            Some(version) => {
                let fields = NoteVersionRow::fields();
                let old = NoteVersionRow::filter(
                    fields.note_id().eq(id).and(fields.version().eq(version)),
                )
                .first()
                .exec(write.exec())
                .await?
                .ok_or_else(|| Error::UnknownNote(format!("{id} version {version}")))?;
                (old.html, old.text, old.files.0, now)
            }
        };
        let version = row.version + 1;
        NoteRow::update_by_id(id)
            .html(&html)
            .text(&text)
            .files(Json(files.clone()))
            .updated_at(updated_at)
            .deleted_at(None::<Timestamp>)
            .version(version)
            .exec(write.exec())
            .await?;
        add_version(
            write.exec(),
            id,
            version,
            &html,
            &text,
            files,
            now,
            NoteChange::Restored,
        )
        .await?;
        index_note(write.exec(), id).await?;
        write.publish().await?;
        self.notes_changed();
        self.note(id).await
    }

    /// Note `id`'s versions, newest first.
    ///
    /// # Errors
    /// On database failures.
    pub async fn note_versions(&self, id: &str) -> Result<Vec<NoteVersion>> {
        let mut db = self.inner.db.clone();
        let rows = NoteVersionRow::filter(NoteVersionRow::fields().note_id().eq(id))
            .order_by(NoteVersionRow::fields().version().desc())
            .exec(&mut db)
            .await?;
        let files = files_of(&mut db, &[id]).await?;
        Ok(rows
            .into_iter()
            .map(|row| NoteVersion {
                version: row.version,
                files: resolve(&row.files.0, &files),
                html: row.html,
                text: row.text,
                saved_at: row.saved_at,
                change: NoteChange::parse(&row.change),
            })
            .collect())
    }

    /// Keeps what's typed in an open note editor, replacing what was kept for it before.
    ///
    /// # Errors
    /// On database failures.
    pub async fn save_note_draft(&self, draft: NoteDraft) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        NoteDraftRow::filter_by_id(&draft.id)
            .delete()
            .exec(write.exec())
            .await?;
        NoteDraftRow::create()
            .id(&draft.id)
            .thread_id(draft.thread_id.as_str())
            .message_id(draft.message_id)
            .note_id(draft.note_id)
            .html(draft.html)
            .text(draft.text)
            .files(Json(draft.files))
            .updated_at(draft.updated_at)
            .exec(write.exec())
            .await?;
        write.commit().await?;
        self.notes_changed();
        Ok(())
    }

    /// Drops what was kept of note editor `id`'s text.
    ///
    /// # Errors
    /// On database failures.
    pub async fn discard_note_draft(&self, id: &str) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        NoteDraftRow::filter_by_id(id)
            .delete()
            .exec(write.exec())
            .await?;
        write.publish().await?;
        self.notes_changed();
        Ok(())
    }

    /// The folder notes' files' bytes are kept in, each named by its `uuid`. The app writes them.
    /// Signing out ([`Store::delete`]) leaves it.
    #[must_use]
    pub fn note_files(&self) -> &Path {
        &self.inner.notes
    }

    /// Writes the notes' tables to the standing backup beside the store now, whole: a temporary
    /// file, synced to disk and renamed over it, off the runtime's threads. Backups are written
    /// one at a time, each reading the tables as they are then, so the last one written holds the
    /// latest notes. Note changes ask for it on their own, [`BACKUP_PACE`] on; closing the store
    /// (quitting, signing out) calls it so nothing waits unwritten.
    ///
    /// Nothing is written while the file holds a backup that couldn't be restored.
    ///
    /// # Errors
    /// On database failures, or when the file can't be written. The backup stays due, and is
    /// tried again after the next note change and the next open.
    pub async fn backup_notes(&self) -> Result<()> {
        let backup = &self.inner.backup;
        if backup.disabled.load(Ordering::SeqCst) {
            return Ok(());
        }
        let _turn = backup.writing.lock().await;
        // What changes from here on is the next backup's.
        backup.dirty.store(false, Ordering::SeqCst);
        let written = self.write_backup().await;
        if written.is_err() {
            backup.dirty.store(true, Ordering::SeqCst);
        }
        written
    }

    /// Whether the notes changed since the backup last held them.
    #[must_use]
    pub fn notes_backup_due(&self) -> bool {
        let backup = &self.inner.backup;
        backup.dirty.load(Ordering::SeqCst) && !backup.disabled.load(Ordering::SeqCst)
    }

    /// The file name a notes' backup that couldn't be restored was kept as when the store
    /// opened, once: the app says so.
    #[must_use]
    pub fn take_notes_backup_set_aside(&self) -> Option<String> {
        self.inner
            .backup
            .set_aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Reads the notes' tables and writes them over the backup.
    async fn write_backup(&self) -> Result<()> {
        let mut db = self.inner.db.clone();
        let path = self.inner.backup.path.clone();
        let text = dump(&mut db).await?.ok_or_else(|| Error::Io {
            path: path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the notes' tables read as nothing",
            ),
        })?;
        let temporary = PathBuf::from(format!("{}.tmp", path.display()));
        let write = {
            let (path, temporary) = (path.clone(), temporary.clone());
            move || {
                use std::io::Write as _;
                let io = |path: &Path| {
                    let path = path.to_owned();
                    move |source| Error::Io { path, source }
                };
                let mut file = std::fs::File::create(&temporary).map_err(io(&temporary))?;
                file.write_all(text.as_bytes()).map_err(io(&temporary))?;
                // On disk before it takes the old backup's place.
                file.sync_all().map_err(io(&temporary))?;
                drop(file);
                std::fs::rename(&temporary, &path).map_err(io(&path))
            }
        };
        let written = tokio::task::spawn_blocking(write)
            .await
            .unwrap_or_else(|error| {
                Err(Error::Io {
                    path: path.clone(),
                    source: std::io::Error::other(error),
                })
            });
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written
    }

    /// The notes' tables changed: their backup is due, and written [`BACKUP_PACE`] from now
    /// unless a write is waiting already.
    pub(crate) fn notes_changed(&self) {
        let backup = &self.inner.backup;
        backup.dirty.store(true, Ordering::SeqCst);
        if backup.disabled.load(Ordering::SeqCst) || backup.scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            backup.scheduled.store(false, Ordering::SeqCst);
            return;
        };
        // The wait doesn't keep the store open.
        let (inner, pace) = (Arc::downgrade(&self.inner), backup.pace);
        runtime.spawn(async move {
            tokio::time::sleep(pace).await;
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let store = Store { inner };
            let backup = &store.inner.backup;
            backup.scheduled.store(false, Ordering::SeqCst);
            if backup.dirty.load(Ordering::SeqCst)
                && let Err(error) = store.backup_notes().await
            {
                log::error!("writing the notes' backup, to try again: {error}");
            }
        });
    }

    /// Asks Gmail for the threads with live notes that the store has none of (after Clear Cache,
    /// or signing in again with the notes brought back), [`NOTED_AT_ONCE`] a round, each once a
    /// session. One Gmail has is stored whole, as a noted thread stays; one it doesn't is
    /// recorded as gone, which is when its notes say `Email deleted`. Returns how many it asked
    /// for.
    ///
    /// # Errors
    /// [`Error::Offline`] and the like when Gmail can't be reached. What was fetched before stays.
    pub async fn fetch_noted_threads(&self) -> Result<usize> {
        let mut db = self.inner.db.clone();
        let unstored: Vec<String> = strings(
            toasty::sql::query("SELECT thread_id FROM note_threads WHERE gone = 0 AND thread_id IN (SELECT thread_id FROM notes WHERE deleted_at IS NULL) AND thread_id NOT IN (SELECT thread_id FROM messages) ORDER BY thread_id")
                .exec(&mut db)
                .await?,
        )
        .collect();
        let wanted: Vec<String> = {
            let asked = self
                .inner
                .noted_asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            unstored
                .into_iter()
                .filter(|thread| !asked.contains(thread))
                .take(NOTED_AT_ONCE)
                .collect()
        };
        for thread in &wanted {
            self.load_thread_in(&ThreadId::new(thread.clone()), crate::quota::Lane::Sync)
                .await?;
            self.inner
                .noted_asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(thread.clone());
        }
        Ok(wanted.len())
    }

    /// Records whether Gmail has said noted thread `thread` is `gone`, and tells the UI when
    /// that changed. A thread without notes has nothing to record.
    pub(crate) async fn set_noted_thread_gone(&self, thread: &str, gone: bool) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let changed = toasty::sql::statement(
            "UPDATE note_threads SET gone = ?1 WHERE thread_id = ?2 AND gone <> ?1",
        )
        .bind(i64::from(gone))
        .bind(thread)
        .exec(write.exec())
        .await?;
        if changed == 0 {
            return write.commit().await;
        }
        write.publish().await?;
        self.notes_changed();
        Ok(())
    }

    /// Every noted thread, as [`noted_thread`](Self::noted_thread) has each, by thread id.
    ///
    /// # Errors
    /// On database failures.
    pub async fn all_notes(&self) -> Result<Vec<NotedThread>> {
        let mut db = self.inner.db.clone();
        let threads: Vec<String> = strings(
            toasty::sql::query("SELECT DISTINCT thread_id FROM notes ORDER BY thread_id")
                .exec(&mut db)
                .await?,
        )
        .collect();
        let mut all = Vec::with_capacity(threads.len());
        for thread in threads {
            if let Some(noted) = self.noted_thread(&ThreadId::new(thread)).await? {
                all.push(noted);
            }
        }
        Ok(all)
    }

    /// `thread`'s context, all its notes, deleted ones too, and every version of each, or `None`
    /// when it has no notes. A thread whose context was never saved gets an empty one.
    ///
    /// # Errors
    /// On database failures.
    pub async fn noted_thread(&self, thread: &ThreadId) -> Result<Option<NotedThread>> {
        let ThreadNotes {
            context,
            notes,
            deleted,
            ..
        } = self.thread_notes(thread).await?;
        if notes.is_empty() && deleted.is_empty() {
            return Ok(None);
        }
        let context = context.unwrap_or_else(|| NoteThread {
            thread_id: thread.clone(),
            subject: String::new(),
            participants: Vec::new(),
            first_date: None,
            last_date: None,
            messages: 0,
        });
        let mut all: Vec<Note> = notes.into_iter().chain(deleted).collect();
        all.sort_by(|a, b| (a.created_at, a.number, &a.id).cmp(&(b.created_at, b.number, &b.id)));
        let mut versions = Vec::new();
        for note in &all {
            let mut of_note = self.note_versions(&note.id).await?;
            of_note.reverse();
            versions.extend(
                of_note
                    .into_iter()
                    .map(|version| (note.id.clone(), version)),
            );
        }
        Ok(Some((context, all, versions)))
    }

    /// Which of the messages `ids` are gone: the store has their thread and not them, or Gmail
    /// has said their thread is gone. A noted message whose whole thread merely isn't stored
    /// (after Clear Cache, until it's fetched again) isn't among them.
    ///
    /// # Errors
    /// On database failures.
    pub async fn missing_messages(&self, ids: &[String]) -> Result<BTreeSet<String>> {
        if ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut db = self.inner.db.clone();
        let mut stored = BTreeSet::new();
        // In chunks, as SQLite takes only so many values in one statement.
        for chunk in ids.chunks(MESSAGE_CHUNK) {
            let list: Vec<&str> = chunk.iter().map(String::as_str).collect();
            stored.extend(
                MessageRow::filter(MessageRow::fields().id().in_list(list.as_slice()))
                    .exec(&mut db)
                    .await?
                    .into_iter()
                    .map(|row| row.id),
            );
        }
        // Those whose thread isn't stored at all, and isn't known to be gone, may well be in
        // Gmail still.
        let mut unknown = BTreeSet::new();
        for chunk in ids.chunks(MESSAGE_CHUNK) {
            let list = serde_json::to_string(chunk).expect("ids serialize");
            unknown.extend(strings(
                toasty::sql::query("SELECT DISTINCT message_id FROM notes WHERE message_id IN (SELECT value FROM json_each(?1)) AND thread_id NOT IN (SELECT thread_id FROM messages) AND thread_id NOT IN (SELECT thread_id FROM note_threads WHERE gone = 1)")
                    .bind(list)
                    .exec(&mut db)
                    .await?,
            ));
        }
        Ok(ids
            .iter()
            .filter(|id| !stored.contains(*id) && !unknown.contains(*id))
            .cloned()
            .collect())
    }

    /// Note `id` as stored, deleted or not.
    ///
    /// # Errors
    /// [`Error::UnknownNote`] for a note that isn't stored, or on database failures.
    pub async fn note(&self, id: &str) -> Result<Note> {
        let mut db = self.inner.db.clone();
        let row = NoteRow::filter_by_id(id)
            .first()
            .exec(&mut db)
            .await?
            .ok_or_else(|| Error::UnknownNote(id.to_owned()))?;
        let files = files_of(&mut db, &[id]).await?;
        Ok(note(row, &files))
    }
}

/// How many live notes each of `threads` has, for those with any.
pub(crate) async fn live_counts(
    exec: &mut dyn Executor,
    threads: &[String],
) -> Result<HashMap<String, usize>> {
    let threads = serde_json::to_string(threads).expect("ids serialize");
    let rows = toasty::sql::query("SELECT thread_id, count(*) FROM notes WHERE deleted_at IS NULL AND thread_id IN (SELECT value FROM json_each(?1)) GROUP BY thread_id")
        .bind(threads)
        .exec(exec)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match row {
            Value::Record(row) => match &row.fields[..] {
                [Value::String(thread), Value::I64(count)] => {
                    Some((thread.clone(), usize::try_from(*count).unwrap_or_default()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect())
}

/// What a lookup in the notes' index ([`NoteTerm`]) found: its live notes, as note id to thread
/// id, and their threads.
#[derive(Debug, Clone, Default)]
pub(crate) struct TermMatch {
    pub notes: BTreeMap<String, String>,
    pub threads: BTreeSet<String>,
}

/// The live notes each of `terms` finds. A term with an exact form keeps only the notes whose
/// text has it as typed, so a phrase matches within one note, word for word.
pub(crate) async fn term_matches(
    exec: &mut dyn Executor,
    terms: &[NoteTerm],
) -> Result<Vec<TermMatch>> {
    let mut out = Vec::with_capacity(terms.len());
    for term in terms {
        let rows = if term.expression.is_empty() {
            toasty::sql::query("SELECT id, thread_id, text FROM notes WHERE deleted_at IS NULL")
                .exec(exec)
                .await?
        } else {
            toasty::sql::query("SELECT id, thread_id, text FROM notes WHERE deleted_at IS NULL AND id IN (SELECT note_id FROM notes_fts WHERE notes_fts MATCH ?1)")
                .bind(term.expression.clone())
                .exec(exec)
                .await?
        };
        let mut found = TermMatch::default();
        for row in rows {
            let Value::Record(row) = row else { continue };
            let [
                Value::String(note),
                Value::String(thread),
                Value::String(text),
            ] = &row.fields[..]
            else {
                continue;
            };
            if term
                .exact
                .as_ref()
                .is_some_and(|exact| !exact.is_match(text))
            {
                continue;
            }
            found.threads.insert(thread.clone());
            found.notes.insert(note.clone(), thread.clone());
        }
        out.push(found);
    }
    Ok(out)
}

/// The notes `notes` as search results list them, by thread, each thread's in note order, with
/// an excerpt of `snippet_tokens` words that has what `expression` matches in bold.
pub(crate) async fn hits(
    exec: &mut dyn Executor,
    notes: &[String],
    expression: &str,
    snippet_tokens: i64,
) -> Result<HashMap<String, Vec<NoteHit>>> {
    let ids = serde_json::to_string(notes).expect("ids serialize");
    let rows = toasty::sql::query("SELECT note_id, snippet(notes_fts, 2, char(58704), char(58705), char(8230), ?1) FROM notes_fts WHERE notes_fts MATCH ?2 AND note_id IN (SELECT value FROM json_each(?3))")
        .bind(snippet_tokens)
        .bind(expression)
        .bind(ids)
        .exec(exec)
        .await?;
    let snippets: HashMap<String, String> = rows
        .into_iter()
        .filter_map(|row| match row {
            Value::Record(row) => match &row.fields[..] {
                [Value::String(note), Value::String(snippet)] => {
                    Some((note.clone(), snippet.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    let ids: Vec<&str> = notes.iter().map(String::as_str).collect();
    let mut rows = NoteRow::filter(NoteRow::fields().id().in_list(ids.as_slice()))
        .exec(exec)
        .await?;
    rows.retain(|row| row.deleted_at.is_none());
    rows.sort_by(|a, b| (a.created_at, a.number, &a.id).cmp(&(b.created_at, b.number, &b.id)));
    let mut hits: HashMap<String, Vec<NoteHit>> = HashMap::new();
    for row in rows {
        let snippet = snippets
            .get(&row.id)
            .map_or_else(Highlighted::default, |snippet| {
                Highlighted::from_marked(snippet)
            });
        hits.entry(row.thread_id).or_default().push(NoteHit {
            label: label(row.number),
            date: row.created_at,
            snippet,
            note_id: row.id,
        });
    }
    Ok(hits)
}

/// The text of `thread`'s live notes.
pub(crate) async fn live_texts(exec: &mut dyn Executor, thread: &str) -> Result<Vec<String>> {
    Ok(NoteRow::filter(NoteRow::fields().thread_id().eq(thread))
        .exec(exec)
        .await?
        .into_iter()
        .filter(|row| row.deleted_at.is_none())
        .map(|row| row.text)
        .collect())
}

/// Brings back the notes from the backup at `backup` when the store has none (a store made again
/// after signing out, or a Clear Cache whose old file couldn't be read), and fills the notes'
/// search index when it's missing some (a Clear Cache makes it again, empty).
///
/// A backup whose content can't be restored (it isn't JSON, isn't a backup, or has rows the
/// tables refuse) may be the only copy of the notes, so it's set aside where the next backup
/// can't overwrite it, and the app says so. One the database couldn't take just now stays where
/// it is for the next open to try, which a marker beside it asks for even if notes are written
/// meanwhile, and nothing overwrites it this session. Neither stops the store opening.
pub(crate) async fn recover(db: &mut Db, backup: &Path) -> Result<Recovered> {
    let mut recovered = Recovered::default();
    let marker = retry_marker(backup);
    let empty = query_i64(db, "SELECT count(*) FROM notes").await? == 0;
    if empty || marker.exists() {
        // With no drafts either, a backup that holds only drafts (a note being written when the
        // account signed out, and none saved yet) has something to bring back.
        let bare = empty && query_i64(db, "SELECT count(*) FROM note_drafts").await? == 0;
        match restore(db, backup, !empty, bare).await {
            Ok(()) => {
                let _ = std::fs::remove_file(&marker);
            }
            Err(Unrestored::Content(error)) => {
                let aside = set_aside(backup);
                match std::fs::rename(backup, &aside) {
                    Ok(()) => {
                        log::error!(
                            "the notes' backup at {} can't be restored, and is kept as {}: {error}",
                            backup.display(),
                            aside.display()
                        );
                        recovered.set_aside = file_name(&aside);
                        let _ = std::fs::remove_file(&marker);
                    }
                    // It stays under its own name, and nothing is written over it.
                    Err(rename) => {
                        log::error!(
                            "the notes' backup at {} can't be restored ({error}) or set aside ({rename}), so no backup is written this session",
                            backup.display()
                        );
                        recovered.set_aside = file_name(backup);
                        recovered.disabled = true;
                    }
                }
            }
            Err(Unrestored::Later(error)) => {
                log::error!(
                    "the notes' backup at {} wasn't restored, and is left for the next open: {error}",
                    backup.display()
                );
                if let Err(error) = std::fs::write(&marker, b"") {
                    log::error!("marking the notes' backup to restore: {error}");
                }
                recovered.disabled = true;
            }
        }
    }
    let live = query_i64(db, "SELECT count(*) FROM notes WHERE deleted_at IS NULL").await?;
    if query_i64(db, "SELECT count(*) FROM notes_fts").await? < live {
        let ids: Vec<String> = strings(
            toasty::sql::query("SELECT id FROM notes WHERE deleted_at IS NULL")
                .exec(db)
                .await?,
        )
        .collect();
        let mut tx = db.transaction().await?;
        toasty::sql::statement("DELETE FROM notes_fts")
            .exec(&mut tx)
            .await?;
        for id in &ids {
            index_note(&mut tx, id).await?;
        }
        tx.commit().await?;
        log::info!("indexed {} notes again", ids.len());
    }
    Ok(recovered)
}

/// Why a backup wasn't restored.
#[derive(Debug)]
enum Unrestored {
    /// What's in the file can't be restored, now or later.
    Content(String),
    /// The file couldn't be read, or the database couldn't take it, this time.
    Later(String),
}

impl Unrestored {
    /// A failure of the database while it took the backup's rows: the rows' own fault when a
    /// constraint or a column's type refused them, and otherwise the database's (it's busy, the
    /// disk is full), which the next open may not meet.
    fn from_database(error: &toasty::Error) -> Self {
        let text = error.to_string();
        let lower = text.to_lowercase();
        if lower.contains("constraint") || lower.contains("mismatch") {
            Self::Content(text)
        } else {
            Self::Later(text)
        }
    }
}

impl std::fmt::Display for Unrestored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Content(error) | Self::Later(error) => f.write_str(error),
        }
    }
}

/// Where a backup at `backup` that can't be restored is kept:
/// `{account}.notes-backup.unrestored-{epoch milliseconds}.json` beside it.
fn set_aside(backup: &Path) -> PathBuf {
    let base = backup.with_extension("");
    PathBuf::from(format!(
        "{}.unrestored-{}.json",
        base.display(),
        Timestamp::now().as_millisecond()
    ))
}

/// The file beside `backup` that says the next open should restore it, whatever the tables
/// hold: `{account}.notes-backup.pending`.
fn retry_marker(backup: &Path) -> PathBuf {
    backup.with_extension("pending")
}

/// `path`'s file name, as the app shows it.
fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Copies the backup at `path` into the notes' tables in one transaction, so a failure leaves
/// them as they were. Into empty tables, a backup's row replaces any row left with its key (a
/// draft kept before the notes came back). With `merge` (the tables have notes written since a
/// restore failed), only the rows the tables lack are added. A backup without notes copies
/// nothing, unless the tables are `bare` (no notes and no drafts) and it has drafts, which are
/// then what there is to bring back. A column the backup lacks (it's from an older Exo) takes
/// its default.
async fn restore(
    db: &mut Db,
    path: &Path,
    merge: bool,
    bare: bool,
) -> std::result::Result<(), Unrestored> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        // Not text at all.
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(Unrestored::Content(error.to_string()));
        }
        Err(error) => return Err(Unrestored::Later(error.to_string())),
    };
    let backup: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| Unrestored::Content(error.to_string()))?;
    for table in TABLES {
        let rows = match &backup[table] {
            serde_json::Value::Array(rows) => rows,
            // An older backup may lack a table, though never the notes.
            serde_json::Value::Null if table != "notes" => continue,
            _ => return Err(Unrestored::Content(format!("it has no {table} table"))),
        };
        if rows.iter().any(|row| !row.is_object()) {
            return Err(Unrestored::Content(format!(
                "its {table} table has a row that isn't one"
            )));
        }
    }
    let has = |table: &str| {
        backup[table]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    };
    if !has("notes") && !(bare && has("note_drafts")) {
        return Ok(());
    }
    let later = |error: toasty::Error| Unrestored::Later(error.to_string());
    let mut tx = db.transaction().await.map_err(later)?;
    for table in TABLES {
        let columns = table_columns(&mut tx, table)
            .await
            .map_err(|error| Unrestored::Later(error.to_string()))?;
        let names = columns
            .iter()
            .map(|column| format!(r#""{}""#, column.name))
            .collect::<Vec<_>>()
            .join(", ");
        let values = columns
            .iter()
            .map(Column::backed_up)
            .collect::<Vec<_>>()
            .join(", ");
        let statement = if merge {
            let same = columns
                .iter()
                .filter(|column| column.key)
                .map(|column| {
                    format!(
                        r#"kept."{name}" = json_extract(value, '$."{name}"')"#,
                        name = column.name
                    )
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            format!(
                r#"INSERT INTO "{table}" ({names}) SELECT {values} FROM json_each(?1, '$.{table}') WHERE NOT EXISTS (SELECT 1 FROM "{table}" AS kept WHERE {same})"#
            )
        } else {
            format!(
                r#"INSERT OR REPLACE INTO "{table}" ({names}) SELECT {values} FROM json_each(?1, '$.{table}')"#
            )
        };
        let rows = toasty::sql::statement(statement)
            .bind(text.clone())
            .exec(&mut tx)
            .await
            .map_err(|error| Unrestored::from_database(&error))?;
        log::info!("restored {rows} rows of {table} from the notes' backup");
    }
    // Numbers are never reused, whichever side a thread's count came from: it's past the
    // backup's count and every note's number.
    toasty::sql::statement("UPDATE note_threads SET next_number = max(next_number, coalesce((SELECT max(json_extract(value, '$.next_number')) FROM json_each(?1, '$.note_threads') WHERE json_extract(value, '$.thread_id') = note_threads.thread_id), 0), coalesce((SELECT max(number) FROM notes WHERE notes.thread_id = note_threads.thread_id), 0) + 1)")
        .bind(text.clone())
        .exec(&mut tx)
        .await
        .map_err(|error| Unrestored::from_database(&error))?;
    tx.commit().await.map_err(later)?;
    Ok(())
}

/// The notes' tables as one JSON document, `{table: [row, …]}`, each row an object of its
/// columns, or `None` if the database gave nothing. It's read in one statement, so it's one
/// moment's tables.
async fn dump(exec: &mut dyn Executor) -> Result<Option<String>> {
    let mut parts = Vec::new();
    for table in TABLES {
        let fields = table_columns(exec, table)
            .await?
            .iter()
            .map(|column| format!(r#"'{name}', "{name}""#, name = column.name))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            r#"'{table}', json((SELECT json_group_array(json_object({fields})) FROM "{table}"))"#
        ));
    }
    let rows = toasty::sql::query(format!("SELECT json_object({})", parts.join(", ")))
        .exec(exec)
        .await?;
    Ok(strings(rows).next().filter(|text| !text.is_empty()))
}

/// A column of a notes table.
struct Column {
    name: String,
    /// Its default as the schema writes it, for a column that has one.
    default: Option<String>,
    /// Whether it's part of the table's key.
    key: bool,
}

impl Column {
    /// Its value in a backup's row (`value`, of `json_each`): what the row has, or the column's
    /// default where the row has nothing.
    fn backed_up(&self) -> String {
        let read = format!(r#"json_extract(value, '$."{}"')"#, self.name);
        match &self.default {
            Some(default) => format!("coalesce({read}, {default})"),
            None => read,
        }
    }
}

/// The columns of `table`, in order.
async fn table_columns(exec: &mut dyn Executor, table: &str) -> Result<Vec<Column>> {
    let rows =
        toasty::sql::query("SELECT name, dflt_value, pk FROM pragma_table_info(?1) ORDER BY cid")
            .bind(table)
            .exec(exec)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match row {
            Value::Record(row) => match &row.fields[..] {
                [Value::String(name), default, key] => Some(Column {
                    name: name.clone(),
                    default: match default {
                        Value::String(default) => Some(default.clone()),
                        _ => None,
                    },
                    key: !matches!(key, Value::I64(0)),
                }),
                _ => None,
            },
            _ => None,
        })
        .collect())
}

/// Saves the thread's context with a note being saved, from the thread as it's stored now (or as
/// it was when it isn't stored). Returns the note's number: the thread's next one for a `new`
/// note on a message, and `None` otherwise.
async fn save_context(exec: &mut dyn Executor, save: &SaveNote, new: bool) -> Result<Option<u32>> {
    let thread = save.thread_id.as_str();
    let stored = NoteThreadRow::filter_by_thread_id(thread)
        .first()
        .exec(exec)
        .await?;
    let next_number = stored.as_ref().map_or(1, |row| row.next_number);
    let number = (new && save.message_id.is_some()).then_some(next_number);
    let next_number = if number.is_some() {
        next_number + 1
    } else {
        next_number
    };
    let context = thread_context(exec, thread).await?;
    match (stored, context) {
        (Some(_), None) => {
            NoteThreadRow::update_by_thread_id(thread)
                .next_number(next_number)
                .exec(exec)
                .await?;
        }
        (Some(_), Some(context)) => {
            NoteThreadRow::update_by_thread_id(thread)
                .subject(context.subject)
                .participants(Json(context.participants))
                .first_date(context.first_date)
                .last_date(context.last_date)
                .messages(context.messages)
                .next_number(next_number)
                .exec(exec)
                .await?;
        }
        (None, context) => {
            let context = context.unwrap_or_else(|| NoteThread {
                thread_id: save.thread_id.clone(),
                subject: String::new(),
                participants: Vec::new(),
                first_date: None,
                last_date: None,
                messages: 0,
            });
            NoteThreadRow::create()
                .thread_id(thread)
                .subject(context.subject)
                .participants(Json(context.participants))
                .first_date(context.first_date)
                .last_date(context.last_date)
                .messages(context.messages)
                .next_number(next_number)
                .gone(false)
                .exec(exec)
                .await?;
        }
    }
    Ok(number)
}

/// The stored thread's context, or `None` when it isn't stored.
async fn thread_context(exec: &mut dyn Executor, thread: &str) -> Result<Option<NoteThread>> {
    let Some(view) = load_view(exec, thread).await? else {
        return Ok(None);
    };
    let received: Vec<&MessageRow> = view
        .messages
        .iter()
        .filter(|message| !message.is_draft())
        .map(|message| &message.row)
        .collect();
    let messages = if received.is_empty() {
        view.messages.iter().map(|message| &message.row).collect()
    } else {
        received
    };
    let mut participants: Vec<NoteParticipant> = Vec::new();
    for mailbox in messages.iter().flat_map(|row| {
        row.sender
            .0
            .iter()
            .chain(row.to.iter())
            .chain(row.cc.iter())
    }) {
        match participants
            .iter_mut()
            .find(|known| known.email.eq_ignore_ascii_case(&mailbox.email))
        {
            Some(known) => {
                if known.name.is_none() {
                    known.name.clone_from(&mailbox.name);
                }
            }
            None => participants.push(NoteParticipant {
                name: mailbox.name.clone(),
                email: mailbox.email.clone(),
            }),
        }
    }
    let dates = messages
        .iter()
        .filter_map(|row| Timestamp::from_millisecond(row.internal_date).ok());
    Ok(Some(NoteThread {
        thread_id: ThreadId::new(thread),
        subject: view.subject(),
        participants,
        first_date: dates.clone().min(),
        last_date: dates.max(),
        messages: u32::try_from(messages.len()).unwrap_or(u32::MAX),
    }))
}

/// Who sent message `message` (their name, or else their address) and its date, or `None`s when
/// it isn't stored.
async fn message_context(
    exec: &mut dyn Executor,
    message: &str,
) -> Result<(Option<String>, Option<Timestamp>)> {
    let Some(row) = MessageRow::filter_by_id(message).first().exec(exec).await? else {
        return Ok((None, None));
    };
    let from = row.sender.0.map(|sender| {
        sender
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or(sender.email)
    });
    Ok((from, Timestamp::from_millisecond(row.internal_date).ok()))
}

/// Writes a note's version.
#[allow(clippy::too_many_arguments)]
async fn add_version(
    exec: &mut dyn Executor,
    note: &str,
    version: u32,
    html: &str,
    text: &str,
    files: Vec<String>,
    saved_at: Timestamp,
    change: NoteChange,
) -> Result<()> {
    NoteVersionRow::create()
        .note_id(note)
        .version(version)
        .html(html)
        .text(text)
        .files(Json(files))
        .saved_at(saved_at)
        .change(change.as_str())
        .exec(exec)
        .await?;
    Ok(())
}

/// The files of `notes`, by uuid.
async fn files_of(exec: &mut dyn Executor, notes: &[&str]) -> Result<HashMap<String, NoteFile>> {
    Ok(
        NoteFileRow::filter(NoteFileRow::fields().note_id().in_list(notes))
            .exec(exec)
            .await?
            .into_iter()
            .map(|row| {
                (
                    row.uuid.clone(),
                    NoteFile {
                        uuid: row.uuid,
                        name: row.name,
                        mime_type: row.mime_type,
                        size: row.size,
                        content_id: row.content_id,
                    },
                )
            })
            .collect(),
    )
}

/// The files `uuids` names, in its order.
fn resolve(uuids: &[String], files: &HashMap<String, NoteFile>) -> Vec<NoteFile> {
    uuids
        .iter()
        .filter_map(|uuid| files.get(uuid).cloned())
        .collect()
}

fn note(row: NoteRow, files: &HashMap<String, NoteFile>) -> Note {
    Note {
        files: resolve(&row.files.0, files),
        id: row.id,
        thread_id: ThreadId::new(row.thread_id),
        message_id: row.message_id,
        message_from: row.message_from,
        message_date: row.message_date,
        number: row.number,
        kind: row.kind,
        html: row.html,
        text: row.text,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
        version: row.version,
    }
}
