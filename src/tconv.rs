use crate::jalali::{self, JalaliDate};
use crate::utils;
use chrono::{DateTime, Local, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_humanize::HumanTime;
use clap::Parser;
use std::io::{self, Read};
use std::process::exit;

#[derive(Parser, Debug)]
pub struct Args {
    /// Unix timestamp, ISO8601/RFC3339, "YYYY-MM-DD[ HH:MM:SS[.f]]", or "now".
    /// If omitted, reads from stdin (pipe)
    input: Option<String>,

    /// Read the input as a Jalali (Shamsi) date in local time: 1403-07-02, 1403/07/02 14:30
    #[arg(short, long)]
    jalali: bool,
}

pub fn run(args: &Args, copy: bool) {
    let input = match &args.input {
        Some(s) => s.clone(),
        None => read_stdin(),
    };
    let input = input.trim();

    let dt = if args.jalali {
        parse_jalali(input).unwrap_or_else(|| {
            eprintln!("Error: {input:?} isn't a Jalali date like 1403-07-02 or 1403/07/02 14:30");
            exit(1);
        })
    } else if input.eq_ignore_ascii_case("now") {
        Utc::now()
    } else if let Some(dt) = parse_numeric(input) {
        dt
    } else if let Some(dt) = parse_datetime(input) {
        dt
    } else {
        eprintln!("Error: unable to parse input {:?}", input);
        exit(1);
    };

    utils::emit(&format_output(input, dt), copy).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        exit(1);
    });
}

fn read_stdin() -> String {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
        eprintln!("Error: failed to read stdin: {}", e);
        exit(1);
    });
    buf
}

/// Detects unit by digit count: 10=s, 13=ms, 16=us, 19=ns.
fn parse_numeric(input: &str) -> Option<DateTime<Utc>> {
    let value: i128 = input.parse().ok()?;

    match input.len() {
        10 => DateTime::from_timestamp(value as i64, 0),
        13 => DateTime::from_timestamp_millis(value as i64),
        16 => {
            let secs = value / 1_000_000;
            let nanos = ((value % 1_000_000) * 1000) as u32;
            DateTime::from_timestamp(secs as i64, nanos)
        }
        19 => {
            let secs = value / 1_000_000_000;
            let nanos = (value % 1_000_000_000) as u32;
            DateTime::from_timestamp(secs as i64, nanos)
        }
        _ => None,
    }
}

fn parse_datetime(input: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Some(dt.with_timezone(&Utc));
    }

    let layouts = ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"];

    for layout in layouts {
        if let Ok(dt) = NaiveDateTime::parse_from_str(input, layout) {
            let local = Local.from_local_datetime(&dt).single()?;
            return Some(local.with_timezone(&Utc));
        }
    }

    if let Ok(date) = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        let dt = date.and_hms_opt(0, 0, 0)?;
        let local = Local.from_local_datetime(&dt).single()?;
        return Some(local.with_timezone(&Utc));
    }

    None
}

/// A Jalali date, optionally with a time (HH:MM or HH:MM:SS), in local time.
fn parse_jalali(input: &str) -> Option<DateTime<Utc>> {
    let (date, time) = match input.split_once([' ', 'T']) {
        Some((date, time)) => (date, Some(time.trim())),
        None => (input, None),
    };
    let [year, month, day] = date.split(['-', '/']).collect::<Vec<_>>()[..] else {
        return None;
    };
    let date = jalali::to_gregorian(JalaliDate {
        year: year.parse().ok()?,
        month: month.parse().ok()?,
        day: day.parse().ok()?,
    })?;
    let time = match time {
        None => NaiveTime::MIN,
        Some(time) => NaiveTime::parse_from_str(time, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(time, "%H:%M"))
            .ok()?,
    };
    let local = Local.from_local_datetime(&date.and_time(time)).single()?;
    Some(local.with_timezone(&Utc))
}

fn format_jalali(local: DateTime<Local>) -> String {
    match jalali::from_gregorian(local.date_naive()) {
        Some(date) => format!(
            "{:04}-{:02}-{:02} {} ({} {} {})",
            date.year,
            date.month,
            date.day,
            local.format("%H:%M:%S"),
            date.day,
            date.month_name(),
            date.year
        ),
        None => "out of the supported range".to_string(),
    }
}

fn format_output(input: &str, dt: DateTime<Utc>) -> String {
    let local = dt.with_timezone(&Local);

    let micros = dt.timestamp() as i128 * 1_000_000 + dt.timestamp_subsec_micros() as i128;
    let nanos = dt.timestamp() as i128 * 1_000_000_000 + dt.timestamp_subsec_nanos() as i128;

    format!(
        "Input\n  {}\n\nUTC\n  {}\n\nLocal\n  {}\n\nJalali\n  {}\n\nISO8601\n  {}\n  {}\n\nUnix\n  Seconds      : {}\n  Milliseconds : {}\n  Microseconds : {}\n  Nanoseconds  : {}\n\nRelative\n  {}",
        input,
        dt.format("%Y-%m-%d %H:%M:%S%.f UTC"),
        local.format("%Y-%m-%d %H:%M:%S%.f %Z"),
        format_jalali(local),
        dt.to_rfc3339(),
        local.to_rfc3339(),
        dt.timestamp(),
        dt.timestamp_millis(),
        micros,
        nanos,
        HumanTime::from(dt)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seconds_millis_micros_nanos() {
        assert!(parse_numeric("1700000000").is_some());
        assert!(parse_numeric("1700000000000").is_some());
        assert!(parse_numeric("1700000000000000").is_some());
        assert!(parse_numeric("1700000000000000000").is_some());
        assert!(parse_numeric("bad").is_none());
    }

    #[test]
    fn parses_jalali_dates_in_local_time() {
        let local = |y, m, d, h, min| {
            Local
                .with_ymd_and_hms(y, m, d, h, min, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc)
        };
        assert_eq!(parse_jalali("1403-07-02"), Some(local(2024, 9, 23, 0, 0)));
        assert_eq!(
            parse_jalali("1403/07/02 14:30"),
            Some(local(2024, 9, 23, 14, 30))
        );
        assert_eq!(
            parse_jalali("1403-7-2T14:30:05").map(|d| d.timestamp() % 60),
            Some(5)
        );
        assert_eq!(parse_jalali("1404-12-30"), None, "1404 isn't a leap year");
        assert_eq!(parse_jalali("1403-07"), None);
        assert_eq!(parse_jalali("1403-07-02 25:00"), None);
    }

    #[test]
    fn shows_the_jalali_date() {
        let local = Local
            .with_ymd_and_hms(2024, 3, 20, 9, 5, 0)
            .single()
            .unwrap();
        assert_eq!(
            format_jalali(local),
            "1403-01-01 09:05:00 (1 Farvardin 1403)"
        );
    }

    #[test]
    fn parses_rfc3339_and_date_only() {
        assert!(parse_datetime("2024-01-02T03:04:05Z").is_some());
        assert!(parse_datetime("2024-01-02").is_some());
        assert!(parse_datetime("not-a-date").is_none());
    }
}
