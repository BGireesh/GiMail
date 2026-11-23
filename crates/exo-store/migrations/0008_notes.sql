-- Thread notes, which Gmail has no record of, and their search index.
CREATE TABLE "note_threads" (
    "thread_id" TEXT NOT NULL,
    "subject" TEXT NOT NULL,
    "participants" TEXT NOT NULL,
    "first_date" TEXT,
    "last_date" TEXT,
    "messages" INTEGER NOT NULL,
    "next_number" INTEGER NOT NULL,
    PRIMARY KEY ("thread_id")
);
-- #[toasty::breakpoint]
CREATE TABLE "notes" (
    "id" TEXT NOT NULL,
    "thread_id" TEXT NOT NULL,
    "message_id" TEXT,
    "message_from" TEXT,
    "message_date" TEXT,
    "number" INTEGER,
    "kind" TEXT NOT NULL,
    "html" TEXT NOT NULL,
    "text" TEXT NOT NULL,
    "files" TEXT NOT NULL,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL,
    "deleted_at" TEXT,
    "version" INTEGER NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_notes_by_thread_id" ON "notes" ("thread_id");
-- #[toasty::breakpoint]
CREATE TABLE "note_versions" (
    "note_id" TEXT NOT NULL,
    "version" INTEGER NOT NULL,
    "html" TEXT NOT NULL,
    "text" TEXT NOT NULL,
    "files" TEXT NOT NULL,
    "saved_at" TEXT NOT NULL,
    "change" TEXT NOT NULL,
    PRIMARY KEY ("note_id", "version")
);
-- #[toasty::breakpoint]
CREATE TABLE "note_files" (
    "uuid" TEXT NOT NULL,
    "note_id" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "mime_type" TEXT NOT NULL,
    "size" INTEGER NOT NULL,
    "content_id" TEXT,
    PRIMARY KEY ("uuid")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_note_files_by_note_id" ON "note_files" ("note_id");
-- #[toasty::breakpoint]
CREATE TABLE "note_drafts" (
    "id" TEXT NOT NULL,
    "thread_id" TEXT NOT NULL,
    "message_id" TEXT,
    "note_id" TEXT,
    "html" TEXT NOT NULL,
    "text" TEXT NOT NULL,
    "files" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_note_drafts_by_thread_id" ON "note_drafts" ("thread_id");
-- #[toasty::breakpoint]
CREATE VIRTUAL TABLE notes_fts USING fts5(note_id UNINDEXED, thread_id UNINDEXED, text, files, tokenize = "porter ascii tokenchars '_'");
