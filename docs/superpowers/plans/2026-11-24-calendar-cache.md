# Calendar Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Colleagues listed in Settings → Calendar cache have their next 7 weekdays' events kept in Exo's local database, refreshed on a schedule while Exo runs, so Meet (and the other guest overlays) show them at once while the latest loads, with a spinner before their name.

**Architecture:** A one-row-per-person `guest_cache` table in exo-store behind three `Store` methods. Cache settings live in `CalendarSettings`. The calendar sync task (`crates/exo/src/calendar/sync.rs`) gets the app's `Clock` and the cache settings, runs the schedule on its existing loop, seeds `Data.guests` from the cache before a live read, and writes live answers back. A new `Marker::Refreshing` draws the spinner before the colour swatch.

**Tech Stack:** Rust, egui, tokio, toasty (SQLite), jiff, gmail-mock for tests.

**Spec:** `docs/superpowers/specs/2026-11-24-calendar-cache-design.md`

## Global Constraints

- Cache window: the next 7 days from today (today included), weekdays only (Monday–Friday). Weekend events are never stored.
- Refresh only while Exo runs; no background service. Refresh days are Monday–Friday only.
- Defaults: switch off; all five weekdays on; 8:00 AM to 6:00 PM; every **1 hour**. Frequency options exactly: 15 minutes, 30 minutes, 1 hour.
- Google's answer wins: events → replace the cache; busy-only blocks → replace with them; hidden (`Ok(None)`) → delete the person's cache; error/offline → keep the cache.
- Switch off → all cached rows deleted; a person removed → their row deleted.
- The cache is a cache: `guest_cache` is **not** in `KEPT_TABLES` (Clear Cache refills it).
- A refresh runs at most 4 reads at once, off the UI thread.
- Commits: `feat: …` / `fix: …` sentences; stage by path; never stage `fastlane/Fastfile`, `.bundle/`, `vendor/`; no Claude/Anthropic attribution.

## Review Focus

1. Switch on with nobody listed: no requests, no errors (test in Task 4).
2. "Between" with the end at or before the start (e.g. 6:00 PM to 8:00 AM): the To menu only offers times after From, and `due` treats such a range as never (tests in Tasks 2 and 3).
3. A person removed while a refresh is in flight: their answer isn't written back (test in Task 4).
4. Offline at refresh time: cache kept, no `Failed` shown for cached people in Meet (test in Task 5).
5. A cached person whose calendar is now hidden: cache deleted at refresh and on a live read (tests in Tasks 4 and 5).

---

### Task 1: The `guest_cache` table and its Store API

**Files:**
- Create: `crates/exo-store/migrations/0010_guest_cache.sql`
- Modify: `crates/exo-store/src/migrations.rs` (`FILES`, new last entry), `crates/exo-store/src/db.rs` (model), `crates/exo-store/src/calendar.rs` (methods), `crates/exo-store/src/lib.rs` (export `CachedGuest`)
- Test: `crates/exo-store/tests/calendar.rs`, `crates/exo-store/tests/clear_cache.rs` (`every_table_is_kept_or_refilled`: list `guest_cache` as refilled)

**Interfaces:**
- Produces (in `exo_store`):
  - `pub struct CachedGuest { pub fetched_at: Timestamp, pub from: Timestamp, pub to: Timestamp, pub events: Vec<gmail::calendar::Event> }` (`Debug, Clone, PartialEq`)
  - `impl Store { pub async fn cached_guest(&self, email: &str) -> Result<Option<CachedGuest>> }` (address matched case-insensitively; stored lower-cased)
  - `impl Store { pub async fn cache_guest(&self, email: &str, cached: &CachedGuest) -> Result<()> }` (replaces the person's row)
  - `impl Store { pub async fn forget_cached_guests(&self, keep: &[String]) -> Result<()> }` (deletes every row whose person isn't in `keep`; `&[]` deletes all)

Table: `guest_cache(person TEXT NOT NULL PRIMARY KEY, fetched_at BIGINT NOT NULL, "from" BIGINT NOT NULL, "to" BIGINT NOT NULL, events TEXT NOT NULL)` (ms timestamps; `events` as `Json<Vec<Event>>`, following `EventRow`). Follow the migration rules in `migrations.rs:16-60` (new random 63-bit id, appended last).

- [ ] **Step 1: Write the failing tests** in `crates/exo-store/tests/calendar.rs`:
  - `a_guest_s_cached_calendar_is_kept_across_a_restart`: `cache_guest("Priya@Brightmoor.co", &c)` with two events, then `harness.reopen()`, then `cached_guest("priya@brightmoor.co")` == `Some(c)`; a second `cache_guest` for the same person replaces it (one event now).
  - `forgetting_keeps_only_the_listed_guests`: cache priya and lena; `forget_cached_guests(&["lena@brightmoor.co".into()])` → priya `None`, lena `Some`; `forget_cached_guests(&[])` → both `None`.
  - In `clear_cache.rs`, add `guest_cache` to the refilled list; the existing test fails until the table exists.
- [ ] **Step 2: Run** `cargo test -p exo-store --test calendar cached --test clear_cache` → FAIL (missing methods / table).
- [ ] **Step 3: Implement** the migration, the `GuestCacheRow` model, `MigrationFile` entry, and the three methods (write pattern: `self.inner.begin(&mut db)` … `write.publish()`).
- [ ] **Step 4: Run** `cargo test -p exo-store` → all pass (including the migration upgrade test).
- [ ] **Step 5: Commit** `feat: guests' calendars can be cached in the store, one row per person`

### Task 2: Cache settings and the schedule rules

**Files:**
- Modify: `crates/exo-store/src/settings.rs` (`CalendarCache`, field on `CalendarSettings`)
- Create: `crates/exo/src/calendar/cache.rs` (pure rules; `mod cache;` in `calendar/mod.rs`)
- Test: unit tests in both files

**Interfaces:**
- Produces (in `exo_store`, exported):
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "camelCase", default)]
  pub struct CalendarCache {
      pub enabled: bool,            // false
      pub people: Vec<String>,      // lower-cased addresses, no duplicates
      pub days: [bool; 5],          // Monday..Friday, all true
      pub from_minute: u16,         // 480  (8:00 AM)
      pub to_minute: u16,           // 1080 (6:00 PM)
      pub every_minutes: u16,       // 60; only 15, 30 or 60
  }
  ```
  `CalendarSettings.cache: CalendarCache` with serde key `exoCalendarCache`, `skip_serializing_if` default (as `CalendarSettings::is_default` does).
- Produces (in `crate::calendar::cache`):
  - `pub fn window(today: Date) -> Vec<Date>` — today..today+6, weekdays only.
  - `pub fn window_range(today: Date, tz: &TimeZone) -> (Timestamp, Timestamp)` — start of today to start of today+7 in `tz`.
  - `pub fn keep_window(events: Vec<Event>, today: Date, tz: &TimeZone) -> Vec<Event>` — drops events whose start (date or dateTime, in `tz`) isn't on a `window` day.
  - `pub fn due(cache: &CalendarCache, now: &Zoned, last: Option<Timestamp>) -> bool` — `enabled`, `!people.is_empty()`, `now` on a selected weekday, `from_minute <= minute-of-day < to_minute` (false when `to_minute <= from_minute`), and `last` is `None` or at least `every_minutes` old.

- [ ] **Step 1: Write the failing tests**:
  - settings: `calendar_cache_defaults_and_its_key` — default values above; a settings JSON with `"calendar":{"exoCalendarCache":{"enabled":true,"people":["priya@brightmoor.co"],"everyMinutes":30}}` reads back with the other fields at their defaults; a default `CalendarCache` isn't serialized.
  - cache.rs: `the_window_is_the_next_seven_days_weekdays_only` — Thursday 2026-11-19 → `[11-19, 11-20, 11-23, 11-24, 11-25]`; Saturday 2026-11-21 → `[11-23 … 11-27]`.
  - `events_outside_the_window_are_dropped` — events on Fri 11-20 kept, Sat 11-21 dropped, 11-26 kept, 11-27 (today+8 from Thu) dropped, an all-day event on 11-23 kept.
  - `due_follows_the_days_hours_and_frequency` — with defaults + enabled + one person, New York: Tue 09:00, `last` None → true; `last` 30 min ago → false; 61 min ago → true; Tue 07:59 → false; Tue 18:00 → false; Sat 10:00 → false; Tuesday unticked → false; `people` empty → false; `from 1080, to 480` → false.
- [ ] **Step 2: Run** `cargo test -p exo-store --lib calendar_cache` and `cargo test -p exo --lib calendar::cache` → FAIL.
- [ ] **Step 3: Implement** the struct and the four functions.
- [ ] **Step 4: Run** both commands → PASS.
- [ ] **Step 5: Commit** `feat: the calendar cache's settings and its window and schedule rules`

### Task 3: Calendar Settings → Calendar cache section

**Files:**
- Modify: `crates/exo/src/calendar/options.rs` (section, `OptionsAction` variants, heights), `crates/exo/src/calendar/editing.rs` (`calendar_show_options` handling)
- Test: `crates/exo/tests/calendar.rs`

**Interfaces:**
- Consumes: `exo_store::CalendarCache` (Task 2); `crate::contact_picker::ContactPicker::show(&mut self, ui, id, input: DialogInput<'_>, line: Rect, book: Option<&contacts::Index>) -> Option<String>`; `exo_ui::paint_checkbox`; `exo_ui::paint_dialog_dropdown` + `exo_ui::Menu::show_in_dialog` (as `settings/reminders.rs:395-480` uses them).
- Produces: `OptionsAction::Cache(CacheChange)` with
  ```rust
  pub enum CacheChange { Enabled(bool), Add(String), Remove(String), Day(usize, bool), From(u16), To(u16), Every(u16) }
  ```
  Each is applied with `self.account.update_settings(|s| …)`. `Add` lower-cases and skips duplicates and invalid addresses (`compose::recipients::is_valid`). `From` that reaches or passes `To` moves `To` to `From + 30`.

Layout (after Meeting Links, in the dialog's existing style): checkbox row `Cache colleagues' calendars`; while on: `People` (ContactPicker field, then one row per person: name or address with ×), `Refresh on` (five 24px toggle buttons `M T W T F`, filled when on), `Between` (two dropdowns of times every 30 minutes from 12:00 AM to 11:30 PM, the second offering only times after the first), `Every` (dropdown `15 minutes`, `30 minutes`, `1 hour`).

- [ ] **Step 1: Write the failing test** `calendar_cache_settings_list_people_and_their_schedule`: open Calendar Settings (palette `Calendar Settings`), click the cache checkbox, type `priya` in People and press Enter, type `lena` and click the first suggestion, choose `30 minutes` in Every, click `F` → settings read `enabled: true`, `people: ["priya@brightmoor.co","lena@brightmoor.co"]`, `every_minutes: 30`, `days[4] == false`; click Priya's × → only lena; choose `5:00 PM` in the first Between menu → the second menu's first offer is `5:30 PM`, and choosing `7:00 PM` first then `6:00 PM` … is impossible (`to_minute > from_minute` always holds; picking a From at or after To moves To to From + 30); untick the checkbox → `enabled: false` (people kept in settings). Add test accessors only if needed (e.g. `calendar_options_rect(label)`), following the existing ones in `calendar/mod.rs`.
- [ ] **Step 2: Run** `cargo test -p exo --test calendar calendar_cache_settings` → FAIL.
- [ ] **Step 3: Implement** the section and actions.
- [ ] **Step 4: Run** `cargo test -p exo --test calendar` → PASS (existing Calendar Settings tests unchanged).
- [ ] **Step 5: Commit** `feat: Calendar Settings lists the colleagues to cache and when to refresh`

### Task 4: Scheduled refresh in the calendar task

**Files:**
- Modify: `crates/exo/src/calendar/sync.rs` (clock, cache settings channel, refresh, forgetting), the `CalendarSync::start` call site (pass the account's `Clock`), `crates/exo/src/calendar/mod.rs` or `account.rs` (push settings to the task when they change)
- Test: `crates/exo/tests/calendar.rs`

**Interfaces:**
- Consumes: Task 1's Store methods; Task 2's `window_range`, `keep_window`, `due`; `crate::Clock` (`now() -> Zoned`).
- Produces:
  - `CalendarSync::start(store, loader, presence, repaint, restarts, clock: Clock) -> Self` (new last parameter).
  - `impl CalendarSync { pub fn set_cache(&self, cache: CalendarCache) }` — sends on a `watch` the task reads; the app calls it with `settings.calendar.cache` whenever settings change (and once at start).
  - `Task::refresh_cache(&mut self)` — private.

Behaviour: the task's `select!` gains the cache settings' `changed()` and a sleep to the next check (at most 60 s). On each wake: `forget_cached_guests(&cache.people)` (or `&[]` when off); if `due(cache, clock.now(), last)` and the connection isn't down, read every listed person with `attendee_events(email, window_range)` 4 at a time (`futures::stream::iter(..).buffer_unordered(4)`), then per answer — re-checking the person is still listed and the switch still on — `Ok(Some(ev))` → `cache_guest` with `keep_window(ev)` and `fetched_at = clock.now()`; `Ok(None)` → delete the person's row; `Err` → keep, `log::warn!`. `last` starts as the oldest `fetched_at` among listed people's rows (so launch and wake refresh at once when stale) and is set after each refresh. Hold a `Busy` guard during a refresh so `Fixture::settle` waits for it.

- [ ] **Step 1: Write the failing tests** (fixture clock at the mock's Tue 2026-11-17 09:00 New York; use `start_with_settings` to switch the cache on):
  - `listed_colleagues_are_cached_on_launch`: people `[priya, lena]` → after settle, both have cached rows; priya's events include `Priya / Maya 1:1` and `Design crit`, none dated after 2026-11-23 except weekdays through 11-23 (window Tue 11-17 … Mon 11-23).
  - `nothing_is_cached_outside_the_schedule_or_with_nobody_listed`: clock Sat 2026-11-21 10:00 → no rows; Tue 19:00 → no rows; enabled with `people: []` → no Calendar requests for guests (no rows, no warnings).
  - `the_cache_refreshes_at_its_frequency_and_forgets_whom_it_drops`: launch (rows at 09:00), `set_now` 09:30 + `wait(0.1)` → `fetched_at` unchanged (every 60); `set_now` 10:01 → refreshed; remove priya in settings → priya's row gone; switch off → all gone.
  - `a_colleague_who_stops_sharing_is_forgotten_and_an_offline_refresh_keeps_the_cache`: fault 404 on priya's events and on freeBusy so `attendee_events` returns hidden → row deleted at the next due refresh; `app.disconnect()` at a due time → lena's row kept and unchanged.
  - `a_colleague_removed_during_a_refresh_isn_t_written_back`: gate or slow the read (gmail-mock `Config::latency` via a new `support::start_with_config` if needed), remove the person before it lands → no row.
  Add a test accessor `pub fn calendar_cached(&self, email: &str) -> Option<exo_store::CachedGuest>` (blocking read through the account's store, as other test accessors read store state).
- [ ] **Step 2: Run** `cargo test -p exo --test calendar cached` → FAIL.
- [ ] **Step 3: Implement** as above.
- [ ] **Step 4: Run** `cargo test -p exo --test calendar` → PASS.
- [ ] **Step 5: Commit** `feat: listed colleagues' calendars are cached on a schedule while Exo runs`

### Task 5: Instant from the cache, refreshed live, with a spinner

**Files:**
- Modify: `crates/exo/src/calendar/sync.rs` (task-side `load_guests`, `GuestLoaded` handler), `crates/exo/src/calendar/guests.rs` (`Marker`, `marker`, `paint_marker`, `calendar_overlay_heads`), `crates/exo/src/calendar/grid.rs` (`Grid.heads`, `paint_heads`), `crates/exo/src/calendar/meet.rs` (marker layout), `crates/exo/src/calendar/form.rs` (chip marker layout), `crates/exo/src/calendar/mod.rs` (accessors)
- Test: `crates/exo/tests/calendar.rs`, unit tests in `guests.rs`

**Interfaces:**
- Consumes: Task 1 (`cached_guest`, `cache_guest`), Task 2 (`keep_window`), Task 4 (clock and cache settings in the task).
- Produces:
  - `Marker::Refreshing(CalendarColor)` — shown and loading: the spinner, then the swatch, both visible.
  - `marker()` returns `Refreshing(color)` when the person is shown, `loading` and online; otherwise as today.
  - `Grid.heads: &'a [(String, CalendarColor, bool)]` — the bool is "refreshing"; `paint_heads` draws the spinner before the swatch when it's true.
  - `calendar_form_chips()` maps `Refreshing(c)` to `"refreshing:<colour name>"`; a new `pub fn calendar_meet_markers_for_test(&self) -> BTreeMap<String, String>` maps the same way for Meet.

Behaviour: in the task-side `load_guests`, for each person whose in-memory events are `Pending`, read `cached_guest`; if present, set `events: Shown(cached.events)` (`loading: true`) and publish before spawning the live reads. In the `GuestLoaded` handler, when the person is in the cache list and the switch is on: `Some(Some(ev))` → `cache_guest` with `keep_window(ev)`, `fetched_at = clock.now()`; `Some(None)` → delete their row; `None` (unreachable) → keep the row, and keep `Shown(cached)` instead of `Failed` when cached events were shown.

- [ ] **Step 1: Write the failing tests**:
  - unit `a_shown_guest_still_loading_is_refreshing`: `Data` with priya `Shown`+`loading` → `Refreshing(color)`; not loading → `Swatch(color)`; `Pending`+loading → `Spinner`.
  - `a_cached_colleague_shows_at_once_and_stays_when_the_live_read_fails`: settings cache priya; launch and settle (rows written); fault 500 ×3 on priya's events; open the calendar, `m`, add priya → her column shows `Design crit` (from the cache) and her marker isn't `Failed`/empty.
  - `a_live_read_updates_the_cache`: cached at 09:00; `set_now` 09:20 (not due); open Meet, add priya, settle → `calendar_cached(priya).fetched_at` is 09:20.
  - `the_day_view_head_spins_before_the_swatch_while_refreshing`: Day view with priya cached and the live read held (latency) → `calendar_meet_markers_for_test()["priya@brightmoor.co"] == "refreshing:<colour>"` and the Day head reports refreshing (accessor over `Grid.heads` as needed).
- [ ] **Step 2: Run** `cargo test -p exo --test calendar cached --lib guests` → FAIL.
- [ ] **Step 3: Implement** as above.
- [ ] **Step 4: Run** `cargo test -p exo --no-fail-fast` → all pass.
- [ ] **Step 5: Commit** `feat: cached colleagues show at once in Meet and the overlays while the latest loads, with a spinner before their colour`

### Task 6: Full verification and the app

- [ ] **Step 1:** `cargo fmt --all --check` and `cargo test --workspace --no-fail-fast` → 0 failed.
- [ ] **Step 2:** `RBENV_VERSION=4.0.10 rbenv exec bundle exec fastlane build` → `target/release/Email.app`.
- [ ] **Step 3:** Fill the board task's "How to test" (branch, build, Settings → Calendar cache steps, what to expect) and move it to `testing/` per the board's rules.
