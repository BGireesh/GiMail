//! Thread notes: the user's own notes on a conversation or one of its messages, kept on this
//! device only. They never reach Gmail.
//!
//! Every save, delete and restore is kept as a version. After each, the notes' tables are written
//! to a standing backup beside the store (`{account}.notes-backup.json`), which signing out leaves
//! alone. A store opened with no notes and a backup that has some takes them from it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
use crate::search::{index_note, unindex_note};
use crate::store::{Store, load_view};

/// The tables the backup holds, in the order they're restored.
const TABLES: [&str; 5] = [
    "note_threads",
    "notes",
    "note_versions",
    "note_files",
    "note_drafts",
];

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
        match self.number {
            Some(number) => format!("Note {number}"),
            None => "Thread note".to_owned(),
        }
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
    /// How it's stored.
    fn as_str(self) -> &'static str {
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

/// A thread's notes, as its view and notes pane show them.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        rows.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
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
    /// (or left as it was when it isn't). The notes' backup is written after.
    ///
    /// The file's bytes must be in [`note_files`](Self::note_files) already.
    ///
    /// # Errors
    /// On database failures, or when the backup can't be written.
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
        self.backup_notes().await?;
        self.note(&id).await.map(Some)
    }

    /// Deletes note `id` at `now`. It stays in the thread's deleted notes and its history, and
    /// [`restore_note`](Self::restore_note) brings it back. Deleting a deleted note does nothing.
    ///
    /// # Errors
    /// [`Error::UnknownNote`] for a note that isn't stored, on database failures, or when the
    /// backup can't be written.
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
        self.backup_notes().await
    }

    /// Brings note `id` back at `now`: undeletes it, or with `version`, saves that version of it
    /// as its newest (and undeletes it). Undeleting a live note does nothing.
    ///
    /// # Errors
    /// [`Error::UnknownNote`] for a note or version that isn't stored, on database failures, or
    /// when the backup can't be written.
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
        self.backup_notes().await?;
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
        write.commit().await
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
        write.publish().await
    }

    /// The folder notes' files' bytes are kept in, each named by its `uuid`. The app writes them.
    /// Signing out ([`Store::delete`]) leaves it.
    #[must_use]
    pub fn note_files(&self) -> &Path {
        &self.inner.notes
    }

    /// Writes the notes' tables to the standing backup beside the store, whole: a temporary file
    /// renamed over it. Backups are written one at a time, each reading the tables as they are
    /// then, so the last one written holds the latest notes.
    ///
    /// # Errors
    /// On database failures, or when the file can't be written.
    pub async fn backup_notes(&self) -> Result<()> {
        let _turn = self.inner.backing_up.lock().await;
        let mut db = self.inner.db.clone();
        let backup = dump(&mut db).await?;
        let path = &self.inner.notes_backup;
        let temporary = PathBuf::from(format!("{}.tmp", path.display()));
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| Error::Io { path, source }
        };
        std::fs::write(&temporary, backup).map_err(io(&temporary))?;
        std::fs::rename(&temporary, path).map_err(io(path))
    }

    /// Note `id` as stored.
    async fn note(&self, id: &str) -> Result<Note> {
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

/// Brings back the notes from the backup at `backup` when the store has none (a store made again
/// after signing out, or a Clear Cache whose old file couldn't be read), and fills the notes'
/// search index when it's missing some (a Clear Cache makes it again, empty).
pub(crate) async fn recover(db: &mut Db, backup: &Path) -> Result<()> {
    if query_i64(db, "SELECT count(*) FROM notes").await? == 0 {
        // A backup that can't be restored isn't a reason the store can't open, but it may be the
        // only copy of the notes, so it's set aside where the next backup can't overwrite it (and
        // signing out leaves it). A restore is all or nothing, so the tables stay empty.
        if let Err(error) = restore(db, backup).await {
            let aside = set_aside(backup);
            log::error!(
                "the notes' backup at {} can't be restored, and is kept as {}: {error}",
                backup.display(),
                aside.display()
            );
            std::fs::rename(backup, &aside).map_err(|source| Error::Io {
                path: backup.to_owned(),
                source,
            })?;
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
    Ok(())
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

/// Copies the backup at `path` into the notes' tables, which are empty, in one transaction, so a
/// failure leaves them empty. A backup without notes copies nothing.
async fn restore(db: &mut Db, path: &Path) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    let unreadable = |error: String| Error::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
    };
    let backup: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| unreadable(error.to_string()))?;
    match backup["notes"].as_array() {
        None => return Err(unreadable("it has no notes table".to_owned())),
        Some(notes) if notes.is_empty() => return Ok(()),
        Some(_) => {}
    }
    let mut tx = db.transaction().await?;
    for table in TABLES {
        let columns = columns(&mut tx, table).await?;
        let names = columns
            .iter()
            .map(|column| format!(r#""{column}""#))
            .collect::<Vec<_>>()
            .join(", ");
        let values = columns
            .iter()
            .map(|column| format!(r#"json_extract(value, '$."{column}"')"#))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = toasty::sql::statement(format!(
            r#"INSERT INTO "{table}" ({names}) SELECT {values} FROM json_each(?1, '$.{table}')"#
        ))
        .bind(text.clone())
        .exec(&mut tx)
        .await?;
        log::info!("restored {rows} rows of {table} from the notes' backup");
    }
    tx.commit().await?;
    Ok(())
}

/// The notes' tables as one JSON document, `{table: [row, …]}`, each row an object of its
/// columns. It's read in one statement, so it's one moment's tables.
async fn dump(exec: &mut dyn Executor) -> Result<String> {
    let mut parts = Vec::new();
    for table in TABLES {
        let fields = columns(exec, table)
            .await?
            .iter()
            .map(|column| format!(r#"'{column}', "{column}""#))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            r#"'{table}', json((SELECT json_group_array(json_object({fields})) FROM "{table}"))"#
        ));
    }
    let rows = toasty::sql::query(format!("SELECT json_object({})", parts.join(", ")))
        .exec(exec)
        .await?;
    Ok(strings(rows).next().unwrap_or_default())
}

/// The columns of `table`, in order.
async fn columns(exec: &mut dyn Executor, table: &str) -> Result<Vec<String>> {
    Ok(strings(
        toasty::sql::query("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
            .bind(table)
            .exec(exec)
            .await?,
    )
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
