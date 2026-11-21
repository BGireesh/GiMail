//! The rules that turn mail into the text Exo shows: dates, names, and the width estimates and
//! truncation that decide what fits in a row. Pure functions, tested on their own.

pub mod dates;
pub mod failure;
pub mod friendly;
mod lists;
pub mod locale;
pub(crate) mod measure;
pub mod names;
pub mod row;
pub mod urls;
mod widths;

/// `count` with its digits grouped as the Mac's locale groups them: `1,240`, `1.240`.
pub fn thousands(count: impl TryInto<u64>) -> String {
    // Every count Exo shows fits in 64 bits.
    locale::current().number(count.try_into().unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(thousands(7_usize), "7");
        assert_eq!(thousands(1240_u32), "1,240");
        assert_eq!(thousands(1_234_567_u64), "1,234,567");
        let dots = locale::Locale {
            grouping: locale::Grouping {
                separator: ".".into(),
                ..locale::Locale::en_us().grouping
            },
            ..locale::Locale::en_us()
        };
        assert_eq!(locale::pinned(dots, || thousands(1240_u64)), "1.240");
    }
}
