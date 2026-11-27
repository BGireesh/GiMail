-- Where sync has read the provider's changes up to, a token per scope. Gmail's one scope is the
-- mailbox (''), whose token is the history id that `kv` held until now.
CREATE TABLE "sync_cursors" ("scope" TEXT NOT NULL, "token" TEXT NOT NULL, PRIMARY KEY ("scope"));
-- #[toasty::breakpoint]
INSERT INTO "sync_cursors" ("scope", "token") SELECT '', "value" FROM "kv" WHERE "key" = 'history_id';
-- #[toasty::breakpoint]
DELETE FROM "kv" WHERE "key" = 'history_id';
