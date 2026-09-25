use crate::jalali::{self, JalaliDate};
use crate::utils;
use crate::zones::{self, Zone};
use chrono::{DateTime, Duration, FixedOffset, Local, NaiveDateTime, NaiveTime, Utc};
use chrono_humanize::HumanTime;
use clap::Parser;
use jiff::Span;
use std::io::{self, Read};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools tconv 1700000000
  rtools tconv now --tz Asia/Tehran --tz America/New_York
  rtools tconv '2026-03-20 09:00' --tz Europe/Berlin    09:00 in Berlin
  rtools tconv 'now + 90m'
  rtools tconv 'tomorrow + 9h' --tz Asia/Tehran
  rtools tconv -j '1405/01/01 - 1d'

Date math adds or subtracts durations like 90m, 1h30m, 2d, 1w, 3mo or 1y
(units: y, mo, w, d, h, m, s). Days, months and years follow the calendar, so
+ 1d keeps the time of day across a daylight-saving change.")]
pub struct Args {
    /// Unix timestamp, ISO8601/RFC3339, "YYYY-MM-DD[ HH:MM[:SS[.f]]]", now, today, tomorrow or
    /// yesterday, with optional date math like "+ 90m" or "- 2d". If omitted, reads from stdin (pipe)
    #[arg(allow_hyphen_values = true)]
    input: Option<String>,

    /// Read the input as a Jalali (Shamsi) date: 1403-07-02, 1403/07/02 14:30
    #[arg(short, long)]
    jalali: bool,

    /// Also show the time in this zone, like Asia/Tehran or UTC. Repeat for several.
    /// Dates without an offset are read in the first one instead of local time
    #[arg(long, value_name = "ZONE")]
    tz: Vec<String>,
}

pub fn run(args: &Args, copy: bool) {
    let input = match &args.input {
        Some(s) => s.clone(),
        None => read_stdin(),
    };
    let input = input.trim();
    let zones: Vec<Zone> = args
        .tz
        .iter()
        .map(|name| zones::parse(name))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| fail(&e));
    // Where dates without an offset, `today` and calendar math happen.
    let home = zones.first().cloned().unwrap_or_else(Zone::system);

    let dt = parse_input(input, args.jalali, &home, Utc::now()).unwrap_or_else(|e| fail(&e));
    utils::emit(&format_output(input, dt, &zones), copy).unwrap_or_else(|e| fail(&e));
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

fn read_stdin() -> String {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
        eprintln!("Error: failed to read stdin: {}", e);
        exit(1);
    });
    buf
}

/// The input's time: a date, timestamp or keyword, then any date math.
fn parse_input(
    input: &str,
    jalali: bool,
    home: &Zone,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let (base, math) = split_math(input);
    let Some(mut dt) = parse_base(base, jalali, home, now) else {
        let mut message = if jalali {
            format!("{base:?} isn't a Jalali date like 1403-07-02 or 1403/07/02 14:30")
        } else {
            format!("unable to parse input {input:?}")
        };
        if input.contains(['+', '-']) {
            message.push_str(
                "\nDate math looks like 'now + 90m' or '2026-01-01 - 2d' (units: y, mo, w, d, h, m, s)",
            );
        }
        return Err(message);
    };
    for (subtract, span) in math {
        let zoned = zones::timestamp(dt)
            .ok_or("the date is out of range")?
            .to_zoned(home.clone());
        let result = if subtract {
            zoned.checked_sub(span)
        } else {
            zoned.checked_add(span)
        };
        dt = zones::from_timestamp(
            result
                .map_err(|_| "the result is out of range")?
                .timestamp(),
        );
    }
    if zones::timestamp(dt).is_none() {
        return Err("the date is out of range".to_string());
    }
    Ok(dt)
}

fn parse_base(base: &str, jalali: bool, home: &Zone, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let midnight = |days: i64| {
        let today = zones::wall_time(home, now).date();
        from_wall(
            (today + Duration::days(days)).and_time(NaiveTime::MIN),
            home,
        )
    };
    match base.to_ascii_lowercase().as_str() {
        "" | "now" => return Some(now),
        "today" => return midnight(0),
        "tomorrow" => return midnight(1),
        "yesterday" => return midnight(-1),
        _ => {}
    }
    if jalali {
        return parse_jalali(base, home);
    }
    parse_numeric(base).or_else(|| parse_datetime(base, home))
}

/// Splits trailing date math off the input: `now + 1d - 2h` is `now`, then +1d and -2h.
fn split_math(input: &str) -> (&str, Vec<(bool, Span)>) {
    let mut rest = input.trim();
    let mut math = Vec::new();
    // Peel terms off the end; a date's own dashes and offsets aren't durations.
    while let Some(at) = rest.rfind(['+', '-']) {
        let Some(span) = parse_duration(&rest[at + 1..]) else {
            break;
        };
        math.push((rest[at..].starts_with('-'), span));
        rest = rest[..at].trim_end();
    }
    math.reverse();
    (rest, math)
}

/// A duration like 90m, 1h30m, 2 days or 1y 6mo.
fn parse_duration(text: &str) -> Option<Span> {
    const UNITS: [&[&str]; 7] = [
        &["y", "yr", "yrs", "year", "years"],
        &["mo", "mon", "mons", "month", "months"],
        &["w", "wk", "wks", "week", "weeks"],
        &["d", "day", "days"],
        &["h", "hr", "hrs", "hour", "hours"],
        &["m", "min", "mins", "minute", "minutes"],
        &["s", "sec", "secs", "second", "seconds"],
    ];
    let mut amounts = [0i64; 7];
    let mut rest = text.trim();
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let amount: i64 = rest[..digits].parse().ok()?;
        rest = rest[digits..].trim_start();
        let letters = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = rest[..letters].to_ascii_lowercase();
        let i = UNITS
            .iter()
            .position(|names| names.contains(&unit.as_str()))?;
        amounts[i] = amounts[i].checked_add(amount)?;
        rest = rest[letters..].trim_start();
    }
    let [years, months, weeks, days, hours, minutes, seconds] = amounts;
    Span::new()
        .try_years(years)
        .and_then(|s| s.try_months(months))
        .and_then(|s| s.try_weeks(weeks))
        .and_then(|s| s.try_days(days))
        .and_then(|s| s.try_hours(hours))
        .and_then(|s| s.try_minutes(minutes))
        .and_then(|s| s.try_seconds(seconds))
        .ok()
}

/// A wall-clock time in `zone`. A time a daylight-saving change skips or repeats
/// resolves the usual way: to the later clock, and to the first pass.
fn from_wall(wall: NaiveDateTime, zone: &Zone) -> Option<DateTime<Utc>> {
    let zoned = zone.to_zoned(zones::civil(wall)?).ok()?;
    Some(zones::from_timestamp(zoned.timestamp()))
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

/// RFC 3339, or a date and time without an offset, which is read in `zone`.
fn parse_datetime(input: &str, zone: &Zone) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Some(dt.with_timezone(&Utc));
    }

    let layouts = [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ];
    for layout in layouts {
        if let Ok(dt) = NaiveDateTime::parse_from_str(input, layout) {
            return from_wall(dt, zone);
        }
    }

    if let Ok(date) = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return from_wall(date.and_time(NaiveTime::MIN), zone);
    }

    None
}

/// A Jalali date, optionally with a time (HH:MM or HH:MM:SS), read in `zone`.
fn parse_jalali(input: &str, zone: &Zone) -> Option<DateTime<Utc>> {
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
    from_wall(date.and_time(time), zone)
}

fn format_jalali(wall: NaiveDateTime) -> String {
    match jalali::from_gregorian(wall.date()) {
        Some(date) => format!(
            "{:04}-{:02}-{:02} {} ({} {} {})",
            date.year,
            date.month,
            date.day,
            wall.format("%H:%M:%S"),
            date.day,
            date.month_name(),
            date.year
        ),
        None => "out of the supported range".to_string(),
    }
}

/// The time in a zone, with the zone's abbreviation when it has one, like CEST.
fn in_zone(zone: &Zone, dt: DateTime<Utc>) -> (DateTime<FixedOffset>, String) {
    let offset = FixedOffset::east_opt(zones::offset_seconds(zone, dt)).expect("a valid offset");
    let there = dt.with_timezone(&offset);
    let mut text = there.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string();
    let at = zones::timestamp(dt).expect("a time in range");
    let abbreviation = zone.to_offset_info(at).abbreviation().to_string();
    if abbreviation.chars().any(|c| c.is_ascii_alphabetic()) {
        text = format!("{text} {abbreviation}");
    }
    (there, text)
}

/// `zones` are the ones asked for with --tz; the first also gets the Jalali date.
fn format_output(input: &str, dt: DateTime<Utc>, zones: &[Zone]) -> String {
    let local = dt.with_timezone(&Local);

    let micros = dt.timestamp() as i128 * 1_000_000 + dt.timestamp_subsec_micros() as i128;
    let nanos = dt.timestamp() as i128 * 1_000_000_000 + dt.timestamp_subsec_nanos() as i128;

    let mut out = format!(
        "Input\n  {input}\n\nUTC\n  {}",
        dt.format("%Y-%m-%d %H:%M:%S%.f UTC")
    );
    let mut iso = vec![dt.to_rfc3339()];
    for zone in zones {
        let (there, text) = in_zone(zone, dt);
        out.push_str(&format!("\n\n{}\n  {text}", zones::name(zone)));
        iso.push(there.to_rfc3339());
    }
    out.push_str(&format!(
        "\n\nLocal\n  {}",
        local.format("%Y-%m-%d %H:%M:%S%.f %Z")
    ));
    iso.push(local.to_rfc3339());

    let (jalali_label, jalali_wall) = match zones.first() {
        Some(zone) => (
            format!("Jalali ({})", zones::name(zone)),
            zones::wall_time(zone, dt),
        ),
        None => ("Jalali".to_string(), local.naive_local()),
    };
    out.push_str(&format!(
        "\n\n{jalali_label}\n  {}",
        format_jalali(jalali_wall)
    ));
    out.push_str(&format!("\n\nISO8601\n  {}", iso.join("\n  ")));
    out.push_str(&format!(
        "\n\nUnix\n  Seconds      : {}\n  Milliseconds : {}\n  Microseconds : {}\n  Nanoseconds  : {}\n\nRelative\n  {}",
        dt.timestamp(),
        dt.timestamp_millis(),
        micros,
        nanos,
        HumanTime::from(dt)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(name: &str) -> Zone {
        Zone::get(name).unwrap()
    }

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .and_then(|date| date.and_hms_opt(h, min, 0))
            .unwrap()
            .and_utc()
    }

    /// Parses `input` as if it were 2026-09-24 23:44 UTC, in `home`.
    fn parse(input: &str, home: &str) -> Result<DateTime<Utc>, String> {
        parse_input(input, false, &zone(home), utc(2026, 9, 24, 23, 44))
    }

    #[test]
    fn parses_seconds_millis_micros_nanos() {
        assert!(parse_numeric("1700000000").is_some());
        assert!(parse_numeric("1700000000000").is_some());
        assert!(parse_numeric("1700000000000000").is_some());
        assert!(parse_numeric("1700000000000000000").is_some());
        assert!(parse_numeric("bad").is_none());
    }

    #[test]
    fn parses_jalali_dates_in_the_given_zone() {
        let tehran = zone("Asia/Tehran");
        // Tehran is UTC+03:30.
        assert_eq!(
            parse_jalali("1403-07-02", &tehran),
            Some(utc(2024, 9, 22, 20, 30))
        );
        assert_eq!(
            parse_jalali("1403/07/02 14:30", &tehran),
            Some(utc(2024, 9, 23, 11, 0))
        );
        assert_eq!(
            parse_jalali("1403-7-2T14:30:05", &tehran).map(|d| d.timestamp() % 60),
            Some(5)
        );
        assert_eq!(
            parse_jalali("1404-12-30", &tehran),
            None,
            "1404 isn't a leap year"
        );
        assert_eq!(parse_jalali("1403-07", &tehran), None);
        assert_eq!(parse_jalali("1403-07-02 25:00", &tehran), None);
    }

    #[test]
    fn shows_the_jalali_date() {
        let wall = chrono::NaiveDate::from_ymd_opt(2024, 3, 20)
            .and_then(|date| date.and_hms_opt(9, 5, 0))
            .unwrap();
        assert_eq!(
            format_jalali(wall),
            "1403-01-01 09:05:00 (1 Farvardin 1403)"
        );
    }

    #[test]
    fn parses_rfc3339_and_dates_in_the_given_zone() {
        let berlin = zone("Europe/Berlin");
        assert_eq!(
            parse_datetime("2024-01-02T03:04:00Z", &berlin),
            Some(utc(2024, 1, 2, 3, 4))
        );
        // Offsets win over the zone.
        assert_eq!(
            parse_datetime("2024-07-02T03:04:00+03:30", &berlin),
            Some(utc(2024, 7, 1, 23, 34))
        );
        // Berlin is UTC+1 in winter and UTC+2 in summer.
        assert_eq!(
            parse_datetime("2024-01-02", &berlin),
            Some(utc(2024, 1, 1, 23, 0))
        );
        assert_eq!(
            parse_datetime("2024-07-02 09:30", &berlin),
            Some(utc(2024, 7, 2, 7, 30))
        );
        assert_eq!(
            parse_datetime("2024-07-02T09:30:00", &berlin),
            Some(utc(2024, 7, 2, 7, 30))
        );
        // 02:30 on 2024-03-31 doesn't exist in Berlin: it reads as 03:30.
        assert_eq!(
            parse_datetime("2024-03-31 02:30", &berlin),
            Some(utc(2024, 3, 31, 1, 30))
        );
        assert!(parse_datetime("not-a-date", &berlin).is_none());
    }

    #[test]
    fn reads_keywords_in_the_home_zone() {
        assert_eq!(parse("now", "UTC"), Ok(utc(2026, 9, 24, 23, 44)));
        assert_eq!(parse("NOW", "UTC"), Ok(utc(2026, 9, 24, 23, 44)));
        assert_eq!(parse("today", "UTC"), Ok(utc(2026, 9, 24, 0, 0)));
        // In Tehran it's already 03:14 on the 25th.
        assert_eq!(parse("today", "Asia/Tehran"), Ok(utc(2026, 9, 24, 20, 30)));
        assert_eq!(parse("tomorrow", "UTC"), Ok(utc(2026, 9, 25, 0, 0)));
        assert_eq!(parse("yesterday", "UTC"), Ok(utc(2026, 9, 23, 0, 0)));
    }

    #[test]
    fn does_date_math() {
        assert_eq!(parse("now + 90m", "UTC"), Ok(utc(2026, 9, 25, 1, 14)));
        assert_eq!(parse("now+90m", "UTC"), Ok(utc(2026, 9, 25, 1, 14)));
        assert_eq!(parse("+1h30m", "UTC"), Ok(utc(2026, 9, 25, 1, 14)));
        assert_eq!(parse("now - 2d", "UTC"), Ok(utc(2026, 9, 22, 23, 44)));
        assert_eq!(parse("-2d", "UTC"), Ok(utc(2026, 9, 22, 23, 44)));
        assert_eq!(parse("now + 1d - 2h", "UTC"), Ok(utc(2026, 9, 25, 21, 44)));
        assert_eq!(
            parse("now + 1 hour 30 minutes", "UTC"),
            Ok(utc(2026, 9, 25, 1, 14))
        );
        assert_eq!(parse("now + 1w", "UTC"), Ok(utc(2026, 10, 1, 23, 44)));
        assert_eq!(
            parse("tomorrow + 9h", "Asia/Tehran"),
            Ok(utc(2026, 9, 26, 5, 30))
        );
        assert_eq!(parse("2026-01-01 - 1d", "UTC"), Ok(utc(2025, 12, 31, 0, 0)));
        assert_eq!(parse("2026-01-01-1d", "UTC"), Ok(utc(2025, 12, 31, 0, 0)));
        assert_eq!(
            parse("2026-01-31T10:00:00Z + 1mo", "UTC"),
            Ok(utc(2026, 2, 28, 10, 0)),
            "months clamp to the end of the month"
        );
        assert_eq!(parse("2024-02-29 + 1y", "UTC"), Ok(utc(2025, 2, 28, 0, 0)));
        assert_eq!(
            parse("1700000000 + 1h", "UTC"),
            Ok(utc(2023, 11, 14, 23, 13) + Duration::seconds(20))
        );
        // An offset isn't date math.
        assert_eq!(
            parse("2026-09-25T09:00:00-05:00", "UTC"),
            Ok(utc(2026, 9, 25, 14, 0))
        );
        assert_eq!(
            parse("2026-09-25T09:00:00-05:00 + 1h", "UTC"),
            Ok(utc(2026, 9, 25, 15, 0))
        );
        let jalali = parse_input(
            "1405/01/01 - 1d",
            true,
            &zone("Asia/Tehran"),
            utc(2026, 9, 24, 23, 44),
        );
        assert_eq!(jalali, Ok(utc(2026, 3, 19, 20, 30)));
    }

    #[test]
    fn days_follow_the_calendar_across_daylight_saving() {
        // Berlin moves its clocks forward on 2026-03-29: a day later is still 12:00,
        // but only 23 hours later.
        assert_eq!(
            parse("2026-03-28 12:00 + 1d", "Europe/Berlin"),
            Ok(utc(2026, 3, 29, 10, 0))
        );
        assert_eq!(
            parse("2026-03-28 12:00 + 24h", "Europe/Berlin"),
            Ok(utc(2026, 3, 29, 11, 0))
        );
    }

    #[test]
    fn explains_bad_input() {
        let error = parse("now + 5x", "UTC").unwrap_err();
        assert!(
            error.starts_with("unable to parse input \"now + 5x\""),
            "{error}"
        );
        assert!(error.contains("Date math looks like"), "{error}");
        assert_eq!(
            parse("yesterdayish", "UTC").unwrap_err(),
            "unable to parse input \"yesterdayish\""
        );
        assert!(parse("now + 99999y", "UTC").is_err());
        assert!(
            parse_input("1403-13-01", true, &zone("UTC"), utc(2026, 9, 24, 23, 44))
                .unwrap_err()
                .contains("isn't a Jalali date")
        );
    }

    #[test]
    fn splits_math_from_dates() {
        let math = |input: &str| {
            let (base, terms) = split_math(input);
            (base.to_string(), terms.len())
        };
        assert_eq!(math("2024-01-02"), ("2024-01-02".to_string(), 0));
        assert_eq!(
            math("2024-01-02 03:04:05"),
            ("2024-01-02 03:04:05".to_string(), 0)
        );
        assert_eq!(
            math("2024-01-02T03:04:05+03:30"),
            ("2024-01-02T03:04:05+03:30".to_string(), 0)
        );
        assert_eq!(math("now + 1d - 2h"), ("now".to_string(), 2));
        assert_eq!(math("- 2d"), ("".to_string(), 1));
        assert!(parse_duration("25").is_none(), "a number without a unit");
        assert!(parse_duration("").is_none());
        assert!(parse_duration("1x").is_none());
    }

    #[test]
    fn shows_each_zone() {
        let dt = utc(2026, 7, 1, 10, 0);
        let text = format_output("x", dt, &[zone("Europe/Berlin"), zone("Asia/Tehran")]);
        assert!(
            text.contains("\n\nEurope/Berlin\n  2026-07-01 12:00:00 +02:00 CEST\n"),
            "{text}"
        );
        assert!(
            text.contains("\n\nAsia/Tehran\n  2026-07-01 13:30:00 +03:30\n"),
            "{text}"
        );
        // The Jalali date is in the first zone.
        assert!(
            text.contains("\n\nJalali (Europe/Berlin)\n  1405-04-10 12:00:00 (10 Tir 1405)\n"),
            "{text}"
        );
        assert!(
            text.contains("\n  2026-07-01T12:00:00+02:00\n  2026-07-01T13:30:00+03:30\n"),
            "{text}"
        );
    }
}
