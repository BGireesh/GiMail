# Calendar cache for Meet: design

Date: 2026-11-24. Status: agreed in conversation; written for review.

## Why

Checking colleagues' availability in Meet (the calendar's `m` sidebar) is slow: each person added
waits on a live Google request for their events before their column fills in, so the user picks
people and then waits. The user checks the same colleagues again and again. They want those
colleagues' calendars cached locally and refreshed on a schedule, so the side-by-side view opens at
once from the cache while the latest data loads in parallel.

## Goals

### Settings

Calendar Settings gets a **Calendar cache** section:

```
Calendar cache                                   [ on ]
  People      [ Enter name or email          ]
              ● Priya Raman        ×
              ● Lena Fischer       ×
  Refresh on  [M] [T] [W] [T] [F]
  Between     [ 8:00 AM ] and [ 6:00 PM ]
  Every       [ 1 hour ▾ ]    (15 minutes, 30 minutes, 1 hour)
```

- An on/off switch; the rest shows only while it's on. Off: nothing is cached or refreshed, and the
  cached data is dropped.
- **People**: the same people picker Meet uses (contacts suggested as you type, Return or a click
  adds, × removes). Removing a person drops their cached data.
- **Refresh on**: weekdays only, Monday to Friday, each on or off. Default: all five on.
- **Between**: a start and end time of day, in the user's time zone. Default 8:00 AM to 6:00 PM.
- **Every**: 15 minutes, 30 minutes or 1 hour. Default **1 hour**.

### What's cached

- For each person on the list: their events (title where their calendar shares it, start, end,
  all-day, status, and whatever else Meet's view and event details already use) for the **next 7
  days from today, weekdays only**. On a Thursday that is Thursday, Friday, Monday, Tuesday and
  Wednesday.
- Stored in Exo's local database (the account's store). Days that fall out of the window are
  dropped from the cache at the next refresh or open.
- The cache is the same data Meet already fetches, kept; it isn't shared or sent anywhere.

### When it refreshes

Only while Exo is running; there is no background service.

- While the switch is on, on a selected weekday, between the start and end times: every chosen
  interval.
- When Exo opens, and when the Mac wakes, if it's inside the schedule and the cache is older than
  the chosen interval, it refreshes at once.
- One refresh asks Google for each listed person's events over the window, a few at a time (not
  all at once), and replaces their cached days with the answer.
- Google's answer wins, except when Google couldn't be reached. *(Builder's call, from review.)*
  - Offline, a network or server error: the person keeps their last cached data; the failure is
    logged and retried at the next refresh.
  - Google now shares only busy times: the cache is replaced with the untitled busy blocks.
  - Google now shares nothing (hidden): the person's cached data is deleted.
  So a colleague who stops sharing their calendar stops showing in the cache too.

### Using it in Meet

- Adding a cached person in Meet shows their column immediately from the cache, and at the same
  time fetches their latest events (as Meet does today for everyone).
- While that fetch runs, a spinner shows before the person's name, in Meet's list and on their
  column head in the Day view. When it lands, the view updates to the latest events and the cache
  is updated with them.
- Days Meet shows outside the cached window (a weekend, or further ahead) load live as today.
- People not on the list behave as today: a live fetch, with the spinner before their name while it
  loads.
- The spinner sits before the person's colour swatch, so the colour key stays visible while it
  spins. *(Builder's call, from review.)*
- The same instant-then-fresh behaviour applies wherever Exo lays colleagues' calendars over the
  calendar: Meet, the event form's guest overlay, and Share Availability. They share one store of
  guest calendars. *(Builder's call, from review.)*

## Non-goals

- Background refreshing while Exo is closed or the Mac is asleep.
- Caching weekends, or more than the next 7 days.
- Caching anyone automatically (only the people the user lists).
- Free/busy-only caching.

## Anti-goals

- A cached calendar shown as if it were current with no sign that it's being checked: the spinner
  must show until the live answer lands or fails.
- Refreshing that makes Exo stutter, or floods Google with requests (refreshes stay off the UI
  thread and are spread out).
- Cached data that outlives its person, its day, or the switch being turned off.

## Approaches considered

1. **Cache each listed person's full events for the window. Chosen**: the view opens exactly as
   today (titles included), and for a hand-picked list one request per person per refresh is cheap.
2. Cache only busy times (Google's free/busy, 50 people per request): cheaper, but the instant view
   would show untitled busy blocks until the live fetch; the savings only matter for hundreds of
   people.
3. Google's incremental sync (sync tokens): Google doesn't allow it with a date range, so it would
   download each person's whole calendar once. Rejected.

## Decided by the user

- The feature lives in Settings → Calendar cache, with an enable switch.
- They choose the people; refresh days, a time range, and the frequency (15 min, 30 min, 1 hour;
  default 1 hour).
- Cached people show immediately from the cache while the latest is fetched; updates show and are
  written back to the cache.
- A spinner before the name while fetching the latest.
- The window: the next 7 days from today, weekdays (Monday to Friday) only.
- Refresh only while Exo is running; refresh at once on open if the cache is older than the
  frequency.
- Stored in Exo's local database.
- Engine: approach 1.

## Builder's calls (from the conversation, for the user to check)

- Refresh days are weekdays only too (the picker offers Monday to Friday).
- Refresh also on wake from sleep, not only on launch.
- Default refresh hours 8:00 AM to 6:00 PM, all weekdays on.
- Turning the switch off or removing a person deletes their cached data.

## Left to the builder

- Where the settings live (account settings, an Exo-only key, is the expected shape) and the table
  that holds the cache.
- How many requests run at once during a refresh.
- How the cached events reach Meet's existing guest state (`calendar/sync.rs`: `load_guests`,
  `GuestEvents`) so that cached-then-live needs no second rendering path.
- The settings section's exact layout within Calendar Settings' existing style
  (`calendar/options.rs`).
