# Thread notes: design

Date: 2026-11-23. Status: awaiting approval.

## Intent

Private notes attached to email threads, so the context that builds up around a conversation
(contract values, internal findings, meeting notes, reminders to self) lives beside the mail
instead of in another app. The thread view's spare space carries them.

Example: a vendor opens a renewal thread. The user notes the last contract value on the
vendor's first email, replies, later notes what the team found, then takes meeting notes after a
call. Weeks later they find it all with `/` and `notes:renewal`, and send the notes to a
colleague as one formatted email.

## Goals

- Several notes per thread: on the whole thread, or on one message of it.
- A what-you-see-is-what-you-get editor with no length limit, inline images and file
  attachments, built from the app's own Rust editor. No new third-party dependency.
- Notes shown in the thread, or only in the right pane, by a setting. A timeline pane either way.
- Search with `/`: `notes:` terms, and plain words that match mail and notes together.
- Share: one action opens a compose window holding the notes, formatted, with the thread
  referenced, ready to send.
- Every save kept as a version, with any past version viewable and restorable.
- AI-ready storage: stable ids, plain text beside the formatted text, a `kind` field, the
  thread's context saved with the notes, full history, and an optional folder of Markdown files
  any local tool can index.
- Notes survive Clear Cache, sign-out, app upgrades, eviction and the thread's deletion in Gmail.

## Non-goals (v1)

- Syncing notes to Gmail or another Mac. v1 is this Mac only. A later version may sync through
  Gmail, which is why notes live in the mail database (see Approaches).
- AI features themselves. v1 only stores notes so a later local model can read notes, threads
  and attachments.
- Tasks (checkbox, due date). `kind` is stored now so they can be added.
- Notes not tied to a thread. Notes on a draft. Sharing with live collaboration.
- Typo-tolerant or semantic search.

## Anti-goals

- A note leaving the Mac by any path but the user's own Share (or the Markdown folder they
  turned on). Notes never go into a reply, a forward, a print of the thread or Gmail.
- A note that can be mistaken for part of an email.
- Notes lost to anything the app does to its cache.

## Constraints

- The stack as it is: egui UI, `exo-editor` for rich text, toasty over SQLite with hand-written
  migrations (`crates/exo-store/src/migrations.rs` says how), FTS5 for search.
- The mail database is mostly a cache of Gmail. `Store::clear_cache` rebuilds it, keeping
  `KEPT_TABLES` (`db.rs`). `Store::delete` removes the file and the `.files` and `.parts`
  folders at sign-out. Eviction lets old conversations go.
- Both themes (Carbon, Snow) and the app's existing style constants.
- The user's git rules: `gireesh/` branches, nothing merged to `main` without approval.

## Approaches

- **A. A separate notes database per account.** Safe from the cache by construction, but search
  spans two databases and a later Gmail sync can't share a transaction with the outbox.
- **B. Notes as tables in the mail database (chosen).** One database, one query for search, and
  a later Gmail sync can queue its changes in the same transaction as the note. The user chose it
  for that single, maintainable home. Its risk, the cache's deletions, is covered by the
  safeguards below.
- **C. Markdown files as the source of truth.** Rejected: the editor's model is HTML, Markdown
  can't hold it all, and files changing under the app is a class of bugs v1 doesn't need. The
  readable folder survives as a one-way mirror.

## Decisions made with the user

- Local only in v1; approach B with the two safeguards.
- Both placements exist, chosen by a setting; default is in the thread.
- Numbering: notes on a message are `Note 1`, `Note 2`… per thread in the order written. Numbers
  are never reused, so deleting Note 2 leaves Note 3 as it is. Notes on the whole thread are
  labelled `Thread note`.
- `notes:` in search, combinable with every other filter; plain words search mail and notes.
- Markdown folder: wanted, as an optional setting, off at first.
- History: every save, time travel for people and for AI.
- `kind` stored now; tasks later.
- Notes stay when their thread is deleted.
- Every note belongs to a thread.
- A later AI reads notes, the thread and its attachments.

## Data model

New tables, made by one migration and added to `KEPT_TABLES`:

- `note_threads`: `thread_id` (key), `subject`, `participants` (JSON of names and addresses),
  `first_date`, `last_date`, `next_number`. The thread's context as of the last note change, so
  notes make sense when the mail isn't stored.
- `notes`: `id` (UUID v4, key), `thread_id`, `message_id` (null for a thread note),
  `message_from`, `message_date` (the message's context, saved with it), `number` (null for a
  thread note), `kind` (text, `note` for now), `html`, `text` (plain, derived from `html`),
  `created_at`, `updated_at`, `deleted_at` (null unless deleted), `version` (count of saves).
- `note_versions`: `note_id`, `version`, `html`, `text`, `files` (JSON list of file uuids),
  `saved_at`, `change` (`created`, `edited`, `deleted` or `restored`). Key `(note_id, version)`.
  Full copies, not diffs.
- `note_files`: `uuid` (key), `note_id`, `name`, `mime_type`, `size`, `content_id` (set for an
  inline image, as drafts do). Bytes at `{account}.notes/{uuid}`, a folder `Store::delete`
  doesn't touch. A file's row and bytes stay while any version lists it.
- `notes_fts`: FTS5 over `text` and file names, one row per live note, with the mail index's
  tokenizer. Rebuilt from `notes` whenever it's missing (a new file after Clear Cache).

- `note_drafts`: `id` (key: the note's id, or a new UUID for a note not saved yet), `thread_id`,
  `message_id`, `html`, `files`, `updated_at`. What's typed in an open editor and not saved yet.
  Kept across Clear Cache too.

Deleting a note sets `deleted_at` and writes a `deleted` version. Nothing is removed from
`notes` or `note_versions` in v1.

Unsaved text never counts as the note: it isn't searched, mirrored to Markdown or counted by the
inbox icon. A new note gets its row and its number at its first save. If the app quits with an
editor open, the thread next opens with that editor open again on the recovered text.

A note's `html` is the editor's HTML, the same dialect drafts save.

## Safeguards

1. **Clear Cache.** The notes tables are in `KEPT_TABLES`. `tests/clear_cache.rs` proves notes,
   versions, drafts and files come through, and that search finds a note afterwards.
2. **A standing backup.** After every note save, delete or restore, the notes tables are written
   to `{account}.notes-backup.sqlite3` beside the store (off the UI thread, the latest write
   winning). `Store::delete` leaves it and `{account}.notes/` alone, so sign-out keeps both.
   Opening a store whose notes tables are empty while the backup has notes restores from it.
   This covers sign-out, and a Clear Cache whose old database can't be read.
3. **Eviction.** A thread with a live note is never evicted, rows or bodies, as the open
   conversation isn't. So everything about it stays searchable locally.
4. **Deleted in Gmail.** When sync removes a thread, its notes stay. Search still finds them,
   with a row built from `note_threads` and marked `Email deleted`. Opening it shows the notes
   alone. When only a note's message is removed and the thread remains, the note keeps its
   number and sits where the message was by date, labelled `on a deleted email from {sender} of
   {date}`.

## Interface

A note is amber, in both themes, so it can't be read as mail: an amber bar on its left, an amber
label (`Note 2` or `Thread note`), then where and when (`on Dana's email of 3 Nov · 6 Nov`). New
palette entries in `exo-ui` carry the colors.

### In the thread (setting: In thread)

- `+ Add thread note` is pinned at the foot of the thread's header, so it stays in place while
  the thread scrolls (also while a note editor is open, when it works as `T` does). Thread notes
  sit under it, above the first message, and scroll with the messages.
- A message's notes sit under its card, indented, whether the card is folded or open.
- Pointing at a message shows `+ Note`. Pointing at a note shows `Edit`, `Delete`, `History`.
- The editor opens in place, where the note is or will be.

### In the pane only (setting: In pane)

- The thread is as today, plus a small amber count on the subject and on each message that has
  notes. Clicking one opens the pane.
- The pane shows the notes as full cards in time order, and the editor opens there.

### The notes pane

- The right pane gets two tabs: `Contact` and `Notes · N`.
- In thread mode it's a timeline: one compact entry per note, between small markers for the
  emails, in time order. Clicking an entry scrolls the thread to it and flashes it.
- `Share notes by email` at its foot.
- The pane remembers which tab was last shown.

### Inbox rows

A thread with live notes shows a small amber note icon in its row.

### Editor

`exo_editor::Editor`, as compose uses it: bold, italic, underline, lists, links, quotes, inline
images (paste, drop or pick), and attached files listed as chips under the text. The emoji
trigger stays; snippets and mentions are off. One file may be up to 100 MB.

- `⌘↩` saves and closes. `Esc` also closes and keeps what was typed (a version can undo it). An
  empty new note is dropped.
- While the editor is open, what's typed is saved as it changes, so a crash loses nothing. One
  version is written when the editor closes with changes, not one per keystroke.

### Keys and commands

`N` and `⇧N` are taken (next message, show new messages), so:

- `T`: new thread note. `⇧T`: new note on the focused message.
- `⌘⇧N`: switch the right pane between its Notes and Contact tabs. The pane itself is always
  there, as today. The Notes tab exists only while a thread is open.
- In ⌘K: `Add Thread Note`, `Add Note to Message`, `Show Notes`, `Share Notes by Email`,
  `Show Notes in Thread`, `Show Notes in Pane Only`.

### Setting

`Notes: In thread | In pane only`, in the account's settings document, default In thread.

## Search

- `notes:word` and `notes:"a phrase"`: threads with a live note matching it. `notes:` alone, or
  `has:notes`: threads with any live note.
- Plain words match mail or notes. In `notes:slack renewal`, `slack` must be in a note and
  `renewal` may be in a note or the mail.
- Note text is matched by the rules mail already has: whole words with stemming (so `renew`
  finds `renewal`), `*` for a prefix, quotes for a phrase. One word means the same in mail and in
  notes.
- `notes:` terms are matched locally only, and aren't sent to Gmail's search. Threads with notes
  are stored whole (safeguard 3), so the local match of a query with a `notes:` term is
  complete. A query of plain words alone runs as today, with notes as one more place a local
  match can be found.
- A result whose notes match shows, under its usual row, up to two amber lines: the note's label,
  its date and a snippet with the match highlighted, then `+N more`.
- Opening a result scrolls to the first matching note and highlights the matches.
- Autocomplete offers `notes:` and `has:notes`.

## Share

`Share notes by email` opens a new pop-out compose with:

- Subject `Notes: {thread subject}`. No recipients.
- Body: a line naming the thread (subject, who it's with, its dates, how many emails), then each
  live note in time order as a heading (its label), a line with its date and what it's on, and
  its content. Inline images go as the draft's inline images, files as its attachments.
- It's an ordinary draft from there: editable, sendable, discardable. The notes aren't changed
  by it, and the thread's emails aren't included.
- Files go through compose's own attach path, in note order. Those that would take the message
  past Gmail's size limit are left out, and the body lists them by name under `Not attached (too
  large)`.

## History

- Recorded from stage 1.
- `History` on a note opens its versions, newest first, each with its time and change. Selecting
  one shows it read-only, with `Restore this version`, which saves it as a new version.
- `Deleted notes` at the foot of the notes pane lists the thread's deleted notes, each with
  `Restore`.

## Markdown folder

A setting, off by default, with a folder (default `~/Documents/Email Notes/{account}/`).

- One file per thread, `{subject-slug}-{thread_id}.md`: YAML front matter (thread id, subject,
  participants, dates), then one section per live note with its id, kind, label, created and
  edited times, the message it's on, and its content as Markdown.
- Beside it, `{same name}.history.jsonl`: one line per version of every note, deleted ones
  included.
- Files and images are copied to `files/{thread_id}/` and linked relatively.
- One-way: the app rewrites a thread's files whenever its notes change, and edits made to the
  files are overwritten. A command rebuilds the whole folder.

## Stages

Each is installable and useful on its own, on its own `gireesh/` branch.

1. Tables, migration, safeguards 1 to 3, notes in the thread and the pane, the editor (text and
   formatting), the setting, the keys, the inbox icon, versions recorded.
2. Inline images and file attachments.
3. Search, and safeguard 4.
4. Share by email.
5. The Markdown folder, the History view and Deleted notes.

## Left to the builder

- Exact sizes, spacing and the amber values, following the prototype and the app's constants.
- How notes take part in keyboard focus inside the thread beyond the keys above.
- The store's API shape and how the UI is told of note changes, following drafts and reminders.
- HTML to Markdown conversion details.
- Test layout, following `crates/exo-store/tests` and `crates/exo/tests` (gmail-mock, end to
  end). Every stage ships with tests.

## Reference

Clickable prototypes (throwaway): `.superpowers/brainstorm/38499-1795439961/content/`
(`notes-prototype.html` for the thread, pane, editor and share; `notes-search.html` for search).
