//! The Mac's locale, for the dates and counts that follow it: list dates (`NOV 17` or `17 NOV`),
//! which number comes first in a typed or detected `8/7`, Jump to Date's numeric dates, a reply
//! quote's time, and the grouping of a count's digits (`1,500` or `1.500`). Everything else is
//! written the US English way, whatever the locale.
//!
//! The app installs the Mac's locale at launch. Tests and screenshots get US English unless they
//! pin or install another, so they don't depend on the Mac they run on.

use std::{
    cell::RefCell,
    sync::{Arc, LazyLock, OnceLock},
};

use jiff::{Zoned, civil::Date};

/// How a locale writes the dates that follow it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locale {
    /// US English with the month first. Lists write `NOV 17` and `11/17/2026` in it, and
    /// `17 NOV` and `17/11/2026` in any other locale.
    pub en_us: bool,
    /// A date in numbers, as a Unicode date pattern (`M/d/y`, `dd/MM/y`).
    pub date_pattern: String,
    /// A time of day, as a pattern (`h:mm a`, `HH:mm`).
    pub time_pattern: String,
    /// What the time pattern's `a` writes before and after noon.
    pub am_pm: [String; 2],
    /// How a count's digits are grouped.
    pub grouping: Grouping,
}

/// How a locale groups a whole number's digits: `1,234,567`, `12,34,567`, `1.500`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grouping {
    /// What goes between groups: `,`, `.`, a no-break space.
    pub separator: String,
    /// The digits in the group nearest the units, then in each group before it: 3 and 3, or
    /// Indian English's 3 and 2. A first size of 0 leaves numbers ungrouped.
    pub sizes: [usize; 2],
    /// The fewest digits before the first separator: 1, or 2 where `1500` stays whole and
    /// `15.000` is grouped (Spanish).
    pub minimum: usize,
}

impl Grouping {
    /// Groups of three, split by `separator`.
    fn threes(separator: &str) -> Self {
        Self {
            separator: separator.into(),
            sizes: [3, 3],
            minimum: 1,
        }
    }
}

impl Locale {
    /// US English: `11/17/2026` and `3:45 PM`.
    pub fn en_us() -> Self {
        Self {
            en_us: true,
            date_pattern: "M/d/y".into(),
            time_pattern: "h:mm a".into(),
            am_pm: ["AM".into(), "PM".into()],
            grouping: Grouping::threes(","),
        }
    }

    /// The Mac's locale, with the date format, the 12- or 24-hour time and the number format chosen
    /// in its Language & Region settings.
    pub fn system() -> Self {
        #[cfg(target_os = "macos")]
        return macos::current();
        #[cfg(not(target_os = "macos"))]
        Self::en_us()
    }

    /// The locale `identifier` names (`en_GB`), as macOS writes it with no settings changed.
    pub fn named(identifier: &str) -> Self {
        #[cfg(target_os = "macos")]
        return macos::named(identifier);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = identifier;
            Self::en_us()
        }
    }

    /// Whether the month comes before the day in the locale's numeric dates, so `8/7` is August
    /// 7th. A year first doesn't count: `y-MM-dd` is month first.
    pub fn month_first(&self) -> bool {
        let mut fields = Vec::new();
        render(&self.date_pattern, |letter, _| {
            fields.push(letter);
            String::new()
        });
        let month = fields
            .iter()
            .position(|&letter| matches!(letter, 'M' | 'L'));
        let day = fields.iter().position(|&letter| letter == 'd');
        month.zip(day).is_none_or(|(month, day)| month < day)
    }

    /// `date` in numbers: `11/23/2026`, `23/11/2026`, `2026-11-23`.
    pub fn date(&self, date: Date) -> String {
        render(&self.date_pattern, |letter, width| match letter {
            'y' if width == 2 => format!("{:02}", date.year().rem_euclid(100)),
            'y' => padded(date.year().into(), width),
            'M' | 'L' => padded(date.month().into(), width),
            'd' => padded(date.day().into(), width),
            _ => String::new(),
        })
    }

    /// `at`'s time of day: `3:45 PM`, `15:45`.
    pub fn time(&self, at: &Zoned) -> String {
        let hour = i32::from(at.hour());
        render(&self.time_pattern, |letter, width| match letter {
            'h' => padded(if hour % 12 == 0 { 12 } else { hour % 12 }, width),
            'H' => padded(hour, width),
            'K' => padded(hour % 12, width),
            'k' => padded(if hour == 0 { 24 } else { hour }, width),
            'm' => padded(at.minute().into(), width),
            's' => padded(at.second().into(), width),
            'a' | 'b' | 'B' => self.am_pm[usize::from(hour >= 12)].clone(),
            _ => String::new(),
        })
    }

    /// `count` with its digits grouped: `1,500`, `1.500`, `12,34,567`.
    pub fn number(&self, count: u64) -> String {
        let digits = count.to_string();
        let Grouping {
            separator,
            sizes: [first, rest],
            minimum,
        } = &self.grouping;
        if *first == 0 || digits.len() < first + minimum {
            return digits;
        }
        let mut groups = Vec::new();
        let (mut ungrouped, mut size) = (digits.as_str(), *first);
        while ungrouped.len() > size {
            let (before, group) = ungrouped.split_at(ungrouped.len() - size);
            groups.push(group);
            (ungrouped, size) = (before, *rest);
        }
        groups.push(ungrouped);
        groups.reverse();
        groups.join(separator)
    }
}

/// `number` with zeros in front up to `width` digits.
fn padded(number: i32, width: usize) -> String {
    format!("{number:0width$}")
}

/// Writes a Unicode date pattern. Each run of one letter is a field, which `field` writes from the
/// letter and the run's length. Text in single quotes is literal (`''` is a quote), as is
/// everything that isn't a letter.
fn render(pattern: &str, mut field: impl FnMut(char, usize) -> String) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.next_if_eq(&'\'').is_some() {
                out.push('\'');
            } else {
                quoted = !quoted;
            }
        } else if quoted || !c.is_ascii_alphabetic() {
            out.push(c);
        } else {
            let mut width = 1;
            while chars.next_if_eq(&c).is_some() {
                width += 1;
            }
            out.push_str(&field(c, width));
        }
    }
    out
}

/// The locale installed for the whole app, at launch.
static INSTALLED: OnceLock<Arc<Locale>> = OnceLock::new();

static EN_US: LazyLock<Arc<Locale>> = LazyLock::new(|| Arc::new(Locale::en_us()));

thread_local! {
    /// The locale a test pinned on its thread.
    static PINNED: RefCell<Option<Arc<Locale>>> = const { RefCell::new(None) };
}

/// Makes `locale` the app's, for every thread. Only the first call counts.
pub fn install(locale: Locale) {
    let _ = INSTALLED.set(Arc::new(locale));
}

/// The locale dates follow: the one pinned on this thread, else the installed one, else US
/// English.
pub fn current() -> Arc<Locale> {
    PINNED
        .with_borrow(Clone::clone)
        .or_else(|| INSTALLED.get().cloned())
        .unwrap_or_else(|| Arc::clone(&EN_US))
}

/// Runs `run` with `locale` as this thread's, for tests.
pub fn pinned<T>(locale: Locale, run: impl FnOnce() -> T) -> T {
    let before = PINNED.replace(Some(Arc::new(locale)));
    let result = run();
    PINNED.set(before);
    result
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_foundation::{
        NSDateFormatter, NSDateFormatterStyle, NSLocale, NSNumberFormatter, NSNumberFormatterStyle,
        NSString, ns_string,
    };

    use super::{Grouping, Locale};

    pub(super) fn current() -> Locale {
        read(&NSLocale::currentLocale())
    }

    pub(super) fn named(identifier: &str) -> Locale {
        read(&NSLocale::localeWithLocaleIdentifier(&NSString::from_str(
            identifier,
        )))
    }

    fn formatter(
        locale: &NSLocale,
        date: NSDateFormatterStyle,
        time: NSDateFormatterStyle,
    ) -> String {
        let formatter = NSDateFormatter::new();
        formatter.setLocale(Some(locale));
        formatter.setDateStyle(date);
        formatter.setTimeStyle(time);
        formatter.dateFormat().to_string()
    }

    fn read(locale: &NSLocale) -> Locale {
        let identifier = NSLocale::canonicalLocaleIdentifierFromString(&locale.localeIdentifier());
        let short_date = |locale: &NSLocale| {
            formatter(
                locale,
                NSDateFormatterStyle::ShortStyle,
                NSDateFormatterStyle::NoStyle,
            )
        };
        // A date format chosen in Language & Region shows only in the current locale's short
        // style, which then differs from the same locale's untouched one. Dates take it with the
        // year in full. Otherwise they take the locale's own numeric date.
        let chosen = short_date(locale);
        let date_pattern =
            if chosen != short_date(&NSLocale::localeWithLocaleIdentifier(&identifier)) {
                if chosen.contains("yyy") {
                    chosen
                } else {
                    chosen.replace("yy", "y")
                }
            } else {
                NSDateFormatter::dateFormatFromTemplate_options_locale(
                    ns_string!("yMd"),
                    0,
                    Some(locale),
                )
                .map_or_else(|| "M/d/y".into(), |pattern| pattern.to_string())
            };
        // The short time style follows the 12- or 24-hour choice.
        let time_pattern = formatter(
            locale,
            NSDateFormatterStyle::NoStyle,
            NSDateFormatterStyle::ShortStyle,
        );
        let symbols = NSDateFormatter::new();
        symbols.setLocale(Some(locale));
        let mut found = Locale {
            en_us: false,
            // macOS puts a narrow no-break space before `AM`. Exo's times have a plain one.
            date_pattern: date_pattern.replace('\u{202f}', " "),
            time_pattern: time_pattern.replace('\u{202f}', " "),
            am_pm: [
                symbols.AMSymbol().to_string(),
                symbols.PMSymbol().to_string(),
            ],
            grouping: grouping(locale),
        };
        found.en_us =
            identifier.to_string().split('@').next() == Some("en_US") && found.month_first();
        found
    }

    /// The locale's digit grouping, with the separator chosen in Language & Region.
    fn grouping(locale: &NSLocale) -> Grouping {
        let formatter = NSNumberFormatter::new();
        formatter.setLocale(Some(locale));
        formatter.setNumberStyle(NSNumberFormatterStyle::DecimalStyle);
        let first = if formatter.usesGroupingSeparator() {
            formatter.groupingSize()
        } else {
            0
        };
        // A second size of 0 means groups all of the first's size.
        let rest = match formatter.secondaryGroupingSize() {
            0 => first,
            rest => rest,
        };
        Grouping {
            separator: formatter.groupingSeparator().to_string(),
            sizes: [first, rest],
            minimum: usize::try_from(formatter.minimumGroupingDigits()).map_or(1, |n| n.max(1)),
        }
    }
}

#[cfg(test)]
impl Locale {
    /// British English, written out so tests don't depend on the Mac's copy of the locale data.
    pub(crate) fn en_gb() -> Self {
        Self {
            en_us: false,
            date_pattern: "dd/MM/y".into(),
            time_pattern: "HH:mm".into(),
            am_pm: ["am".into(), "pm".into()],
            grouping: Grouping::threes(","),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: i8, minute: i8) -> Zoned {
        jiff::civil::date(2026, 11, 17)
            .at(hour, minute, 12, 0)
            .in_tz("America/New_York")
            .expect("a time")
    }

    #[test]
    fn us_english_dates_and_times() {
        let us = Locale::en_us();
        assert_eq!(us.date(jiff::civil::date(2026, 12, 1)), "12/1/2026");
        assert_eq!(us.time(&at(15, 45)), "3:45 PM");
        assert_eq!(us.time(&at(0, 5)), "12:05 AM");
        assert!(us.month_first());
    }

    #[test]
    fn other_locales_dates_and_times() {
        let gb = Locale::en_gb();
        assert_eq!(gb.date(jiff::civil::date(2026, 12, 1)), "01/12/2026");
        assert_eq!(gb.time(&at(9, 5)), "09:05");
        assert!(!gb.month_first());
        let korean = Locale {
            en_us: false,
            date_pattern: "y. M. d.".into(),
            time_pattern: "a h:mm".into(),
            am_pm: ["오전".into(), "오후".into()],
            grouping: Grouping::threes(","),
        };
        assert_eq!(korean.date(jiff::civil::date(2026, 3, 7)), "2026. 3. 7.");
        assert_eq!(korean.time(&at(15, 45)), "오후 3:45");
        assert!(
            korean.month_first(),
            "a year first leaves the month before the day"
        );
        let quoted = Locale {
            date_pattern: "d 'de' M 'o''clock' yy".into(),
            ..Locale::en_gb()
        };
        assert_eq!(
            quoted.date(jiff::civil::date(2026, 3, 7)),
            "7 de 3 o'clock 26"
        );
    }

    /// Each as Chromium writes `1500`, `15000` and `1234567` in that language.
    #[test]
    fn counts_group_their_digits_as_the_locale_does() {
        let counts = |grouping: Grouping| {
            let locale = Locale {
                grouping,
                ..Locale::en_us()
            };
            [0, 999, 1500, 15_000, 1_234_567].map(|count| locale.number(count))
        };
        assert_eq!(
            counts(Locale::en_us().grouping),
            ["0", "999", "1,500", "15,000", "1,234,567"]
        );
        assert_eq!(
            counts(Grouping::threes(".")),
            ["0", "999", "1.500", "15.000", "1.234.567"]
        );
        let spanish = Grouping {
            minimum: 2,
            ..Grouping::threes(".")
        };
        assert_eq!(counts(spanish), ["0", "999", "1500", "15.000", "1.234.567"]);
        let indian = Grouping {
            sizes: [3, 2],
            ..Grouping::threes(",")
        };
        assert_eq!(counts(indian), ["0", "999", "1,500", "15,000", "12,34,567"]);
        let french = Grouping::threes("\u{202f}");
        assert_eq!(counts(french)[4], "1\u{202f}234\u{202f}567");
        let none = Grouping {
            sizes: [0, 0],
            ..Grouping::threes(",")
        };
        assert_eq!(counts(none)[4], "1234567");
    }

    #[test]
    fn a_pinned_locale_holds_on_its_thread_only() {
        assert!(current().en_us);
        pinned(Locale::en_gb(), || {
            assert!(!current().en_us);
            assert!(
                std::thread::spawn(|| current().en_us)
                    .join()
                    .expect("joins")
            );
        });
        assert!(current().en_us);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn named_locales_read_from_macos() {
        assert_eq!(Locale::named("en_US"), Locale::en_us());
        let gb = Locale::named("en_GB");
        assert_eq!((gb.en_us, gb.month_first()), (false, false));
        assert_eq!(gb.date(jiff::civil::date(2026, 3, 7)), "07/03/2026");
        assert_eq!(gb.time(&at(15, 45)), "15:45");
        let canada = Locale::named("en_CA");
        assert_eq!((canada.en_us, canada.month_first()), (false, true));
        let counts = |identifier: &str| Locale::named(identifier).number(1_234_567);
        assert_eq!(counts("de_DE"), "1.234.567");
        assert_eq!(counts("en_IN"), "12,34,567");
        assert_eq!(Locale::named("es_ES").number(1500), "1500");
    }
}
