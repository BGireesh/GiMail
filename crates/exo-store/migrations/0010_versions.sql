-- Names for what the columns hold with any provider: a draft's id there, and the version
-- (Gmail's `historyId`) by which the newest copy of a thread or message wins.
ALTER TABLE "drafts" RENAME COLUMN "gmail_id" TO "draft_id";
-- #[toasty::breakpoint]
ALTER TABLE "threads" RENAME COLUMN "history_id" TO "version";
-- #[toasty::breakpoint]
ALTER TABLE "messages" RENAME COLUMN "history_id" TO "version";
