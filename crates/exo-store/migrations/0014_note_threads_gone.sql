-- Whether Gmail has said a noted thread is gone, so its notes say `Email deleted` only then.
ALTER TABLE "note_threads" ADD COLUMN "gone" BOOLEAN NOT NULL DEFAULT 0;
