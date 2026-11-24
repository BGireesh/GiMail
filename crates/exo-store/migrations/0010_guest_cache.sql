-- A colleague's calendar as Exo last fetched it, one row per person, for showing their day beside
-- the user's own. Gmail doesn't keep it for Exo, so Clear Cache and Restart refills it.
CREATE TABLE "guest_cache" (
    "person" TEXT NOT NULL,
    "fetched_at" BIGINT NOT NULL,
    "from_ms" BIGINT NOT NULL,
    "to_ms" BIGINT NOT NULL,
    "events" TEXT NOT NULL,
    PRIMARY KEY ("person")
);
