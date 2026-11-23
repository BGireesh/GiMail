# Thread Notes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Private, versioned, searchable, shareable rich-text notes on email threads and messages, stored locally in the mail database.

**Architecture:** `exo-store` gains notes tables, a notes API on `Store`, a notes FTS index and a standing JSON backup. `exo` gains a `notes` module that draws note cards and an in-place editor (reusing `exo_editor::Editor`) inside the thread view and the right pane; notes reach the UI through the account `Snapshot`, and changes go out as `Request`s. Search, share, history and the Markdown mirror build on that.

**Tech Stack:** Rust, egui/eframe, `exo-editor`, toasty over SQLite with hand-written migrations, FTS5, gmail-mock for tests. No new crates.

**Spec:** `docs/superpowers/specs/2026-11-23-thread-notes-design.md`. Read it before any task. Prototypes: `.superpowers/brainstorm/38499-1795439961/content/notes-prototype.html`, `notes-search.html`.

## Global Constraints

- No new third-party dependency in any `Cargo.toml`.
- Branch `gireesh/thread-notes`. Never commit to or merge into `main`. Never push.
- Never add Claude/Anthropic attribution to commits. Never commit `fastlane/Fastfile`, `.bundle/`, `vendor/`, `.superpowers/`.
- Notes never enter a reply, forward, print, or any Gmail call. Only Share (Task 9) and the Markdown folder (Task 11) take note content out of the database.
- Both themes (Carbon, Snow). Amber comes from new `exo-ui` palette roles, never literals in views.
- Labels: `Thread note` for a note with no message; `Note {number}` otherwise. Numbers are per thread, in order of first save, never reused.
- Keys: `T` new thread note, `⇧T` new note on the focused message, `⌘⇧N` switch the right pane between Notes and Contact, `⌘↩` save and close, `Esc` close keeping the text. `N`/`⇧N` are taken.
- One file may be up to 100 MB (`100 * 1024 * 1024` bytes).
- Follow the codebase's comment and test style (doc comments on items, sentence-style test names). `cargo fmt --all` before each commit.
- Every task ends with `cargo test -p <crates touched>` passing; the stage's last task runs `cargo test --workspace` (output to a file; it takes minutes).
- One deviation from the spec, decided here: the standing backup is `{account}.notes-backup.json` (serde, written to a temp file then renamed), not a `.sqlite3` file. Same guarantees, far less machinery.

## Review Focus

1. A thread whose mail rows are gone (deleted in Gmail, or a new empty database) but whose notes exist: opening, searching and sharing must work from `note_threads`, not panic or show nothing. Test in Tasks 2 and 8.
2. A note whose editor is open when the thread changes under it (new mail arrives, the thread is trashed, the user presses J/K or Esc out of the thread): the typed text must be saved as a note draft, not lost and not left focused invisibly. Test in Task 4.
3. Pasting or dropping a very large or zero-byte file, or an image the decoder rejects, into a note: refused with a toast, the note unchanged. Test in Task 6.
4. Search text with FTS5 syntax in it (`notes:"`, `notes:a-b`, `notes:*`, `notes:OR`): never an SQL or FTS error, as mail search already guarantees. Test in Task 7.
5. Two saves racing (autosave of a draft and ⌘↩, or a save while the backup is being written): one version per close, the backup never truncated or half-written. Test in Task 2.

---

## Stage 1: notes in the thread and the pane

### Task 1: Schema

**Files:**
- Modify: `crates/exo-store/src/db.rs` (models, `KEPT_TABLES` 6 → 11)
- Create: `crates/exo-store/migrations/0008_notes.sql`
- Modify: `crates/exo-store/src/migrations.rs` (`FILES`, the `search_fts` exclusions at ~:345 and ~:442 extended to `notes_fts`, the hard-coded migration name lists at ~:810–858)
- Modify: `crates/exo-store/tests/clear_cache.rs`

**Interfaces:**
- Produces toasty models in `db.rs`, all `pub(crate)`, timestamps `jiff::Timestamp`:
  - `NoteThreadRow` `#[table = "note_threads"]`: `#[key] thread_id: String`, `subject: String`, `participants: Json<Vec<NoteParticipant>>`, `first_date: Option<Timestamp>`, `last_date: Option<Timestamp>`, `messages: u32`, `next_number: u32`.
  - `NoteRow` `#[table = "notes"]`: `#[key] id: String`, `#[index] thread_id: String`, `message_id: Option<String>`, `message_from: Option<String>`, `message_date: Option<Timestamp>`, `number: Option<u32>`, `kind: String`, `html: String`, `text: String`, `files: Json<Vec<String>>`, `created_at`, `updated_at`, `deleted_at: Option<Timestamp>`, `version: u32`.
  - `NoteVersionRow` `#[table = "note_versions"]`, `#[key(note_id, version)]`: `note_id: String`, `version: u32`, `html`, `text`, `files: Json<Vec<String>>`, `saved_at`, `change: String`.
  - `NoteFileRow` `#[table = "note_files"]`: `#[key] uuid: String`, `#[index] note_id: String`, `name`, `mime_type`, `size: u64`, `content_id: Option<String>`.
  - `NoteDraftRow` `#[table = "note_drafts"]`: `#[key] id: String`, `#[index] thread_id: String`, `message_id: Option<String>`, `note_id: Option<String>`, `html`, `text`, `files: Json<Vec<NoteFile>>`, `updated_at`.
  - `NoteParticipant { name: Option<String>, email: String }` and `NoteFile { uuid, name, mime_type, size: u64, content_id: Option<String> }`, public, serde, defined in a new `crates/exo-store/src/notes.rs` and re-exported from `lib.rs`.
- Migration `0008_notes.sql`: the five tables as a new file has them, then `CREATE VIRTUAL TABLE notes_fts USING fts5(note_id UNINDEXED, thread_id UNINDEXED, text, files, tokenize = "porter ascii tokenchars '_'");`, statements separated by `-- #[toasty::breakpoint]`. New random 63-bit id.

- [ ] **Step 1:** Add to `tests/clear_cache.rs`: the five tables in the `kept` list, `notes_fts%` excluded beside `search_fts%`. Add to `migrations.rs` tests a `the_notes_migration_starts_empty` test modelled on `the_reminders_migration_starts_empty_and_keeps_what_is_set`. Run `cargo test -p exo-store migrations clear_cache`; expect failures naming the missing tables.
- [ ] **Step 2:** Add the models, the migration, the `FILES` entry, `KEPT_TABLES`, and the `notes_fts` exclusions.
- [ ] **Step 3:** `cargo test -p exo-store` passes, including `migrations_make_the_models_schema` and `an_upgraded_file_has_a_new_files_schema`.
- [ ] **Step 4:** Commit `feat: notes tables`.

### Task 2: Notes API in the store

**Files:**
- Modify: `crates/exo-store/src/notes.rs`, `lib.rs`, `store.rs` (`Inner.notes: PathBuf` = `{account}.notes`, restore-on-open), `evict.rs` (`kept`), `model.rs` + `view.rs` + `store.rs::load_views` (`ThreadSummary.notes`), `search.rs` (only an `index_note`/`unindex_note` pair used here)
- Test: `crates/exo-store/tests/notes.rs` (new), `tests/evict.rs`, `tests/clear_cache.rs`
- Modify: `crates/exo/src/row_menu.rs`, `crates/exo/src/inbox/mod.rs`, `crates/exo/src/mailbox/mod.rs` (struct literals and `pub notes: usize` on the app's `ThreadSummary`)

**Interfaces (all public, in `exo_store`):**
```rust
pub struct Note { pub id: String, pub thread_id: ThreadId, pub message_id: Option<String>,
    pub message_from: Option<String>, pub message_date: Option<Timestamp>,
    pub number: Option<u32>, pub kind: String, pub html: String, pub text: String,
    pub files: Vec<NoteFile>, pub created_at: Timestamp, pub updated_at: Timestamp,
    pub deleted_at: Option<Timestamp>, pub version: u32 }
impl Note { pub fn label(&self) -> String }            // "Thread note" | "Note 3"
pub struct NoteDraft { pub id: String, pub thread_id: ThreadId, pub message_id: Option<String>,
    pub note_id: Option<String>, pub html: String, pub text: String, pub files: Vec<NoteFile>,
    pub updated_at: Timestamp }
pub struct NoteVersion { pub version: u32, pub html: String, pub text: String,
    pub files: Vec<NoteFile>, pub saved_at: Timestamp, pub change: NoteChange }
pub enum NoteChange { Created, Edited, Deleted, Restored }   // stored lowercase
pub struct NoteThread { pub thread_id: ThreadId, pub subject: String,
    pub participants: Vec<NoteParticipant>, pub first_date: Option<Timestamp>,
    pub last_date: Option<Timestamp>, pub messages: u32 }
pub struct ThreadNotes { pub context: Option<NoteThread>, pub notes: Vec<Note>,      // live, by created_at
    pub deleted: Vec<Note>, pub drafts: Vec<NoteDraft> }
pub struct SaveNote { pub draft_id: String, pub note_id: Option<String>, pub thread_id: ThreadId,
    pub message_id: Option<String>, pub html: String, pub text: String, pub files: Vec<NoteFile> }
impl Store {
    pub async fn thread_notes(&self, thread: &ThreadId) -> Result<ThreadNotes>;
    pub async fn save_note(&self, save: SaveNote, now: Timestamp) -> Result<Option<Note>>;
    pub async fn delete_note(&self, id: &str, now: Timestamp) -> Result<()>;
    pub async fn restore_note(&self, id: &str, version: Option<u32>, now: Timestamp) -> Result<Note>;
    pub async fn note_versions(&self, id: &str) -> Result<Vec<NoteVersion>>;   // newest first
    pub async fn save_note_draft(&self, draft: NoteDraft) -> Result<()>;
    pub async fn discard_note_draft(&self, id: &str) -> Result<()>;
    pub fn note_files(&self) -> &Path;                                         // {account}.notes
    pub async fn backup_notes(&self) -> Result<()>;
}
```
Rules:
- `save_note`: an empty `text` with no files on a new note returns `Ok(None)` and only discards the draft. Unchanged html and files on an existing note write no version. Otherwise one transaction: upsert `note_threads` from the stored thread (subject, participants, dates, message count; left as it was when the thread isn't stored), take `next_number` for a new message note, write `notes` and a `note_versions` row (`created`/`edited`), write `note_files` rows, delete the draft row, reindex `notes_fts`, `publish()`. Then `backup_notes()`.
- `restore_note(id, None)` undeletes; `Some(v)` copies that version in as a new `restored` version (and undeletes).
- A file's row stays while any version lists its uuid. Bytes are written by the app, not the store.
- `backup_notes` serializes the five tables to `{account}.notes-backup.json` via a temp file and `rename`, under the write mutex's ordering so the latest state wins. Opening a store with no `notes` rows and a backup file with some restores them and rebuilds `notes_fts`. `notes_fts` is also rebuilt at open when it has fewer rows than live notes (after Clear Cache).
- `evict.rs::kept`: add, unconditionally beside `DRAFTED`, `SELECT DISTINCT thread_id FROM notes WHERE deleted_at IS NULL`.
- `ThreadSummary.notes: usize` = live notes, filled in `load_views` with one grouped query per batch.
- `provider::delete_thread` and bankruptcy are not touched: notes survive them.

- [ ] **Step 1:** Write `tests/notes.rs` with these tests (harness as in `tests/reminders.rs`): `a_saved_note_comes_back_with_its_number_and_context` (thread note has `number == None`, first message note 1, second 2; `context.subject` is the thread's); `numbers_are_never_reused` (save two, delete Note 1, save a third → 3); `an_edit_adds_a_version_and_an_unchanged_save_does_not`; `deleting_keeps_the_note_in_history_and_restore_brings_it_back`; `restoring_an_old_version_saves_it_as_a_new_one`; `an_empty_new_note_is_dropped`; `a_draft_survives_reopening_and_is_gone_after_the_save`; `notes_survive_the_threads_deletion` (delete the thread on the mock, sync, `thread_notes` still returns notes and `context`); `the_backup_restores_into_an_empty_store` (save, `Store::delete`, reopen, sync, notes are back); `concurrent_saves_leave_one_version_each_and_a_whole_backup` (join two `save_note` calls on different notes and one `save_note_draft`; backup parses and holds both). In `tests/evict.rs` add `a_thread_with_a_note_is_never_evicted`; in `tests/clear_cache.rs` extend `clearing_a_newer_exos_cache_keeps_what_gmail_doesnt_have` with a note, its version and its file row.
- [ ] **Step 2:** Run them; they fail to compile.
- [ ] **Step 3:** Implement per the interfaces and rules.
- [ ] **Step 4:** `cargo test -p exo-store` and `cargo build -p exo` pass.
- [ ] **Step 5:** Commit `feat: notes in the store, with history and a backup`.

### Task 3: App plumbing, setting, region, colors

**Files:**
- Modify: `crates/exo/src/account.rs` (`Snapshot.notes: Arc<ThreadNotes>` for the open thread, read in `publish()` beside `thread`/`bodiless`; `Request::{SaveNote, DeleteNote, RestoreNote, SaveNoteDraft, DiscardNoteDraft, NoteVersions}`; `Event::NoteVersions { id, versions }`, `Event::NoteFailed(String)`; `Account::{save_note, delete_note, restore_note, save_note_draft, discard_note_draft, note_versions, note_files}`)
- Modify: `crates/exo-store/src/settings.rs`: `pub notes_placement: NotesPlacement` (`#[serde(rename_all = "lowercase")] enum { #[default] Thread, Pane }`, `skip_serializing_if = "is_default"`)
- Modify: `crates/exo-commands/src/region.rs`: `Region::NoteEditor`, in `is_text_input` and `PALETTE_TOGGLE`
- Modify: `crates/exo-ui/src/theme.rs` `roles!`: `note_bar_default` (`#b7791f`, `#e0b24d`), `note_background_default` (`#fbf3df`, `#2f2c22`), `note_label_default` (`#8a5a00`, `#e0b24d`), `note_flash_default` (`#f3dfa8`, `#5a4d22`), `note_mark_default` (`#f6d98a`, `#6b5a1e`) (Snow, Carbon)

**Interfaces:**
- Consumes Task 2's `Store` API.
- Produces: `Snapshot.notes`; `Account::save_note(&self, save: SaveNote)`, `delete_note(&self, id: String)`, `restore_note(&self, id: String, version: Option<u32>)`, `save_note_draft(&self, draft: NoteDraft)`, `discard_note_draft(&self, id: String)`, `note_versions(&self, id: String)`, `note_files(&self) -> PathBuf`. A failed store call posts `Event::NoteFailed` and the app shows an error toast `Couldn't save the note.`

- [ ] **Step 1:** Add `crates/exo/tests/notes.rs` with `mod support;` and one test `a_note_saved_through_the_account_reaches_the_snapshot` (open a thread, call `app.mail_mut()`'s account `save_note`, `settle`, assert `Mail::notes()` (new accessor returning `&ThreadNotes`) has one note labelled `Thread note`). Run; fails to compile.
- [ ] **Step 2:** Implement. Round-trip test for `notes_placement` in `settings.rs` tests (`{"notesPlacement":"pane"}`; default omitted).
- [ ] **Step 3:** `cargo test -p exo-store settings` and `cargo test -p exo --test notes` pass.
- [ ] **Step 4:** Commit `feat: notes reach the app`.

### Task 4: Notes in the thread, and the editor

**Files:**
- Create: `crates/exo/src/notes/mod.rs` (state), `notes/card.rs` (a read-only note card), `notes/editor.rs` (the in-place editor)
- Modify: `crates/exo/src/thread/mod.rs` (`Entry::{ThreadNote(usize), MessageNote(usize), NoteEditor, AddThreadNote}` in `order()`, the layout loop and `paint_messages`; `Hit`/`Action` for `+ Note`), `thread/message.rs` (`+ Note` beside Reply/Forward), `commands.rs` (`Action::{NewThreadNote, NewMessageNote, SaveNote, CloseNote, EditNote(String), DeleteNote(String)}`, registrations), `lib.rs` (owning `notes::Notes`, region choice, running actions, saving on leave)
- Test: `crates/exo/tests/notes.rs`

**Interfaces:**
- Consumes Task 3.
- Produces in `crate::notes`:
  - `pub struct Notes` on `Mail`, with `pub fn set(&mut self, notes: Arc<ThreadNotes>, placement: NotesPlacement)`, `pub fn editing(&self) -> Option<&Editing>`, `pub fn start(&mut self, target: Target, now: Timestamp)`, `pub fn close(&mut self, account: &Account, now: Timestamp)` (saves when changed, drops an empty new note), `pub fn region(&self) -> Option<Region>` (`NoteEditor` while the editor has focus).
  - `pub enum Target { Thread, Message(String), Existing(String) }`.
  - `pub fn card_height(&mut self, ui: &Ui, note: &Note, width: f32) -> f32`, `pub fn card(&mut self, ui: &mut Ui, note: &Note, rect: Rect, clip: Rect) -> Option<CardAction>` (`CardAction::{Edit, Delete, History}`), `pub fn editor_height(&mut self, ui: &Ui, width: f32) -> f32`, `pub fn editor(&mut self, ui: &mut Ui, rect: Rect) -> Option<EditorAction>`.
  - Test accessors on `Mail`: `notes() -> &ThreadNotes`, `note_editor_text() -> Option<String>`, `note_rects() -> Vec<(String, Rect)>` (id, last painted rect).
- Read-only bodies are an `exo_editor::Editor` per note id, cached with the note's `version`, under an input-blocking `ui.interact` overlay (Out of Office's disabled pattern, `out_of_office/dialog.rs:960`). The editing one follows Out of Office's message editor (`out_of_office/dialog.rs:888–985`, commands `out_of_office/mod.rs:153–188`): Esc registered only when `!wants_escape()`, `FormatCommand::ALL` that `applies`, non-emoji triggers dismissed each frame.
- Layout: thread notes first in the column (spanning the card width), then `AddThreadNote` (a 32px dashed row reading `+ Add thread note`, hidden while an editor is open); message notes after their message's entry, indented by 28px; margins 8px between a message and its notes. A note's message that is no longer in the thread sorts by `message_date` among the messages.
- Autosave: 1s after the last change the editor's content goes out as `save_note_draft`. On `set`, a draft with no open editor for this thread reopens its editor (recovery).
- Leaving (J/K to another conversation, Esc out of the thread, the thread closing, the window closing) calls `close`.

- [ ] **Step 1:** Tests in `crates/exo/tests/notes.rs`: `t_opens_a_thread_note_and_command_enter_saves_it` (Enter, `T`, type, `⌘↩` → one note, label `Thread note`, text matches, editor closed); `shift_t_notes_the_focused_message` (label `Note 1`, `message_id` is the focused message's); `escape_keeps_what_was_typed`; `an_empty_note_is_dropped`; `typing_in_a_note_does_not_trigger_shortcuts` (type `e#j`; thread still open, not done, not trashed); `editing_a_note_adds_a_version`; `deleting_a_note_removes_it_from_the_thread`; `leaving_the_thread_mid_note_keeps_the_text` (type, `Escape`×2 or `J`; come back; the note or its draft holds the text); `mail_arriving_mid_note_keeps_the_editor` (deliver a message on the mock, `sync`, editor text unchanged); `a_draft_is_recovered_after_a_relaunch` (`type_text`, `wait(1.5)`, `relaunch`, open the thread, editor text matches); `notes_sit_under_their_message` (rect of Note 1 is below its message's card and above the next one's).
- [ ] **Step 2:** Run; they fail.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p exo --test notes --test thread --test compose` pass.
- [ ] **Step 5:** Commit `feat: notes in the thread`.

### Task 5: The notes pane, pane-only mode, the setting, the inbox icon

**Files:**
- Create: `crates/exo/src/notes/pane.rs`
- Modify: `crates/exo/src/lib.rs` (right pane `None =>` arm ~:2614: a 40px tab row `Contact | Notes · N` when a thread is open, then `contact_pane::show` or `notes::pane::show` in the rest), `commands.rs` (`Action::{ToggleNotesPane, SetNotesPlacement(NotesPlacement), ShareNotes}`; `⌘⇧N`; palette rows `Add Thread Note`, `Add Note to Message`, `Show Notes`, `Show Notes in Thread`, `Show Notes in Pane Only`), `settings/menu.rs` + `settings/mod.rs` (a `Notes` row opening a two-radio dialog, as `settings/theme.rs`), `inbox/row.rs::metadata` (icon `legacy/DEPRECATED_notes` in `note_label_default` when `thread.notes > 0`), `thread/mod.rs` + `thread/header.rs` (pane mode: no note entries in the column; an amber count chip after the subject and after a message's sender), device settings (the remembered tab)
- Test: `crates/exo/tests/notes.rs`

**Interfaces:**
- `notes::pane::show(ui, pane: Rect, rem: f32, notes: &mut Notes, detail: Option<&ThreadDetail>, now) -> Option<PaneAction>`; `PaneAction::{Reveal(String) /* note id */, RevealMessage(String), Start(Target), Card(String, CardAction), Share, RestoreDeleted(String)}`.
- Thread mode: a timeline (amber dot entries: label, date, two-line excerpt; grey dot entries for emails: `✉ {sender} · {date}`), in time order. Clicking a note entry scrolls the thread to it (`ThreadView::reveal_note`) and flashes it for 1s in `note_flash_default`.
- Pane mode: full cards in time order, the editor inside the pane, `+ Add thread note`.
- Foot of both: a `Share notes by email` button (disabled look until Task 9 wires it; clicking shows the toast `Sharing arrives with the next update.` — replaced in Task 9).
- `Mail::notes_tab() -> bool` and `Mail::notes_placement()` test accessors.

- [ ] **Step 1:** Tests: `command_shift_n_switches_the_pane_to_notes_and_back`; `the_timeline_lists_notes_and_emails_in_order`; `clicking_a_timeline_entry_reveals_the_note`; `pane_only_mode_keeps_notes_out_of_the_thread` (`start_with_settings(.., |s| s.notes_placement = Pane)`; `note_rects()` all inside the right pane; T opens the editor in the pane); `the_command_switches_placement`; `a_thread_with_notes_is_marked_in_the_inbox` (`app.mail().inbox().threads()[i].notes == 1`).
- [ ] **Step 2:** Run; they fail. **Step 3:** Implement. **Step 4:** `cargo test --workspace > log` passes. **Step 5:** Commit `feat: the notes pane and its setting`.

## Stage 2: images and files

### Task 6: Inline images and attachments in notes

**Files:**
- Create: `crates/exo/src/notes/files.rs`
- Modify: `notes/editor.rs`, `notes/card.rs`, `crates/exo/src/compose/images.rs` (make `decode` reachable: `pub(crate)` re-export), `lib.rs` (routing paste, drop and pick to the note editor when it has focus)
- Test: `crates/exo/tests/notes.rs`, unit tests in `notes/files.rs`

**Interfaces:**
- `pub const MAX_FILE: u64 = 100 * 1024 * 1024;`
- `pub fn add_file(folder: &Path, name: &str, mime: &str, bytes: &[u8], content_id: Option<String>) -> Result<NoteFile, Refused>`; `pub fn add_path(folder: &Path, path: &Path) -> Result<NoteFile, Refused>`; `pub enum Refused { TooLarge, Empty, Unreadable }` with toasts `That file is over 100 MB.`, `That file is empty.`, `Couldn't read that file.`
- Bytes at `account.note_files().join(uuid)`. Inline images go in with `editor.insert_image(&cid)` and are loaded for editors and cards from that folder through the `image_requests`/`set_image` loop, decoding off the UI thread. `take_uploads()` is drained each frame (pasted `data:` images).
- Listed files show as `exo_ui::paint_attachment_card` chips under a note; clicking one opens the file with the system (`open::that`'s existing equivalent in `downloads.rs`).
- The editor's `Editing.files` feeds `SaveNote.files` and `NoteDraft.files`.

- [ ] **Step 1:** Tests: `a_dropped_file_is_listed_under_the_note_and_saved` (drop a temp file via the fixture's dropped-files input, `⌘↩`; `note.files[0].name` matches, bytes exist at `note_files()/uuid`); `a_pasted_image_goes_inline` (html contains `src-cid`; a file row has `content_id`); `an_empty_file_is_refused_with_a_toast`; unit tests `a_file_over_the_limit_is_refused` (a sparse 100 MB + 1 byte file by `set_len`) and `an_undecodable_image_is_kept_as_a_file`.
- [ ] **Step 2–4:** Fail, implement, `cargo test -p exo --test notes` passes. **Step 5:** Commit `feat: images and files in notes`.

## Stage 3: search

### Task 7: `notes:` in the store's search

**Files:**
- Modify: `crates/exo-store/src/query.rs` (`Kind::Notes`; `parse_leaf`: `notes:`/`note:` with or without a value, `has:notes`/`has:note`; arms in `fts_token`, `gmail_query` (empty), `Matcher::find_token`, `Matcher::excludes`, `invalid_tokens`, `last_token`), `search.rs`, `notes.rs`
- Test: `crates/exo-store/tests/search.rs`, unit tests in `query.rs`

**Interfaces:**
- `pub struct NoteHit { pub note_id: String, pub label: String, pub date: Timestamp, pub snippet: Highlighted }`; `SearchResult.notes: Vec<NoteHit>`; `SearchResult.deleted: bool` (a row built from `note_threads` because the mail is gone).
- `Fts.notes: Vec<(bool /* negated */, String /* fts expr; empty = any note */)>`.
- A `notes:` term adds `AND thread_id [NOT] IN (SELECT thread_id FROM notes_fts WHERE notes_fts MATCH ?)`; with an empty value, `IN (SELECT thread_id FROM notes WHERE deleted_at IS NULL)`. With any `notes:` term the All-Mail date bound and the Gmail search are skipped.
- A bare text term matches when `search_fts` matches it or `notes_fts` does (per-term `rowid IN (…) OR thread_id IN (…)`), and `recheck()` also reads the thread's note text.
- Threads with matching notes and no `search_fts` row are unioned in from `note_threads` as `deleted` results whose `ThreadSummary` carries the saved subject, participants and `last_date`.
- Note values reach FTS5 only through the existing escaping (`fts5_prefix_or_phrase`), so `*` is a prefix and quotes a phrase, as for mail.

- [ ] **Step 1:** Tests in `tests/search.rs`: `notes_terms_find_threads_by_their_notes` (`notes:overage` finds the noted thread only, with one `NoteHit` whose bold is `["overage"]`); `a_bare_notes_term_lists_every_noted_thread`; `has_notes_is_the_same`; `plain_words_match_mail_or_notes` (a word only in a note finds the thread, with a hit; a word only in mail finds it with none); `notes_and_plain_words_combine` (`notes:slack renewal` with `renewal` only in the mail body matches); `from_combines_with_notes`; `a_phrase_matches_within_one_note`; `a_prefix_star_matches`; `a_deleted_note_is_not_found`; `negated_notes_excludes`; `fts_syntax_in_a_notes_term_is_harmless` (each of `notes:"`, `notes:a-b`, `notes:*`, `notes:OR`, `notes:(` returns `Ok`); `notes_on_a_deleted_thread_are_still_found` (`deleted == true`, subject from the context). Unit tests in `query.rs`: `notes_tokens_never_reach_gmail` (`to_gmail` of `notes:x from:a` is `from:a`), `a_split_refuses_notes_tokens`.
- [ ] **Step 2–4:** Fail, implement, `cargo test -p exo-store` passes. **Step 5:** Commit `feat: search finds notes`.

### Task 8: Search in the app

**Files:**
- Modify: `crates/exo/src/search/mod.rs` and its row drawing (up to two amber lines per result: icon, `{label} · {date} — {snippet}` with matches on `note_mark_default`, then `+N more`), `search/autocomplete.rs` (`notes:`, `has:notes` suggestions), `mailbox/mod.rs::search_result`, `lib.rs` (opening a result reveals the first hit's note and passes the query's terms to `Notes` for highlighting; a `deleted` result opens a notes-only view: subject, `Email deleted` in `ink(0.4, 0.4)`, then the notes), `notes/card.rs` (highlighting)
- Test: `crates/exo/tests/search.rs`, `tests/notes.rs`

**Interfaces:**
- `Notes::highlight(&mut self, terms: Vec<String>)`; cleared when the thread is opened from anywhere but a search.
- `Mail::search_note_hits(row: usize) -> Vec<String>` test accessor (the hit lines' text).

- [ ] **Step 1:** Tests: `a_notes_search_shows_the_matching_note_under_the_row`; `opening_a_result_reveals_the_note`; `autocomplete_offers_notes`; `a_note_on_a_deleted_thread_opens_alone` (delete the thread on the mock, sync, search `notes:…`, Enter → notes-only view with the note); `a_note_whose_message_is_gone_stays_in_place` (delete one message on the mock; label line reads `on a deleted email from …`).
- [ ] **Step 2–4:** Fail, implement, `cargo test --workspace > log` passes. **Step 5:** Commit `feat: notes in search results`.

## Stage 4: share

### Task 9: Share notes by email

**Files:**
- Create: `crates/exo/src/notes/share.rs`
- Modify: `crates/exo/src/compose/popout.rs` or `compose/mod.rs` (`compose_share_popped`), `compose/files.rs` (a named-file add), `lib.rs` (running `Action::ShareNotes`), `notes/pane.rs` (the button)
- Test: `crates/exo/tests/notes.rs`, unit tests in `share.rs`

**Interfaces:**
- `pub struct Shared { pub subject: String, pub html: String, pub images: Vec<NoteFile>, pub files: Vec<NoteFile> }`
- `pub fn build(context: &NoteThread, notes: &[Note], me: &str) -> Shared`: subject `Notes: {subject}`; body = `<div>Notes on “{subject}” ({n} emails with {names}, {first date} to {last date}).</div>`, then per live note in time order `<div><b>{label}</b></div><div>{date} · {where}</div>` and its html, separated by `<hr>`. `{where}` is `whole thread`, `on {first name}'s email of {date}`, `on my reply of {date}`, or `on a deleted email from {first name} of {date}`. Text is HTML-escaped; note html goes in as it is.
- `Mail::compose_share_popped(&mut self, shared: Shared, now)`: `factory::compose` with `Fields { subject, body, .. }`, each image through `files::add_inline` with its existing content id, each file copied into the draft's files and attached while `exceeds_gmail_limit` allows, the rest appended to the body under `<div><b>Not attached (too large)</b></div>` as a list of names; then `touch`, `with_auto_bcc`, push, `pop_out`.
- With no live notes the action shows the toast `No notes to share.`

- [ ] **Step 1:** Tests: unit `the_shared_body_names_the_thread_and_each_note`, `note_text_in_labels_is_escaped`, `where_lines_cover_each_case`; end to end `share_opens_a_draft_with_the_notes` (pane button or ⌘K `Share Notes by Email`; a popped-out draft exists with subject `Notes: …`, body containing each note's text, no recipients; the thread's notes unchanged); `shared_files_ride_along_and_oversized_ones_are_listed` (one small file attached; one 40 MB file listed under `Not attached (too large)`); `sharing_nothing_says_so`.
- [ ] **Step 2–4:** Fail, implement, `cargo test --workspace > log` passes. **Step 5:** Commit `feat: share notes by email`.

## Stage 5: history and the Markdown folder

### Task 10: History and deleted notes

**Files:**
- Create: `crates/exo/src/notes/history.rs`
- Modify: `notes/mod.rs`, `notes/card.rs`, `notes/pane.rs`, `lib.rs`
- Test: `crates/exo/tests/notes.rs`

**Interfaces:**
- `History` on a card posts `account.note_versions(id)`; `Event::NoteVersions` opens the history in place of the card: a list of rows `{date time} · {Created|Edited|Deleted|Restored}` newest first, the selected version's body read-only under it, `Restore this version` (hidden on the newest) and `Close`. Keys: `↑`/`↓` select, `Esc` closes.
- The pane's foot shows `Deleted notes ({n})` when there are any; expanding lists them with their label, date and `Restore`.
- `Mail::note_history() -> Option<Vec<(u32, NoteChange)>>` test accessor.

- [ ] **Step 1:** Tests: `history_lists_every_save_newest_first`; `restoring_a_version_brings_its_text_back_as_a_new_version`; `a_deleted_note_can_be_restored_from_the_pane`.
- [ ] **Step 2–4:** Fail, implement, pass. **Step 5:** Commit `feat: note history`.

### Task 11: The Markdown folder

**Files:**
- Create: `crates/exo/src/notes/markdown.rs`
- Modify: `crates/exo/src/copy/turndown.rs` + `copy/mod.rs` (visibility `pub(crate)` for `Tree::parse` and `turndown`), device settings in `crates/exo-store/src/settings.rs` (`notes_folder: Option<String>`; `None` = off), `settings/` (in the Notes dialog: a `Keep a Markdown copy` toggle and the folder's path with `Choose…`), `commands.rs` (`Rebuild Notes Folder`), `account.rs` (a store call returning every thread's notes and versions: `Store::all_notes() -> Result<Vec<(NoteThread, Vec<Note>, Vec<(String, NoteVersion)>)>>`, added to `exo-store/src/notes.rs`), `lib.rs` (export after a note change when the folder is set)
- Test: unit tests in `markdown.rs`, `crates/exo/tests/notes.rs`

**Interfaces:**
- `pub fn default_folder(account: &str) -> PathBuf` = `~/Documents/Email Notes/{account}`.
- `pub fn file_stem(context: &NoteThread) -> String`: the subject lowercased, non-alphanumerics to `-`, runs collapsed, trimmed, at most 60 chars, then `-{thread_id}`; `notes-{thread_id}` when the subject gives nothing.
- `pub fn render(context: &NoteThread, notes: &[Note]) -> String`: YAML front matter (`thread_id`, `subject`, `participants` as `Name <email>` list, `first_date`, `last_date`, `updated`), then per live note `## {label}` and a list (`- id:`, `- kind:`, `- created:`, `- edited:`, `- on:`), a blank line and the note's Markdown. Inline images become `![{name}](files/{thread_id}/{uuid}-{name})`; listed files a `Files:` list of the same links. YAML strings are double-quoted with `"` and `\` escaped.
- `pub fn history_line(note_id: &str, version: &NoteVersion) -> String`: one JSON object per line (`note_id`, `version`, `saved_at`, `change`, `text`, `html`, `files`).
- `pub fn export(folder: &Path, files: &Path, context: &NoteThread, notes: &[Note], versions: &[(String, NoteVersion)]) -> io::Result<()>`: writes `{stem}.md` and `{stem}.history.jsonl` through temp files and `rename`, copies missing files to `files/{thread_id}/`, removes the thread's `.md` when it has no live notes (history stays).
- Export runs off the UI thread; an error shows the toast `Couldn't write the notes folder.` once per session.

- [ ] **Step 1:** Tests: unit `the_stem_is_safe_for_any_subject` (`"Re: Q4/Plan: “final”?"`, `""`, 200 chars, `../..`); `rendering_has_front_matter_and_a_section_per_note`; `yaml_strings_are_escaped`; `formatting_survives_as_markdown` (bold, list, link); `a_history_line_is_valid_json`; end to end `saving_a_note_writes_its_markdown_file_when_the_folder_is_on` (folder = a temp dir via `start_with_settings`' device equivalent) and `rebuild_writes_every_thread`.
- [ ] **Step 2–4:** Fail, implement, pass. **Step 5:** Commit `feat: a Markdown copy of notes`.

### Task 12: Finish

- [ ] **Step 1:** `cargo fmt --all`, `cargo clippy --workspace --all-targets 2>&1 | grep -c warning` no higher than on `main`.
- [ ] **Step 2:** `cargo test --workspace > log 2>&1`; zero failures.
- [ ] **Step 3:** `RBENV_VERSION=4.0.10 bundle exec fastlane build`; install to `/Applications/Email.app` only after the running app quits on request (never force-quit).
- [ ] **Step 4:** Report to the user what to test, per stage.
