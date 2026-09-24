//! Jalali (Solar Hijri, Shamsi) calendar conversions, ported from the
//! jalaali-js algorithm by Kazimierz M. Borkowski. Valid for Jalali years
//! -61 to 3177.

use chrono::{Datelike, Duration, NaiveDate};

pub const MONTHS: [&str; 12] = [
    "Farvardin",
    "Ordibehesht",
    "Khordad",
    "Tir",
    "Mordad",
    "Shahrivar",
    "Mehr",
    "Aban",
    "Azar",
    "Dey",
    "Bahman",
    "Esfand",
];

/// Jalali years in which the 33-year leap cycle shifts.
const BREAKS: [i32; 20] = [
    -61, 9, 38, 199, 426, 686, 756, 818, 1111, 1181, 1210, 1635, 2060, 2097, 2192, 2262, 2324,
    2394, 2456, 3178,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JalaliDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl JalaliDate {
    pub fn month_name(&self) -> &'static str {
        MONTHS[self.month as usize - 1]
    }
}

struct YearInfo {
    /// Years since the last leap year; 0 means this is a leap year.
    leap: i32,
    /// The Gregorian year in which this Jalali year starts.
    gregorian_year: i32,
    /// The day of March on which Farvardin 1 falls.
    march_day: i32,
}

fn year_info(year: i32) -> Option<YearInfo> {
    if year < BREAKS[0] || year >= BREAKS[BREAKS.len() - 1] {
        return None;
    }
    let gregorian_year = year + 621;
    let mut leap_jalali = -14;
    let mut previous = BREAKS[0];
    let mut jump = 0;
    for &next in &BREAKS[1..] {
        jump = next - previous;
        if year < next {
            break;
        }
        leap_jalali += jump / 33 * 8 + jump % 33 / 4;
        previous = next;
    }
    let mut n = year - previous;
    leap_jalali += n / 33 * 8 + (n % 33 + 3) / 4;
    if jump % 33 == 4 && jump - n == 4 {
        leap_jalali += 1;
    }
    let leap_gregorian = gregorian_year / 4 - (gregorian_year / 100 + 1) * 3 / 4 - 150;
    let march_day = 20 + leap_jalali - leap_gregorian;

    if jump - n < 6 {
        n = n - jump + (jump + 4) / 33 * 33;
    }
    let mut leap = ((n + 1) % 33 - 1) % 4;
    if leap == -1 {
        leap = 4;
    }
    Some(YearInfo {
        leap,
        gregorian_year,
        march_day,
    })
}

fn farvardin_first(info: &YearInfo) -> NaiveDate {
    NaiveDate::from_ymd_opt(info.gregorian_year, 3, info.march_day as u32)
        .expect("Farvardin 1 is always in March")
}

pub fn is_leap_year(year: i32) -> bool {
    year_info(year).is_some_and(|info| info.leap == 0)
}

pub fn from_gregorian(date: NaiveDate) -> Option<JalaliDate> {
    let mut year = date.year() - 621;
    let info = year_info(year)?;
    let mut days = (date - farvardin_first(&info)).num_days() as i32;
    if days >= 0 {
        if days <= 185 {
            // The first six months have 31 days.
            return Some(JalaliDate {
                year,
                month: 1 + days as u32 / 31,
                day: days as u32 % 31 + 1,
            });
        }
        days -= 186;
    } else {
        // Before Farvardin 1: the end of the previous Jalali year.
        year -= 1;
        days += 179;
        if info.leap == 1 {
            days += 1;
        }
    }
    Some(JalaliDate {
        year,
        month: 7 + days as u32 / 30,
        day: days as u32 % 30 + 1,
    })
}

pub fn to_gregorian(date: JalaliDate) -> Option<NaiveDate> {
    let month_days = match date.month {
        1..=6 => 31,
        7..=11 => 30,
        12 if is_leap_year(date.year) => 30,
        12 => 29,
        _ => return None,
    };
    if date.day < 1 || date.day > month_days {
        return None;
    }
    let info = year_info(date.year)?;
    let month = date.month as i64;
    let days = (month - 1) * 31 - month / 7 * (month - 7) + date.day as i64 - 1;
    Some(farvardin_first(&info) + Duration::days(days))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gregorian(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn jalali(year: i32, month: u32, day: u32) -> JalaliDate {
        JalaliDate { year, month, day }
    }

    #[test]
    fn converts_known_dates() {
        let cases = [
            (gregorian(2024, 3, 20), jalali(1403, 1, 1)), // Nowruz 1403
            (gregorian(2025, 3, 21), jalali(1404, 1, 1)), // Nowruz 1404
            (gregorian(2025, 3, 20), jalali(1403, 12, 30)), // 1403 is a leap year
            (gregorian(2024, 9, 22), jalali(1403, 7, 1)),
            (gregorian(1979, 2, 11), jalali(1357, 11, 22)),
            (gregorian(2000, 1, 1), jalali(1378, 10, 11)),
            (gregorian(1970, 1, 1), jalali(1348, 10, 11)),
        ];
        for (g, j) in cases {
            assert_eq!(from_gregorian(g), Some(j), "{g}");
            assert_eq!(to_gregorian(j), Some(g), "{j:?}");
        }
    }

    #[test]
    fn knows_leap_years() {
        assert!(is_leap_year(1403));
        assert!(!is_leap_year(1404));
        assert!(is_leap_year(1399));
        assert_eq!(to_gregorian(jalali(1404, 12, 30)), None);
        assert_eq!(to_gregorian(jalali(1403, 7, 31)), None);
        assert_eq!(to_gregorian(jalali(1403, 13, 1)), None);
        assert_eq!(to_gregorian(jalali(1403, 1, 0)), None);
    }

    #[test]
    fn round_trips_every_day_for_three_centuries() {
        let mut date = gregorian(1850, 1, 1);
        let mut previous: Option<JalaliDate> = None;
        while date < gregorian(2150, 1, 1) {
            let j = from_gregorian(date).unwrap();
            assert_eq!(to_gregorian(j), Some(date), "{date} -> {j:?}");
            // Consecutive days are consecutive Jalali days.
            if let Some(p) = previous {
                let next_day = p.day + 1 == j.day && p.month == j.month;
                let next_month = j.day == 1
                    && (p.month + 1 == j.month
                        || (p.month == 12 && j.month == 1 && j.year == p.year + 1));
                assert!(next_day || next_month, "{p:?} -> {j:?}");
            }
            previous = Some(j);
            date += Duration::days(1);
        }
    }
}
