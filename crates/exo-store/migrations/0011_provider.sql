-- Which provider's account a store holds, so it's never opened as another's. Every store an
-- earlier Exo wrote holds a Gmail account; a new file has nothing yet, and records its provider
-- when it first opens.
INSERT INTO "kv" ("key", "value") SELECT 'provider', '"gmail"' WHERE EXISTS (SELECT 1 FROM "kv") OR EXISTS (SELECT 1 FROM "sync_cursors") OR EXISTS (SELECT 1 FROM "settings") OR EXISTS (SELECT 1 FROM "threads") OR EXISTS (SELECT 1 FROM "drafts") OR EXISTS (SELECT 1 FROM "outbox");
