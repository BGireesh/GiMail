-- Index each event's series master id so deleting or folding a series queries only its rows.
ALTER TABLE "events" ADD COLUMN "series_id" TEXT;
-- #[toasty::breakpoint]
CREATE INDEX "index_events_by_calendar_id_and_series_id" ON "events" ("calendar_id", "series_id");
-- #[toasty::breakpoint]
UPDATE "events" SET "series_id" = json_extract("event", '$.recurringEventId') WHERE json_extract("event", '$.recurringEventId') IS NOT NULL;
