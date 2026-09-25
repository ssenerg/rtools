//! Time zones for the commands that take --tz. They come from the system's
//! time zone database, which jiff bundles on Windows.

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Timelike, Utc};
pub use jiff::tz::TimeZone as Zone;

pub fn parse(name: &str) -> Result<Zone, String> {
    Zone::get(name).map_err(|_| {
        format!("unknown time zone {name:?}. Use a name like UTC, Europe/Berlin or Asia/Tehran")
    })
}

/// The zone's IANA name, like Asia/Tehran.
pub fn name(zone: &Zone) -> &str {
    zone.iana_name().unwrap_or("the given time zone")
}

pub fn timestamp(at: DateTime<Utc>) -> Option<jiff::Timestamp> {
    jiff::Timestamp::new(at.timestamp(), at.timestamp_subsec_nanos() as i32).ok()
}

pub fn from_timestamp(at: jiff::Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp(at.as_second(), at.subsec_nanosecond() as u32)
        .expect("jiff's range fits in chrono's")
}

pub fn civil(wall: NaiveDateTime) -> Option<jiff::civil::DateTime> {
    jiff::civil::DateTime::new(
        i16::try_from(wall.year()).ok()?,
        wall.month() as i8,
        wall.day() as i8,
        wall.hour() as i8,
        wall.minute() as i8,
        wall.second() as i8,
        wall.nanosecond().min(999_999_999) as i32,
    )
    .ok()
}

/// The UTC offset in effect in `zone` at `at`, in seconds.
pub fn offset_seconds(zone: &Zone, at: DateTime<Utc>) -> i32 {
    let at = jiff::Timestamp::from_second(at.timestamp()).expect("a time in range");
    zone.to_offset(at).seconds()
}

/// The wall-clock time of an instant in a time zone.
pub fn wall_time(zone: &Zone, at: DateTime<Utc>) -> NaiveDateTime {
    at.naive_utc() + Duration::seconds(offset_seconds(zone, at).into())
}
