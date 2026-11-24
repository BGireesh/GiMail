//! The rules of the calendar cache that need no store or network: which days it covers, which of
//! a person's events belong in it, and when a refresh is due.

use exo_store::CalendarCache;
use gmail::calendar::{Event, EventDateTime};
use jiff::{
    Timestamp, Zoned,
    civil::{Date, Weekday},
    tz::TimeZone,
};

/// The days the cache covers: today and the six after it, without Saturdays and Sundays.
pub fn window(today: Date) -> Vec<Date> {
    (0..7)
        .filter_map(|n| today.checked_add(jiff::Span::new().days(n)).ok())
        .filter(|day| !matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday))
        .collect()
}

/// The span of time to ask Google for: the start of today to the start of the day a week on, in
/// `tz`.
pub fn window_range(today: Date, tz: &TimeZone) -> (Timestamp, Timestamp) {
    let start = today
        .to_zoned(tz.clone())
        .map(|z| z.timestamp())
        .unwrap_or(Timestamp::UNIX_EPOCH);
    let end = today
        .checked_add(jiff::Span::new().days(7))
        .ok()
        .and_then(|day| day.to_zoned(tz.clone()).ok())
        .map(|z| z.timestamp())
        .unwrap_or(start);
    (start, end)
}

/// The events under way on a day of the window, in `tz`: an event begun before it (a week out of
/// office) is kept for the days of it the window has.
pub fn keep_window(events: Vec<Event>, today: Date, tz: &TimeZone) -> Vec<Event> {
    let days = window(today);
    events
        .into_iter()
        .filter(|event| {
            event
                .start
                .as_ref()
                .and_then(|start| span(start, event.end.as_ref(), tz))
                .is_some_and(|(start, end)| {
                    days.iter()
                        .filter_map(|day| day_span(*day, tz))
                        .any(|(from, to)| start < to && end > from)
                })
        })
        .collect()
}

/// When an event starting at `start` and ending at `end` (exclusive) is under way, in `tz`: an
/// all-day event from the start of its first day to the start of its end date (the next day
/// without one), a timed one from its start to its end (a moment, without one).
fn span(
    start: &EventDateTime,
    end: Option<&EventDateTime>,
    tz: &TimeZone,
) -> Option<(Timestamp, Timestamp)> {
    if let Some(first) = &start.date {
        let first: Date = first.parse().ok()?;
        let last = end
            .and_then(|end| end.date.as_deref())
            .and_then(|end| end.parse::<Date>().ok())
            .filter(|end| *end > first)
            .or_else(|| first.tomorrow().ok())?;
        let (from, _) = day_span(first, tz)?;
        let (to, _) = day_span(last, tz)?;
        return Some((from, to));
    }
    let from: Timestamp = start.date_time.as_deref()?.parse().ok()?;
    let to = end
        .and_then(|end| end.date_time.as_deref())
        .and_then(|end| end.parse::<Timestamp>().ok())
        .filter(|to| *to > from)
        // A moment still falls on its day.
        .unwrap_or_else(|| from + jiff::SignedDuration::from_nanos(1));
    Some((from, to))
}

/// The start of `day` and of the day after, in `tz`.
fn day_span(day: Date, tz: &TimeZone) -> Option<(Timestamp, Timestamp)> {
    let from = day.to_zoned(tz.clone()).ok()?.timestamp();
    let to = day.tomorrow().ok()?.to_zoned(tz.clone()).ok()?.timestamp();
    Some((from, to))
}

/// Whether the cache should refresh at `now`, the last refresh having been at `last`.
pub fn due(cache: &CalendarCache, now: &Zoned, last: Option<Timestamp>) -> bool {
    if !cache.enabled || cache.people.is_empty() {
        return false;
    }
    let day = now.weekday().to_monday_zero_offset() as usize;
    if !cache.days.get(day).copied().unwrap_or(false) {
        return false;
    }
    let minute =
        u16::try_from(i32::from(now.hour()) * 60 + i32::from(now.minute())).unwrap_or(u16::MAX);
    if !(cache.from_minute <= minute && minute < cache.to_minute) {
        return false;
    }
    last.is_none_or(|last| {
        now.timestamp().as_second() - last.as_second() >= i64::from(cache.every_minutes) * 60
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn timed(rfc3339: &str) -> Event {
        Event {
            start: Some(EventDateTime {
                date_time: Some(rfc3339.into()),
                ..EventDateTime::default()
            }),
            ..Event::default()
        }
    }

    fn all_day(day: &str) -> Event {
        Event {
            start: Some(EventDateTime {
                date: Some(day.into()),
                ..EventDateTime::default()
            }),
            ..Event::default()
        }
    }

    #[test]
    fn the_window_is_the_next_seven_days_weekdays_only() {
        assert_eq!(
            window(date(2026, 11, 19)),
            [
                date(2026, 11, 19),
                date(2026, 11, 20),
                date(2026, 11, 23),
                date(2026, 11, 24),
                date(2026, 11, 25)
            ]
        );
        assert_eq!(
            window(date(2026, 11, 21)),
            [
                date(2026, 11, 23),
                date(2026, 11, 24),
                date(2026, 11, 25),
                date(2026, 11, 26),
                date(2026, 11, 27)
            ]
        );
    }

    #[test]
    fn events_outside_the_window_are_dropped() {
        let tz = TimeZone::get("America/New_York").expect("zone");
        let events = vec![
            timed("2026-11-20T10:00:00-05:00"),
            timed("2026-11-21T10:00:00-05:00"),
            timed("2026-11-26T10:00:00-05:00"),
            timed("2026-11-27T10:00:00-05:00"),
            all_day("2026-11-23"),
        ];
        let kept = keep_window(events.clone(), date(2026, 11, 20), &tz);
        assert_eq!(
            kept,
            [events[0].clone(), events[2].clone(), events[4].clone()]
        );
        let (start, end) = window_range(date(2026, 11, 20), &tz);
        assert_eq!(start.to_string(), "2026-11-20T05:00:00Z");
        assert_eq!(end.to_string(), "2026-11-27T05:00:00Z");
    }

    fn timed_until(start: &str, end: &str) -> Event {
        Event {
            end: Some(EventDateTime {
                date_time: Some(end.into()),
                ..EventDateTime::default()
            }),
            ..timed(start)
        }
    }

    fn all_days(first: &str, end: &str) -> Event {
        Event {
            end: Some(EventDateTime {
                date: Some(end.into()),
                ..EventDateTime::default()
            }),
            ..all_day(first)
        }
    }

    #[test]
    fn events_under_way_on_a_window_day_are_kept() {
        let tz = TimeZone::get("America/New_York").expect("zone");
        let monday = date(2026, 11, 23);
        let events = vec![
            // Out of office from the Friday before to the Wednesday after (end exclusive).
            all_days("2026-11-20", "2026-11-26"),
            // Sunday night into Monday.
            timed_until("2026-11-22T22:00:00-05:00", "2026-11-23T01:00:00-05:00"),
            // Ends just as the window starts.
            timed_until("2026-11-22T23:00:00-05:00", "2026-11-23T00:00:00-05:00"),
            // All day Sunday.
            all_days("2026-11-22", "2026-11-23"),
        ];
        assert_eq!(
            keep_window(events.clone(), monday, &tz),
            [events[0].clone(), events[1].clone()]
        );

        let friday = date(2026, 11, 20);
        let events = vec![
            // Saturday, between the window's Friday and Monday.
            timed_until("2026-11-21T10:00:00-05:00", "2026-11-21T12:00:00-05:00"),
            all_days("2026-11-21", "2026-11-22"),
            // Saturday through Monday.
            all_days("2026-11-21", "2026-11-24"),
            // Friday 10 PM in New York, Saturday in UTC.
            timed_until("2026-11-21T03:00:00Z", "2026-11-21T03:30:00Z"),
        ];
        assert_eq!(
            keep_window(events.clone(), friday, &tz),
            [events[2].clone(), events[3].clone()]
        );
    }

    #[test]
    fn due_follows_the_days_hours_and_frequency() {
        let ny = TimeZone::get("America/New_York").expect("zone");
        let at = |d: Date, h: i8, m: i8| d.at(h, m, 0, 0).to_zoned(ny.clone()).expect("zoned");
        let cache = CalendarCache {
            enabled: true,
            people: vec!["priya@brightmoor.co".into()],
            ..CalendarCache::default()
        };
        let tue = date(2026, 11, 24);
        let now = at(tue, 9, 0);
        let ago = |minutes: i64| Some(now.timestamp() - jiff::SignedDuration::from_mins(minutes));
        assert!(due(&cache, &now, None));
        assert!(!due(&cache, &now, ago(30)));
        assert!(due(&cache, &now, ago(61)));
        assert!(!due(&cache, &at(tue, 7, 59), None));
        assert!(!due(&cache, &at(tue, 18, 0), None));
        assert!(!due(&cache, &at(date(2026, 11, 21), 10, 0), None));
        let no_tuesday = CalendarCache {
            days: [true, false, true, true, true],
            ..cache.clone()
        };
        assert!(!due(&no_tuesday, &now, None));
        let nobody = CalendarCache {
            people: Vec::new(),
            ..cache.clone()
        };
        assert!(!due(&nobody, &now, None));
        let backwards = CalendarCache {
            from_minute: 1080,
            to_minute: 480,
            ..cache
        };
        assert!(!due(&backwards, &now, None));
    }
}
