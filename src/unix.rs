use chrono::{DateTime, FixedOffset, Local, TimeZone};
use clap::Parser;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::utils;

#[derive(Parser, Debug)]
pub struct Args {
    /// Unix timestamp (seconds, milliseconds, microseconds, or nanoseconds)
    #[arg(default_value = "now")]
    ts: String,

    /// Timezone: "UTC", "local", or a fixed offset like "+03:30" / "-0500"
    #[arg(short, long, default_value = "UTC")]
    timezone: String,
}

/// Magnitude-based unit detection: a timestamp in seconds won't exceed
/// ~1e11 until the year 5138, so each extra factor of 1000 means a finer unit.
fn detect_unit(ts: i64) -> (&'static str, i64, u32) {
    if ts < 100_000_000_000 {
        ("seconds", ts, 0)
    } else if ts < 100_000_000_000_000 {
        (
            "milliseconds",
            (ts / 1_000),
            (ts % 1_000) as u32 * 1_000_000,
        )
    } else if ts < 100_000_000_000_000_000 {
        (
            "microseconds",
            (ts / 1_000_000),
            (ts % 1_000_000) as u32 * 1_000,
        )
    } else {
        (
            "nanoseconds",
            (ts / 1_000_000_000),
            (ts % 1_000_000_000) as u32,
        )
    }
}

fn parse_offset(tz: &str) -> Option<FixedOffset> {
    let (sign, rest) = match tz.as_bytes().first()? {
        b'+' => (1i32, &tz[1..]),
        b'-' => (-1i32, &tz[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    let (hours, minutes) = match digits.len() {
        1 | 2 => (digits.parse::<i32>().ok()?, 0),
        4 => (
            digits[..2].parse::<i32>().ok()?,
            digits[2..].parse::<i32>().ok()?,
        ),
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

pub fn run(args: &Args, copy: bool) {
    utils::no_copy_support(copy);
    let ts: i64 = if args.ts.to_lowercase().trim() == "now" {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    } else {
        args.ts.parse::<i64>().unwrap()
    };

    let (unit, secs, nanos) = detect_unit(ts);

    let Some(utc) = DateTime::from_timestamp(secs, nanos) else {
        eprintln!("error: timestamp {} is out of range", ts);
        std::process::exit(1);
    };

    const HUMAN: &str = "%A, %d %B %Y %H:%M:%S%.f %Z";
    let local: String;
    match args.timezone.as_str() {
        "UTC" | "utc" | "Z" => local = utc.format(HUMAN).to_string(),
        "local" | "Local" => {
            local = Local
                .from_utc_datetime(&utc.naive_utc())
                .format(HUMAN)
                .to_string()
        }
        tz => match parse_offset(tz) {
            Some(offset) => local = utc.with_timezone(&offset).format(HUMAN).to_string(),
            None => {
                eprintln!(
                    "error: invalid timezone {:?} (use \"UTC\", \"local\", or an offset like \"+03:30\")",
                    tz
                );
                std::process::exit(1);
            }
        },
    }

    println!("Format:   {}", unit);
    println!("Datetime: {}", local);
}
