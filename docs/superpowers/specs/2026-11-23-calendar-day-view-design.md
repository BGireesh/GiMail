# Calendar day view: design

Date: 2026-11-23. Status: agreed in conversation; the user asked to build it end to end with the
calls below.

## Why

The full calendar (`2`) only shows a week. The user, coming from Google Calendar, wants to switch
between a Day and a Week view, and to check other people's availability side by side: with people
added in Meet, a single day should show each person's calendar next to theirs, readable at a
glance.

## Goals

- The full calendar has two views, **Day** and **Week**.
- `d` switches to Day and `w` to Week (Google Calendar's keys), in the full calendar only. A
  `Day | Week` toggle in the calendar's header does the same with the mouse.
- The view is remembered per device: the full calendar reopens in the view last used.
- Next/previous (`= j n →` / `- k p ←`) step one day in Day and one week in Week. `t`/`space` is
  today in both. Jump to date lands on that day in Day.
- Day view with people in Meet: **one full-width column per person, the user's own first**, then
  each added person in the order added, each headed by the person's name and colour (the colours
  Meet already assigns). Their events fill their column (Google Calendar's day view with several
  calendars). Hover-to-expand isn't needed in Day: every column is already wide.
- Day view without people in Meet: one column, the user's calendars, the full width.
- The time gutter shows the secondary time zones in Day as it does in Week (Calendar Settings →
  Show secondary timezone).
- Everything Week does on a day keeps working in Day: creating an event by dragging or `b`, opening
  an event's details, the now line, all-day events, Find Time's free slots (shaded hours and slot
  picking), Share Availability.

## Non-goals

- Month, agenda or custom N-day views.
- Several secondary time zones at once (the setting keeps one).
- Changing the mail window's right-pane day (`0`), or the event form's day.

## Anti-goals

- Day view mustn't change Week's look or keys, or the Meet sidebar's behaviour.
- Weekends: the "show weekends" setting hides weekend columns in Week. Day shows whatever day is
  active, weekend or not, as Google does.

## Approaches considered

- **A. Google-style per-person columns in Day (chosen).** Day has room for a full column per
  person, which is what makes checking availability easy.
- **B. Week's split days, one day wide.** Least work, but each person's column stays as narrow as
  in Week.

## Builder's notes (from the code as it is)

- `Week` (`crates/exo/src/calendar/week.rs`) keeps only `active`; the days come from
  `calendar_week_days` (`calendar/mod.rs`). Day view is a mode on the same screen, not a new
  screen: the days become `[active]`.
- Per-person columns already exist inside each day (`calendar/guests.rs`: `columns`, `widths`,
  all-day handling, colours). Day can lay the same columns out at full width.
- Secondary zones reach the grid as `Grid.zones` (`calendar_secondary_zones()` in `calendar/mod.rs`).
- Device settings hold per-device UI state; the remembered view belongs there.
- Keys are registered in `register_week` (`calendar/mod.rs`); `d` and `w` are free in the `WEEK`
  region.
