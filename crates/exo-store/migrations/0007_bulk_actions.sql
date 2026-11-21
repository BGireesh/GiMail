-- The bulk actions of the last week, which Gmail has no record of.
CREATE TABLE "bulk_actions" ("id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT, "started_at" BIGINT NOT NULL, "record" TEXT NOT NULL, "changed" TEXT NOT NULL);
