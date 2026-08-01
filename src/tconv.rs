use crate::utils;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use chrono_humanize::HumanTime;
use clap::Parser;
use std::io::{self, Read};
use std::process::exit;

#[derive(Parser, Debug)]
pub struct Args {
    /// Unix timestamp, ISO8601/RFC3339, "YYYY-MM-DD[ HH:MM:SS[.f]]", or "now".
    /// If omitted, reads from stdin (pipe)
    input: Option<String>,
}

pub fn run(args: &Args, copy: bool) {
    let input = match &args.input {
        Some(s) => s.clone(),
        None => read_stdin(),
    };
    let input = input.trim();

    let dt = if input.eq_ignore_ascii_case("now") {
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

fn format_output(input: &str, dt: DateTime<Utc>) -> String {
    let local = dt.with_timezone(&Local);

    let micros = dt.timestamp() as i128 * 1_000_000 + dt.timestamp_subsec_micros() as i128;
    let nanos = dt.timestamp() as i128 * 1_000_000_000 + dt.timestamp_subsec_nanos() as i128;

    format!(
        "Input\n  {}\n\nUTC\n  {}\n\nLocal\n  {}\n\nISO8601\n  {}\n  {}\n\nUnix\n  Seconds      : {}\n  Milliseconds : {}\n  Microseconds : {}\n  Nanoseconds  : {}\n\nRelative\n  {}",
        input,
        dt.format("%Y-%m-%d %H:%M:%S%.f UTC"),
        local.format("%Y-%m-%d %H:%M:%S%.f %Z"),
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
    fn parses_rfc3339_and_date_only() {
        assert!(parse_datetime("2024-01-02T03:04:05Z").is_some());
        assert!(parse_datetime("2024-01-02").is_some());
        assert!(parse_datetime("not-a-date").is_none());
    }
}
