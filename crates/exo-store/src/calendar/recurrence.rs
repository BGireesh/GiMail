//! A recurring event's rule as the event form makes it (a pattern and a range), converted to and
//! from both RFC 5545 `RRULE` lines (Google Calendar) and Microsoft Graph's
//! [`PatternedRecurrence`], plus instance expansion and series-split arithmetic.

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};
use outlook::calendar::{
    DayOfWeek, PatternedRecurrence, RecurrencePattern, RecurrencePatternType, RecurrenceRange,
    RecurrenceRangeType, WeekIndex,
};
use serde::{Deserialize, Serialize};

use super::model::{CalendarEvent, EventPatch, EventTime};

/// Most days one expansion walks through (about 30 years), so an open-ended rule stays bounded.
const MAX_DAYS: i32 = 366 * 30;

/// How a series repeats.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Frequency {
    Daily,
    /// On these days, in this order. Empty means the start's day.
    Weekly(#[serde(with = "serde_weekdays")] Vec<Weekday>),
    /// On the start's day of the month.
    MonthlyOnDay,
    /// On the start's weekday in this week of the month. `None` means the start's week.
    MonthlyOnWeekday(Option<Nth>),
    /// On the start's day of the year.
    Yearly,
}

mod serde_weekdays {
    use jiff::civil::Weekday;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    fn code(day: Weekday) -> &'static str {
        match day {
            Weekday::Monday => "MO",
            Weekday::Tuesday => "TU",
            Weekday::Wednesday => "WE",
            Weekday::Thursday => "TH",
            Weekday::Friday => "FR",
            Weekday::Saturday => "SA",
            Weekday::Sunday => "SU",
        }
    }

    fn parse(text: &str) -> Option<Weekday> {
        match text {
            "MO" | "monday" | "Monday" => Some(Weekday::Monday),
            "TU" | "tuesday" | "Tuesday" => Some(Weekday::Tuesday),
            "WE" | "wednesday" | "Wednesday" => Some(Weekday::Wednesday),
            "TH" | "thursday" | "Thursday" => Some(Weekday::Thursday),
            "FR" | "friday" | "Friday" => Some(Weekday::Friday),
            "SA" | "saturday" | "Saturday" => Some(Weekday::Saturday),
            "SU" | "sunday" | "Sunday" => Some(Weekday::Sunday),
            _ => None,
        }
    }

    pub fn serialize<S: Serializer>(days: &[Weekday], serializer: S) -> Result<S::Ok, S::Error> {
        let codes: Vec<&str> = days.iter().copied().map(code).collect();
        codes.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Weekday>, D::Error> {
        let codes = Vec::<String>::deserialize(deserializer)?;
        codes
            .into_iter()
            .map(|text| {
                parse(&text)
                    .ok_or_else(|| serde::de::Error::custom(format!("unknown weekday: {text}")))
            })
            .collect()
    }
}

/// A week of the month.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Nth {
    First,
    Second,
    Third,
    Fourth,
    Last,
}

/// When a series ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum End {
    Never,
    /// Its last start may fall on this day.
    On(Date),
    /// After this many starts.
    After(u32),
}

/// A series' rule: how it repeats, every how many days, weeks, months or years, and until when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recurrence {
    pub frequency: Frequency,
    /// 1 to 999.
    pub interval: u32,
    pub end: End,
}

impl Nth {
    /// The week of the month that `date` is in. A month's 29th to 31st days are in the last.
    #[must_use]
    pub fn of(date: Date) -> Self {
        match (date.day() - 1) / 7 {
            0 => Self::First,
            1 => Self::Second,
            2 => Self::Third,
            3 => Self::Fourth,
            _ => Self::Last,
        }
    }

    /// Its word (`first`, …, `last`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Second => "second",
            Self::Third => "third",
            Self::Fourth => "fourth",
            Self::Last => "last",
        }
    }

    /// The ordinal in `BYDAY`, 1 to 4 or -1.
    fn ordinal(self) -> i8 {
        match self {
            Self::First => 1,
            Self::Second => 2,
            Self::Third => 3,
            Self::Fourth => 4,
            Self::Last => -1,
        }
    }

    fn from_ordinal(ordinal: i8) -> Option<Self> {
        [
            Self::First,
            Self::Second,
            Self::Third,
            Self::Fourth,
            Self::Last,
        ]
        .into_iter()
        .find(|nth| nth.ordinal() == ordinal)
    }

    /// Converts to Microsoft Graph's [`WeekIndex`].
    #[must_use]
    pub const fn to_graph(self) -> WeekIndex {
        match self {
            Self::First => WeekIndex::First,
            Self::Second => WeekIndex::Second,
            Self::Third => WeekIndex::Third,
            Self::Fourth => WeekIndex::Fourth,
            Self::Last => WeekIndex::Last,
        }
    }

    /// Converts from Microsoft Graph's [`WeekIndex`].
    #[must_use]
    pub const fn from_graph(index: WeekIndex) -> Option<Self> {
        match index {
            WeekIndex::First => Some(Self::First),
            WeekIndex::Second => Some(Self::Second),
            WeekIndex::Third => Some(Self::Third),
            WeekIndex::Fourth => Some(Self::Fourth),
            WeekIndex::Last => Some(Self::Last),
            WeekIndex::UnknownFutureValue => None,
        }
    }

    /// Whether `date` is in this week of its month.
    fn holds(self, date: Date) -> bool {
        match self {
            Self::Last => date.day() + 7 > date.days_in_month(),
            nth => (date.day() - 1) / 7 + 1 == nth.ordinal(),
        }
    }
}

/// RFC 5545's two letters for `day`.
fn code(day: Weekday) -> &'static str {
    match day {
        Weekday::Sunday => "SU",
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
    }
}

fn day_of(code: &str) -> Option<Weekday> {
    [
        Weekday::Sunday,
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
    ]
    .into_iter()
    .find(|day| self::code(*day) == code)
}

/// The Sunday a week starts on (`WKST=SU`).
fn week_of(date: Date) -> Date {
    date.checked_sub(i64::from(date.weekday().to_sunday_zero_offset()).days())
        .unwrap_or(date)
}

impl Recurrence {
    /// The rule of a series' `recurrence` lines (its `RRULE`, else its first line) in `tz`, when
    /// it's one the event form can make.
    #[must_use]
    pub fn from_rules(lines: &[String], tz: &TimeZone) -> Option<Self> {
        Self::parse_rules(lines, tz, None)
    }

    /// [`from_rules`](Self::from_rules), also verifying that a monthly `BYDAY` weekday matches
    /// `start`'s weekday so expansion and splits cannot silently shift the rule to another day.
    #[must_use]
    pub(crate) fn from_rules_for(lines: &[String], start: &Zoned) -> Option<Self> {
        Self::parse_rules(lines, start.time_zone(), Some(start.weekday()))
    }

    fn parse_rules(
        lines: &[String],
        tz: &TimeZone,
        start_weekday: Option<Weekday>,
    ) -> Option<Self> {
        if lines
            .iter()
            .any(|line| line.starts_with("EXDATE") || line.starts_with("RDATE"))
            || lines
                .iter()
                .filter(|line| line.starts_with("RRULE"))
                .count()
                > 1
        {
            return None;
        }
        let line = lines
            .iter()
            .find(|line| line.starts_with("RRULE"))
            .or(lines.first())?;
        let rule = line.strip_prefix("RRULE:").unwrap_or(line);
        let (mut frequency, mut interval, mut end, mut by_day) =
            (None, 1, None, None::<Vec<(Option<i8>, Weekday)>>);
        for part in rule.split(';') {
            let (key, value) = part.split_once('=')?;
            match key {
                "FREQ" => frequency = Some(value),
                "INTERVAL" => interval = value.parse().ok().filter(|interval| *interval > 0)?,
                "COUNT" => {
                    if end.is_some() {
                        return None;
                    }
                    end = Some(End::After(value.parse().ok()?));
                }
                "UNTIL" => {
                    if end.is_some() {
                        return None;
                    }
                    end = Some(End::On(until_date(value, tz)?));
                }
                "BYDAY" => {
                    let days = value.split(',').map(|token| {
                        if !token.is_ascii() {
                            return None;
                        }
                        let at = token.len().checked_sub(2)?;
                        let ordinal = if at == 0 {
                            None
                        } else {
                            Some(token[..at].parse().ok()?)
                        };
                        Some((ordinal, day_of(&token[at..])?))
                    });
                    by_day = Some(days.collect::<Option<_>>()?);
                }
                "WKST" if value == "SU" => {}
                _ => return None,
            }
        }
        let frequency = match (frequency?, by_day) {
            ("DAILY", None) => Frequency::Daily,
            ("WEEKLY", None) => Frequency::Weekly(Vec::new()),
            ("WEEKLY", Some(days)) => Frequency::Weekly(
                days.into_iter()
                    .map(|(ordinal, day)| ordinal.is_none().then_some(day))
                    .collect::<Option<_>>()?,
            ),
            ("MONTHLY", None) => Frequency::MonthlyOnDay,
            ("MONTHLY", Some(days)) => match days.as_slice() {
                [(Some(ordinal), weekday)] => {
                    if let Some(expected) = start_weekday
                        && *weekday != expected
                    {
                        return None;
                    }
                    Frequency::MonthlyOnWeekday(Some(Nth::from_ordinal(*ordinal)?))
                }
                _ => return None,
            },
            ("YEARLY", None) => Frequency::Yearly,
            _ => return None,
        };
        Some(Self {
            frequency,
            interval,
            end: end.unwrap_or(End::Never),
        })
    }

    /// Converts a Microsoft Graph [`PatternedRecurrence`] into a [`Recurrence`], when it fits the
    /// supported frequencies and ranges and aligns with `range.start_date`.
    #[must_use]
    pub fn from_graph(recurrence: &PatternedRecurrence) -> Option<Self> {
        let pattern = recurrence.pattern.as_ref()?;
        let range = recurrence.range.as_ref()?;
        let interval = pattern.interval.unwrap_or(1).max(1);
        let start_date = range.start_date;
        let frequency = match pattern.kind? {
            RecurrencePatternType::Daily => Frequency::Daily,
            RecurrencePatternType::Weekly => {
                if interval > 1
                    && matches!(
                        pattern.first_day_of_week,
                        Some(day) if day != DayOfWeek::Sunday
                    )
                {
                    return None;
                }
                Frequency::Weekly(
                    pattern
                        .days_of_week
                        .iter()
                        .copied()
                        .map(Weekday::from)
                        .collect(),
                )
            }
            RecurrencePatternType::AbsoluteMonthly => {
                if let (Some(start), Some(day)) = (start_date, pattern.day_of_month)
                    && i8::try_from(day).ok() != Some(start.day())
                {
                    return None;
                }
                Frequency::MonthlyOnDay
            }
            RecurrencePatternType::RelativeMonthly => {
                if pattern.days_of_week.len() != 1 {
                    return None;
                }
                if let Some(start) = start_date
                    && Weekday::from(pattern.days_of_week[0]) != start.weekday()
                {
                    return None;
                }
                let index = match pattern.index {
                    Some(idx) => Some(Nth::from_graph(idx)?),
                    None => None,
                };
                Frequency::MonthlyOnWeekday(index)
            }
            RecurrencePatternType::AbsoluteYearly => {
                if let Some(start) = start_date {
                    if let Some(month) = pattern.month
                        && i8::try_from(month).ok() != Some(start.month())
                    {
                        return None;
                    }
                    if let Some(day) = pattern.day_of_month
                        && i8::try_from(day).ok() != Some(start.day())
                    {
                        return None;
                    }
                }
                Frequency::Yearly
            }
            RecurrencePatternType::RelativeYearly | RecurrencePatternType::UnknownFutureValue => {
                return None;
            }
        };
        let end = match range.kind? {
            RecurrenceRangeType::NoEnd => End::Never,
            RecurrenceRangeType::EndDate => End::On(range.effective_end_date()?),
            RecurrenceRangeType::Numbered => {
                End::After(range.number_of_occurrences.filter(|&n| n > 0)?)
            }
            RecurrenceRangeType::UnknownFutureValue => return None,
        };
        Some(Self {
            frequency,
            interval,
            end,
        })
    }

    /// Converts this rule to a Microsoft Graph [`PatternedRecurrence`] starting on `start_date`
    /// in `zone_name` (IANA or Windows time zone name).
    #[must_use]
    pub fn to_graph(&self, start_date: Date, zone_name: &str) -> PatternedRecurrence {
        let interval = self.interval.max(1);
        let mut pattern = RecurrencePattern {
            interval: Some(interval),
            ..RecurrencePattern::default()
        };
        match &self.frequency {
            Frequency::Daily => {
                pattern.kind = Some(RecurrencePatternType::Daily);
            }
            Frequency::Weekly(days) => {
                let order = [
                    Weekday::Sunday,
                    Weekday::Monday,
                    Weekday::Tuesday,
                    Weekday::Wednesday,
                    Weekday::Thursday,
                    Weekday::Friday,
                    Weekday::Saturday,
                ];
                let chosen = self.weekdays(days, start_date);
                pattern.kind = Some(RecurrencePatternType::Weekly);
                pattern.days_of_week = order
                    .into_iter()
                    .filter(|day| chosen.contains(day))
                    .map(DayOfWeek::from)
                    .collect();
                pattern.first_day_of_week = Some(DayOfWeek::Sunday);
            }
            Frequency::MonthlyOnDay => {
                pattern.kind = Some(RecurrencePatternType::AbsoluteMonthly);
                pattern.day_of_month = Some(u32::from(start_date.day().unsigned_abs()));
            }
            Frequency::MonthlyOnWeekday(nth) => {
                pattern.kind = Some(RecurrencePatternType::RelativeMonthly);
                pattern.days_of_week = vec![DayOfWeek::from(start_date.weekday())];
                pattern.index = Some(nth.unwrap_or_else(|| Nth::of(start_date)).to_graph());
            }
            Frequency::Yearly => {
                pattern.kind = Some(RecurrencePatternType::AbsoluteYearly);
                pattern.month = Some(u32::from(start_date.month().unsigned_abs()));
                pattern.day_of_month = Some(u32::from(start_date.day().unsigned_abs()));
            }
        }
        let tz_name = gmail::ics::iana_to_windows(zone_name)
            .unwrap_or(zone_name)
            .to_owned();
        let mut range = RecurrenceRange {
            start_date: Some(start_date),
            recurrence_time_zone: Some(tz_name),
            ..RecurrenceRange::default()
        };
        match self.end {
            End::Never => {
                range.kind = Some(RecurrenceRangeType::NoEnd);
            }
            End::On(end_date) => {
                range.kind = Some(RecurrenceRangeType::EndDate);
                range.end_date = Some(end_date);
            }
            End::After(count) => {
                range.kind = Some(RecurrenceRangeType::Numbered);
                range.number_of_occurrences = Some(count.max(1));
            }
        }
        PatternedRecurrence::new(pattern, range)
    }

    /// The `RRULE` line for a series starting at `start`. An all-day series ends on a date. Any
    /// other ends at the last second of the end day in its zone, written in UTC.
    #[must_use]
    pub fn rule(&self, start: &Zoned, all_day: bool) -> String {
        let mut parts = vec![format!(
            "FREQ={}",
            match self.frequency {
                Frequency::Daily => "DAILY",
                Frequency::Weekly(_) => "WEEKLY",
                Frequency::MonthlyOnDay | Frequency::MonthlyOnWeekday(_) => "MONTHLY",
                Frequency::Yearly => "YEARLY",
            }
        )];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval));
        }
        match &self.frequency {
            Frequency::Weekly(days) => {
                let days = self.weekdays(days, start.date());
                let order = [
                    Weekday::Sunday,
                    Weekday::Monday,
                    Weekday::Tuesday,
                    Weekday::Wednesday,
                    Weekday::Thursday,
                    Weekday::Friday,
                    Weekday::Saturday,
                ];
                parts.push(format!(
                    "BYDAY={}",
                    order
                        .into_iter()
                        .filter(|day| days.contains(day))
                        .map(code)
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                parts.push("WKST=SU".to_owned());
            }
            Frequency::MonthlyOnWeekday(nth) => parts.push(format!(
                "BYDAY={}{}",
                nth.unwrap_or_else(|| Nth::of(start.date())).ordinal(),
                code(start.weekday())
            )),
            Frequency::Daily | Frequency::MonthlyOnDay | Frequency::Yearly => {}
        }
        match self.end {
            End::Never => {}
            End::After(count) => parts.push(format!("COUNT={count}")),
            End::On(day) if all_day => parts.push(format!("UNTIL={}", day.strftime("%Y%m%d"))),
            End::On(day) => {
                let last = day
                    .at(23, 59, 59, 0)
                    .to_zoned(start.time_zone().clone())
                    .map_or(Timestamp::MAX, |at| at.timestamp());
                parts.push(format!("UNTIL={}", last.strftime("%Y%m%dT%H%M%SZ")));
            }
        }
        format!("RRULE:{}", parts.join(";"))
    }

    /// The days a weekly rule repeats on, which are `days`, or the weekday of `first` when
    /// `days` is empty.
    fn weekdays(&self, days: &[Weekday], first: Date) -> Vec<Weekday> {
        if days.is_empty() {
            vec![first.weekday()]
        } else {
            days.to_vec()
        }
    }

    /// Whether `date` is one of the days of a series that starts on `first`.
    fn matches(&self, first: Date, date: Date) -> bool {
        let interval = i64::from(self.interval.max(1));
        let months =
            i64::from(date.year() - first.year()) * 12 + i64::from(date.month() - first.month());
        match &self.frequency {
            Frequency::Daily => i64::from((date - first).get_days()) % interval == 0,
            Frequency::Weekly(days) => {
                self.weekdays(days, first).contains(&date.weekday())
                    && i64::from((week_of(date) - week_of(first)).get_days() / 7) % interval == 0
            }
            Frequency::MonthlyOnDay => date.day() == first.day() && months % interval == 0,
            Frequency::MonthlyOnWeekday(nth) => {
                date.weekday() == first.weekday()
                    && nth.unwrap_or_else(|| Nth::of(first)).holds(date)
                    && months % interval == 0
            }
            Frequency::Yearly => {
                date.month() == first.month()
                    && date.day() == first.day()
                    && i64::from(date.year() - first.year()) % interval == 0
            }
        }
    }

    /// The series' starts, at `first`'s time of day in its zone, from `first`'s day until `stop`
    /// says enough or the rule ends.
    fn walk(&self, first: &Zoned, mut stop: impl FnMut(&Zoned) -> bool) -> Vec<Zoned> {
        let (tz, time, day) = (first.time_zone().clone(), first.time(), first.date());
        let mut starts = Vec::new();
        for offset in 0..MAX_DAYS {
            let Ok(date) = day.checked_add(offset.days()) else {
                break;
            };
            if matches!(self.end, End::On(last) if date > last)
                || matches!(self.end, End::After(count) if starts.len() >= count as usize)
            {
                break;
            }
            if !self.matches(day, date) {
                continue;
            }
            let Ok(start) = date.to_datetime(time).to_zoned(tz.clone()) else {
                continue;
            };
            if stop(&start) {
                break;
            }
            starts.push(start);
        }
        starts
    }

    /// The starts of a series whose first is `first` that fall from `from` to `to`, both
    /// included.
    #[must_use]
    pub fn starts(&self, first: &Zoned, from: Timestamp, to: Timestamp) -> Vec<Zoned> {
        self.walk(first, |start| start.timestamp() > to)
            .into_iter()
            .filter(|start| start.timestamp() >= from)
            .collect()
    }

    /// The first start at or after `start`. This is where a new series starts when its own day
    /// isn't one of the rule's.
    #[must_use]
    pub fn first(&self, start: &Zoned) -> Option<Zoned> {
        let mut found = false;
        self.walk(start, |_| std::mem::replace(&mut found, true))
            .into_iter()
            .next()
    }

    /// The rule for the part of a series from `day` on, where the series starts at `first`. A
    /// count goes down by the starts before `day`.
    #[must_use]
    pub fn from_day(&self, first: &Zoned, day: Date) -> Self {
        let end = match self.end {
            End::After(count) => {
                let before = self.walk(first, |start| start.date() >= day).len();
                End::After(count.saturating_sub(u32::try_from(before).unwrap_or(u32::MAX)))
            }
            end => end,
        };
        Self {
            end,
            ..self.clone()
        }
    }
}

/// Fixed-offset [`TimeZone`] parsed from an RFC 3339 timestamp's trailing `Z` or `±HH:MM`.
pub(crate) fn rfc3339_zone(text: &str) -> Option<TimeZone> {
    let trimmed = text.trim();
    if trimmed.ends_with(['Z', 'z']) {
        return Some(TimeZone::UTC);
    }
    let tail = trimmed.get(trimmed.len().checked_sub(6)?..)?;
    if !tail.is_ascii() {
        return None;
    }
    let sign = match tail.as_bytes().first()? {
        b'+' => 1_i32,
        b'-' => -1_i32,
        _ => return None,
    };
    if tail.as_bytes().get(3) != Some(&b':') {
        return None;
    }
    let hours: i32 = tail[1..3].parse().ok()?;
    let minutes: i32 = tail[4..6].parse().ok()?;
    let seconds = sign * (hours * 3600 + minutes * 60);
    jiff::tz::Offset::from_seconds(seconds)
        .ok()
        .map(TimeZone::fixed)
}

/// Rewrites any `UNTIL=` in `lines` to match `all_day` (date `YYYYMMDD` for all-day, else UTC
/// date-time at the end of that day in `start`'s zone), reading existing `UNTIL` values in
/// `old_tz`.
pub(crate) fn retime_until(
    lines: &[String],
    start: &Zoned,
    all_day: bool,
    old_tz: &TimeZone,
) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line.strip_prefix("RRULE:") {
            Some(rule) => {
                let parts: Vec<String> = rule
                    .split(';')
                    .map(|part| match part.strip_prefix("UNTIL=") {
                        Some(value) => match until_date(value, old_tz) {
                            Some(day) if all_day => format!("UNTIL={}", day.strftime("%Y%m%d")),
                            Some(day) => {
                                let last = day
                                    .at(23, 59, 59, 0)
                                    .to_zoned(start.time_zone().clone())
                                    .map_or(Timestamp::MAX, |at| at.timestamp());
                                format!("UNTIL={}", last.strftime("%Y%m%dT%H%M%SZ"))
                            }
                            None => part.to_owned(),
                        },
                        None => part.to_owned(),
                    })
                    .collect();
                format!("RRULE:{}", parts.join(";"))
            }
            None => line.clone(),
        })
        .collect()
}

/// When `target_start` toggles `all_day` relative to `master.start` and `patch` leaves recurrence
/// untouched, rewrites `UNTIL` in `master.raw_recurrence` (or re-emits `master.recurrence`) into
/// `patch.raw_recurrence`.
pub(crate) fn retime_until_if_toggled(
    master: &CalendarEvent,
    target_start: &EventTime,
    patch: &mut EventPatch,
) {
    let Some(old_start) = &master.start else {
        return;
    };
    if old_start.is_all_day() == target_start.is_all_day()
        || patch.recurrence.is_some()
        || patch.raw_recurrence.is_some()
        || master.raw_recurrence.is_empty()
    {
        return;
    }
    let Some(old_zoned) = old_start.own_zoned(&TimeZone::UTC) else {
        return;
    };
    let Some(target_zoned) = target_start.own_zoned(old_zoned.time_zone()) else {
        return;
    };
    patch.raw_recurrence = Some(retime_until(
        &master.raw_recurrence,
        &target_zoned,
        target_start.is_all_day(),
        old_zoned.time_zone(),
    ));
}

/// The instances of series `event` (which the provider hasn't received yet) starting from `from`
/// to `to`. Each has the id `{series}_{start}`, `recurring_event_id = Some(series)`, and keeps
/// the series' rule so details can describe it.
pub(crate) fn instances(
    event: &CalendarEvent,
    from: Timestamp,
    to: Timestamp,
) -> Vec<CalendarEvent> {
    let (Some(start), Some(end), Some(id)) = (
        event.start.as_ref(),
        event.end.as_ref(),
        event.id.as_deref(),
    ) else {
        return vec![event.clone()];
    };
    let all_day = start.is_all_day();
    let (Some(first), Some(last)) = (
        start.own_zoned(&TimeZone::UTC),
        end.own_zoned(&TimeZone::UTC),
    ) else {
        return vec![event.clone()];
    };
    let Some(rule) = event
        .recurrence
        .clone()
        .or_else(|| Recurrence::from_rules_for(&event.raw_recurrence, &first))
    else {
        return vec![event.clone()];
    };
    let length = last.timestamp().duration_since(first.timestamp());
    let days = (last.date() - first.date()).get_days();
    let start_zone = start.time_zone().map(str::to_owned);
    let end_zone = end.time_zone().map(str::to_owned);
    // An instance starting before `from` still shows if it lasts into it.
    let from = from
        .checked_sub(length)
        .unwrap_or(from)
        .checked_sub(jiff::SignedDuration::from_hours(24))
        .unwrap_or(from);
    rule.starts(&first, from, to)
        .into_iter()
        .filter_map(|at| {
            let until = if all_day {
                at.checked_add(i64::from(days).days()).ok()?
            } else {
                at.checked_add(length).ok()?
            };
            let instance_start = if all_day {
                EventTime::AllDay(at.date())
            } else {
                EventTime::Timed {
                    at: at.timestamp(),
                    time_zone: start_zone.clone(),
                }
            };
            let instance_end = if all_day {
                EventTime::AllDay(until.date())
            } else {
                EventTime::Timed {
                    at: until.timestamp(),
                    time_zone: end_zone.clone(),
                }
            };
            let suffix = if all_day {
                at.strftime("%Y%m%d").to_string()
            } else {
                at.timestamp().strftime("%Y%m%dT%H%M%SZ").to_string()
            };
            Some(CalendarEvent {
                id: Some(format!("{id}_{suffix}")),
                recurring_event_id: Some(id.to_owned()),
                start: Some(instance_start.clone()),
                end: Some(instance_end),
                original_start: Some(instance_start),
                ..event.clone()
            })
        })
        .collect()
}

/// The day of `UNTIL`, as written for a date, else the time's day in `tz`.
fn until_date(value: &str, tz: &TimeZone) -> Option<Date> {
    if value.len() == 8 {
        return Date::strptime("%Y%m%d", value).ok();
    }
    if let Some(utc) = value.strip_suffix('Z') {
        let civil = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", utc).ok()?;
        let at = civil.to_zoned(TimeZone::UTC).ok()?.timestamp();
        return Some(at.to_zoned(tz.clone()).date());
    }
    let civil = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", value).ok()?;
    Some(civil.date())
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn at(text: &str) -> Zoned {
        format!("{text}[America/New_York]").parse().expect("time")
    }

    fn rule(frequency: Frequency, interval: u32, end: End) -> Recurrence {
        Recurrence {
            frequency,
            interval,
            end,
        }
    }

    #[test]
    fn rules_write_their_rrule_lines() {
        let start = at("2026-11-17T15:00");
        assert_eq!(
            rule(Frequency::Daily, 1, End::Never).rule(&start, false),
            "RRULE:FREQ=DAILY"
        );
        assert_eq!(
            rule(Frequency::Weekly(Vec::new()), 1, End::Never).rule(&start, false),
            "RRULE:FREQ=WEEKLY;BYDAY=TU;WKST=SU"
        );
        let weekdays = vec![
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
        ];
        assert_eq!(
            rule(Frequency::Weekly(weekdays), 2, End::After(13)).rule(&start, false),
            "RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TU,WE,TH,FR;WKST=SU;COUNT=13"
        );
        assert_eq!(
            rule(Frequency::MonthlyOnWeekday(None), 1, End::Never).rule(&start, false),
            "RRULE:FREQ=MONTHLY;BYDAY=3TU"
        );
        assert_eq!(
            rule(Frequency::MonthlyOnWeekday(Some(Nth::Last)), 1, End::Never).rule(&start, false),
            "RRULE:FREQ=MONTHLY;BYDAY=-1TU"
        );
        assert_eq!(
            rule(Frequency::Yearly, 1, End::On(date(2027, 2, 17))).rule(&start, false),
            "RRULE:FREQ=YEARLY;UNTIL=20270218T045959Z",
            "the end day's last second in New York"
        );
        assert_eq!(
            rule(Frequency::MonthlyOnDay, 1, End::On(date(2027, 2, 17))).rule(&start, true),
            "RRULE:FREQ=MONTHLY;UNTIL=20270217"
        );
    }

    #[test]
    fn rules_round_trip_with_microsoft_graph_patterned_recurrence() {
        let start = date(2026, 11, 17);
        let cases = [
            rule(Frequency::Daily, 2, End::Never),
            rule(
                Frequency::Weekly(vec![Weekday::Tuesday, Weekday::Thursday]),
                1,
                End::After(8),
            ),
            rule(Frequency::MonthlyOnDay, 1, End::On(date(2027, 5, 17))),
            rule(Frequency::MonthlyOnWeekday(Some(Nth::Third)), 1, End::Never),
            rule(Frequency::Yearly, 1, End::On(date(2030, 11, 17))),
        ];
        for expected in cases {
            let graph = expected.to_graph(start, "America/New_York");
            assert_eq!(
                graph
                    .range
                    .as_ref()
                    .and_then(|r| r.recurrence_time_zone.as_deref()),
                Some("Eastern Standard Time")
            );
            assert_eq!(Recurrence::from_graph(&graph), Some(expected));
        }
    }

    #[test]
    fn rules_read_back_when_the_form_could_make_them() {
        let ny = TimeZone::get("America/New_York").expect("zone");
        let read = |line: &str| Recurrence::from_rules(&[line.to_owned()], &ny);
        assert_eq!(
            read("RRULE:FREQ=WEEKLY;BYDAY=TU;WKST=SU"),
            Some(rule(
                Frequency::Weekly(vec![Weekday::Tuesday]),
                1,
                End::Never
            ))
        );
        assert_eq!(
            read("RRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=4"),
            Some(rule(
                Frequency::MonthlyOnWeekday(Some(Nth::Last)),
                1,
                End::After(4)
            ))
        );
        assert_eq!(
            read("RRULE:FREQ=DAILY;INTERVAL=3;UNTIL=20261231"),
            Some(rule(Frequency::Daily, 3, End::On(date(2026, 12, 31))))
        );
        assert_eq!(
            read("RRULE:FREQ=YEARLY;UNTIL=20270218T045959Z"),
            Some(rule(Frequency::Yearly, 1, End::On(date(2027, 2, 17)))),
            "UNTIL in UTC converts to the series zone without +1 day drift"
        );
        let la = TimeZone::get("America/Los_Angeles").expect("zone");
        let la_start: Zoned = "2026-11-17T09:00[America/Los_Angeles]"
            .parse()
            .expect("time");
        let rrule = rule(Frequency::Daily, 1, End::On(date(2026, 11, 20))).rule(&la_start, false);
        assert_eq!(
            Recurrence::from_rules(std::slice::from_ref(&rrule), &la),
            Some(rule(Frequency::Daily, 1, End::On(date(2026, 11, 20))))
        );
        let series = CalendarEvent {
            id: Some("s1".into()),
            start: Some(EventTime::Timed {
                at: "2026-11-18T17:00:00Z".parse().expect("ts"),
                time_zone: Some("America/Los_Angeles".into()),
            }),
            end: Some(EventTime::Timed {
                at: "2026-11-18T17:30:00Z".parse().expect("ts"),
                time_zone: Some("America/Los_Angeles".into()),
            }),
            raw_recurrence: vec![rrule],
            ..CalendarEvent::default()
        };
        let expanded = instances(&series, Timestamp::MIN, Timestamp::MAX);
        assert_eq!(
            expanded.len(),
            3,
            "Nov 18, 19 and 20 only — never spills onto Nov 21"
        );
        assert_eq!(
            read("RRULE:FREQ=MONTHLY;BYDAY=TU"),
            None,
            "a weekday without its week"
        );
        assert_eq!(read("RRULE:FREQ=WEEKLY;BYDAY=1TU"), None);
        assert_eq!(read("RRULE:FREQ=HOURLY"), None);
    }

    #[test]
    fn starts_follow_the_rule_in_the_series_zone() {
        let first = at("2026-10-27T09:00");
        let weekly = rule(
            Frequency::Weekly(vec![Weekday::Tuesday, Weekday::Thursday]),
            2,
            End::After(4),
        );
        let starts: Vec<String> = weekly
            .starts(&first, Timestamp::MIN, Timestamp::MAX)
            .iter()
            .map(|start| start.strftime("%a %b %-d %H:%M %Z").to_string())
            .collect();
        // Every other week, at 9:00 on either side of the clocks going back on Nov 1.
        assert_eq!(
            starts,
            [
                "Tue Oct 27 09:00 EDT",
                "Thu Oct 29 09:00 EDT",
                "Tue Nov 10 09:00 EST",
                "Thu Nov 12 09:00 EST"
            ]
        );
        let last_friday = rule(Frequency::MonthlyOnWeekday(Some(Nth::Last)), 1, End::Never);
        let days: Vec<Date> = last_friday
            .starts(
                &at("2026-10-30T09:00"),
                Timestamp::MIN,
                at("2027-01-31T00:00").timestamp(),
            )
            .iter()
            .map(Zoned::date)
            .collect();
        assert_eq!(
            days,
            [
                date(2026, 10, 30),
                date(2026, 11, 27),
                date(2026, 12, 25),
                date(2027, 1, 29)
            ]
        );
        let on_31st = rule(Frequency::MonthlyOnDay, 1, End::On(date(2027, 1, 31)));
        let days: Vec<Date> = on_31st
            .starts(
                &at("2026-10-31T09:00"),
                at("2026-11-01T00:00").timestamp(),
                Timestamp::MAX,
            )
            .iter()
            .map(Zoned::date)
            .collect();
        assert_eq!(
            days,
            [date(2026, 12, 31), date(2027, 1, 31)],
            "months without the day are skipped, and the window starts later"
        );
    }

    #[test]
    fn a_series_starts_on_its_first_day_and_splits_with_what_is_left() {
        let start = at("2026-11-17T15:00");
        let fridays = rule(
            Frequency::Weekly(vec![Weekday::Monday, Weekday::Friday]),
            1,
            End::After(6),
        );
        assert_eq!(
            fridays.first(&start).map(|first| first.date()),
            Some(date(2026, 11, 20))
        );
        assert_eq!(
            rule(Frequency::Daily, 1, End::Never).first(&start),
            Some(start.clone())
        );
        let from_dec = fridays.from_day(&at("2026-11-20T15:00"), date(2026, 11, 30));
        assert_eq!(from_dec.end, End::After(3), "Nov 20, 23 and 27 came before");
    }

    #[test]
    fn non_ascii_byday_and_unsupported_rrule_keys_return_none_without_panicking() {
        let ny = TimeZone::get("America/New_York").expect("zone");
        let read = |line: &str| Recurrence::from_rules(&[line.to_owned()], &ny);

        // Multi-byte UTF-8 tokens in BYDAY must never panic on byte-index slicing.
        assert_eq!(read("RRULE:FREQ=WEEKLY;BYDAY=éa"), None);
        assert_eq!(read("RRULE:FREQ=WEEKLY;BYDAY=1é"), None);
        assert_eq!(read("RRULE:FREQ=MONTHLY;BYDAY=é1"), None);
        assert_eq!(read("RRULE:FREQ=WEEKLY;BYDAY=MO,éa"), None);

        // Unrecognized/unrepresentable RRULE clauses and EXDATE/RDATE are rejected.
        assert_eq!(read("RRULE:FREQ=MONTHLY;BYMONTHDAY=15"), None);
        assert_eq!(read("RRULE:FREQ=YEARLY;BYMONTH=6"), None);
        assert_eq!(read("RRULE:FREQ=WEEKLY;BYDAY=MO;WKST=MO"), None);
        assert_eq!(read("RRULE:FREQ=DAILY;COUNT=2;UNTIL=20261231"), None);
        assert_eq!(
            Recurrence::from_rules(
                &[
                    "RRULE:FREQ=WEEKLY;BYDAY=TU".to_owned(),
                    "EXDATE:20261124T150000Z".to_owned(),
                ],
                &ny
            ),
            None
        );

        // `from_rules_for` rejects a monthly BYDAY weekday that disagrees with the start date.
        let tuesday = at("2026-11-17T09:00");
        assert_eq!(
            Recurrence::from_rules_for(&["RRULE:FREQ=MONTHLY;BYDAY=3FR".to_owned()], &tuesday),
            None
        );
        assert_eq!(
            Recurrence::from_rules_for(&["RRULE:FREQ=MONTHLY;BYDAY=3TU".to_owned()], &tuesday),
            Some(rule(
                Frequency::MonthlyOnWeekday(Some(Nth::Third)),
                1,
                End::Never
            ))
        );
    }

    #[test]
    fn from_graph_rejects_unaligned_and_unknown_patterns() {
        let range = |start: Date| RecurrenceRange {
            kind: Some(RecurrenceRangeType::NoEnd),
            start_date: Some(start),
            ..RecurrenceRange::default()
        };

        // UnknownFutureValue WeekIndex must return None (not Some(MonthlyOnWeekday(None))).
        let unknown_idx = PatternedRecurrence {
            pattern: Some(RecurrencePattern {
                kind: Some(RecurrencePatternType::RelativeMonthly),
                interval: Some(1),
                days_of_week: vec![DayOfWeek::Tuesday],
                index: Some(WeekIndex::UnknownFutureValue),
                ..RecurrencePattern::default()
            }),
            range: Some(range(date(2026, 11, 17))),
        };
        assert_eq!(Recurrence::from_graph(&unknown_idx), None);

        // RelativeMonthly weekday mismatch against start_date is rejected.
        let unaligned_rel = PatternedRecurrence {
            pattern: Some(RecurrencePattern {
                kind: Some(RecurrencePatternType::RelativeMonthly),
                interval: Some(1),
                days_of_week: vec![DayOfWeek::Friday],
                index: Some(WeekIndex::Third),
                ..RecurrencePattern::default()
            }),
            range: Some(range(date(2026, 11, 17))), // Tuesday
        };
        assert_eq!(Recurrence::from_graph(&unaligned_rel), None);

        // AbsoluteMonthly day_of_month mismatch against start_date is rejected.
        let unaligned_month = PatternedRecurrence {
            pattern: Some(RecurrencePattern {
                kind: Some(RecurrencePatternType::AbsoluteMonthly),
                interval: Some(1),
                day_of_month: Some(15),
                ..RecurrencePattern::default()
            }),
            range: Some(range(date(2026, 11, 17))),
        };
        assert_eq!(Recurrence::from_graph(&unaligned_month), None);

        // AbsoluteYearly month/day mismatch against start_date is rejected.
        let unaligned_year = PatternedRecurrence {
            pattern: Some(RecurrencePattern {
                kind: Some(RecurrencePatternType::AbsoluteYearly),
                interval: Some(1),
                month: Some(12),
                day_of_month: Some(17),
                ..RecurrencePattern::default()
            }),
            range: Some(range(date(2026, 11, 17))),
        };
        assert_eq!(Recurrence::from_graph(&unaligned_year), None);

        // Multi-week Weekly with non-Sunday first_day_of_week is rejected.
        let non_sunday_biweekly = PatternedRecurrence {
            pattern: Some(RecurrencePattern {
                kind: Some(RecurrencePatternType::Weekly),
                interval: Some(2),
                days_of_week: vec![DayOfWeek::Monday],
                first_day_of_week: Some(DayOfWeek::Monday),
                ..RecurrencePattern::default()
            }),
            range: Some(range(date(2026, 11, 16))),
        };
        assert_eq!(Recurrence::from_graph(&non_sunday_biweekly), None);

        // Single-week Weekly with Monday first_day_of_week still parses cleanly.
        let weekly_monday_wkst = PatternedRecurrence {
            pattern: Some(RecurrencePattern {
                kind: Some(RecurrencePatternType::Weekly),
                interval: Some(1),
                days_of_week: vec![DayOfWeek::Monday],
                first_day_of_week: Some(DayOfWeek::Monday),
                ..RecurrencePattern::default()
            }),
            range: Some(range(date(2026, 11, 16))),
        };
        assert_eq!(
            Recurrence::from_graph(&weekly_monday_wkst),
            Some(rule(
                Frequency::Weekly(vec![Weekday::Monday]),
                1,
                End::Never
            ))
        );
    }
}
