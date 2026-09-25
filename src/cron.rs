//! Explains cron schedules in plain English and lists when they run next.
//! Matching follows Linux cron (cronie, Vixie cron), including how the two
//! day fields combine.

use crate::utils;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use chrono_humanize::HumanTime;
use clap::Parser;
use jiff::tz::{AmbiguousOffset, Offset, TimeZone as Zone};
use std::io::{self, IsTerminal, Read};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools cron '*/15 9-17 * * 1-5'     every 15 minutes in working hours
  rtools cron '30 5 * * *' --tz UTC   a schedule that runs in UTC, like GitHub Actions
  rtools cron @weekly -n 10
  crontab -l | rtools cron            explain every line of your crontab

Quote the schedule so your shell leaves the * characters alone.")]
pub struct Args {
    /// Schedule: minute hour day-of-month month day-of-week, or @hourly, @daily, @weekly,
    /// @monthly, @yearly or @reboot. A command after it, as in a crontab line, is shown too.
    /// If omitted, reads crontab lines from stdin (pipe)
    #[arg(value_name = "SCHEDULE")]
    schedule: Vec<String>,

    /// How many upcoming runs to list
    #[arg(short = 'n', long, value_name = "N", default_value_t = 5)]
    count: usize,

    /// Time zone the schedule runs in, like UTC or Asia/Tehran [default: this computer's]
    #[arg(long, value_name = "ZONE")]
    tz: Option<String>,
}

/// How far ahead to look for runs. Leap days make some schedules wait years.
const SEARCH_DAYS: usize = 100 * 366;

pub fn run(args: &Args, copy: bool) {
    let zone = match args.tz.as_deref().map(parse_zone).transpose() {
        Ok(zone) => zone,
        Err(e) => fail(&e),
    };
    let now = Utc::now();
    let local = Zone::system();

    let (text, ok) = if args.schedule.is_empty() {
        if io::stdin().is_terminal() {
            fail(
                "nothing to explain: pass a schedule like '*/15 * * * *', or pipe crontab lines in",
            );
        }
        let mut input = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut input) {
            fail(&format!("failed to read stdin: {e}"));
        }
        render_crontab(&input, zone, &local, args.count, now)
    } else {
        let line = args.schedule.join(" ");
        match parse_entry(&line, zone) {
            Ok(entry) => (render_entry(&entry, &local, args.count, now), true),
            Err(e) if args.schedule.len() > 1 => fail(&format!(
                "{e}\nQuote the schedule so your shell leaves the * characters alone: rtools cron '*/15 * * * *'"
            )),
            Err(e) => fail(&e),
        }
    };

    if let Err(e) = utils::emit(&text, copy) {
        fail(&e);
    }
    if !ok {
        exit(1);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

/// A time zone from the system's time zone database (bundled on Windows).
fn parse_zone(name: &str) -> Result<Zone, String> {
    Zone::get(name).map_err(|_| {
        format!("unknown time zone {name:?}. Use a name like UTC, Europe/Berlin or Asia/Tehran")
    })
}

fn zone_name(zone: &Zone) -> &str {
    zone.iana_name().unwrap_or("the given time zone")
}

/// The instants a wall-clock time is in a time zone.
enum Resolved {
    One(DateTime<Utc>),
    /// The clock went back, so it happens twice.
    Twice(DateTime<Utc>, DateTime<Utc>),
    /// The clock jumped past it.
    Skipped,
}

fn resolve(zone: &Zone, wall: NaiveDateTime) -> Resolved {
    let civil = jiff::civil::date(wall.year() as i16, wall.month() as i8, wall.day() as i8).at(
        wall.hour() as i8,
        wall.minute() as i8,
        0,
        0,
    );
    let at = |offset: Offset| wall.and_utc() - Duration::seconds(offset.seconds().into());
    match zone.to_ambiguous_timestamp(civil).offset() {
        AmbiguousOffset::Unambiguous { offset } => Resolved::One(at(offset)),
        AmbiguousOffset::Fold { before, after } => Resolved::Twice(at(before), at(after)),
        AmbiguousOffset::Gap { .. } => Resolved::Skipped,
    }
}

/// The wall-clock time of an instant in a time zone.
fn wall_time(zone: &Zone, at: DateTime<Utc>) -> NaiveDateTime {
    let timestamp = jiff::Timestamp::from_second(at.timestamp()).expect("a time in range");
    at.naive_utc() + Duration::seconds(zone.to_offset(timestamp).seconds().into())
}

// ---------------------------------------------------------------------------
// Parsing

struct Spec {
    name: &'static str,
    min: u32,
    max: u32,
    /// What `*` spans; the day of week also accepts 7 for Sunday.
    star_max: u32,
    names: &'static [&'static str],
    /// `?` means "any" (Quartz, AWS); only the day fields take it.
    question: bool,
}

const MINUTE: Spec = Spec {
    name: "minute",
    min: 0,
    max: 59,
    star_max: 59,
    names: &[],
    question: false,
};
const HOUR: Spec = Spec {
    name: "hour",
    min: 0,
    max: 23,
    star_max: 23,
    names: &[],
    question: false,
};
const DAY: Spec = Spec {
    name: "day of month",
    min: 1,
    max: 31,
    star_max: 31,
    names: &[],
    question: true,
};
const MONTH: Spec = Spec {
    name: "month",
    min: 1,
    max: 12,
    star_max: 12,
    names: &[
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ],
    question: false,
};
const WEEKDAY: Spec = Spec {
    name: "day of week",
    min: 0,
    max: 7,
    star_max: 6,
    names: &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"],
    question: true,
};

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

#[derive(Clone, Debug)]
struct Field {
    /// Bit `v` is set when value `v` matches.
    bits: u64,
    text: String,
    /// Starts with `*` or `?`. Linux cron combines the day fields based on this.
    starts_with_star: bool,
    /// Has a plain `*` or `?` part, without a step. Go's robfig/cron (used by
    /// Kubernetes) combines the day fields based on this instead.
    plain_star: bool,
    /// Parts written in ways Linux cron rejects, with what it accepts instead.
    not_linux: Vec<(String, String)>,
}

impl Field {
    fn parse(text: &str, spec: &Spec) -> Result<Field, String> {
        let mut bits = 0u64;
        let mut plain_star = false;
        let mut not_linux = Vec::new();
        for part in text.split(',') {
            let (range, step) = match part.split_once('/') {
                Some((range, step)) => match step.parse::<u32>() {
                    Ok(step) if step > 0 => (range, Some(step)),
                    _ => {
                        return Err(format!(
                            "{} {part:?}: the step after / must be a whole number above 0",
                            spec.name
                        ));
                    }
                },
                None => (part, None),
            };
            let (start, end) = if range == "*" || (range == "?" && spec.question) {
                plain_star |= step.is_none_or(|step| step == 1);
                if range == "?" {
                    not_linux.push((part.to_string(), part.replacen('?', "*", 1)));
                }
                (spec.min, spec.star_max)
            } else if let Some((a, b)) = range.split_once('-') {
                let (a, mut b) = (value(a, spec)?, value(b, spec)?);
                if spec.max == 7 && b == 0 && a > 0 {
                    // FRI-SUN: Sunday as 7.
                    b = 7;
                }
                if a > b {
                    return Err(format!(
                        "{} {range:?} runs backwards. Split it in two, like {a}-{},{}-{b}",
                        spec.name, spec.max, spec.min
                    ));
                }
                (a, b)
            } else {
                // `5/15` means from 5 to the end, every 15. (`7/2` is Sunday.)
                let a = value(range, spec)?;
                match step {
                    Some(step) => {
                        let end = spec.star_max.max(a);
                        not_linux.push((part.to_string(), format!("{range}-{end}/{step}")));
                        (a, end)
                    }
                    None => (a, a),
                }
            };
            for v in (start..=end).step_by(step.unwrap_or(1) as usize) {
                bits |= 1 << v;
            }
        }
        if spec.max == 7 && bits & (1 << 7) != 0 {
            // 7 is another way to write Sunday.
            bits = (bits & !(1 << 7)) | 1;
        }
        Ok(Field {
            bits,
            text: text.to_string(),
            starts_with_star: text.starts_with(['*', '?']),
            plain_star,
            not_linux,
        })
    }

    fn has(&self, value: u32) -> bool {
        self.bits & (1 << value) != 0
    }

    fn values(&self, spec: &Spec) -> Vec<u32> {
        (spec.min..=spec.star_max)
            .filter(|&v| self.has(v))
            .collect()
    }

    fn is_full(&self, spec: &Spec) -> bool {
        (spec.min..=spec.star_max).all(|v| self.has(v))
    }
}

fn value(token: &str, spec: &Spec) -> Result<u32, String> {
    let upper = token.to_ascii_uppercase();
    let v = match token.parse::<u32>() {
        Ok(v) => v,
        Err(_) => match spec.names.iter().position(|&name| name == upper) {
            Some(i) => spec.min + i as u32,
            None if spec.question && is_quartz(&upper, spec) => {
                return Err(format!(
                    "{} {token:?}: L, W and # are Quartz extensions that standard cron doesn't support",
                    spec.name
                ));
            }
            None if !spec.names.is_empty() => {
                return Err(format!(
                    "invalid {} {token:?}: use {}-{} or {}-{}",
                    spec.name,
                    spec.min,
                    spec.max,
                    spec.names[0],
                    spec.names[spec.names.len() - 1]
                ));
            }
            None if token.is_empty() => return Err(format!("empty {} value", spec.name)),
            None if token == "?" => {
                return Err(format!("{}: ? only works in the day fields", spec.name));
            }
            None => return Err(format!("invalid {} {token:?}", spec.name)),
        },
    };
    if v < spec.min || v > spec.max {
        return Err(format!(
            "{} {v} is out of range ({}-{})",
            spec.name, spec.min, spec.max
        ));
    }
    Ok(v)
}

/// Quartz's `L` (last), `W` (weekday) and `#` (nth weekday), like `L`, `15W` or `MON#2`.
fn is_quartz(upper: &str, spec: &Spec) -> bool {
    let rest = spec
        .names
        .iter()
        .fold(upper.to_string(), |rest, name| rest.replace(name, ""));
    rest.contains(['L', 'W', '#'])
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'L' | 'W' | '#'))
}

#[derive(Clone, Debug)]
struct Schedule {
    minutes: Field,
    hours: Field,
    days: Field,
    months: Field,
    weekdays: Field,
}

/// How the day of month and day of week decide which days run.
#[derive(Debug, PartialEq)]
enum DayRule {
    EveryDay,
    DayOfMonth,
    Weekday,
    /// Both are set: a day runs when either matches.
    Either,
    /// Both are set, but one starts with `*`: a day runs only when both match.
    Both,
}

impl Schedule {
    fn parse(fields: &[&str]) -> Result<Schedule, String> {
        Ok(Schedule {
            minutes: Field::parse(fields[0], &MINUTE)?,
            hours: Field::parse(fields[1], &HOUR)?,
            days: Field::parse(fields[2], &DAY)?,
            months: Field::parse(fields[3], &MONTH)?,
            weekdays: Field::parse(fields[4], &WEEKDAY)?,
        })
    }

    fn fields(&self) -> [&Field; 5] {
        [
            &self.minutes,
            &self.hours,
            &self.days,
            &self.months,
            &self.weekdays,
        ]
    }

    /// Linux cron runs on days matching either day field, unless one starts with `*`.
    fn days_either(&self) -> bool {
        !self.days.starts_with_star && !self.weekdays.starts_with_star
    }

    fn day_rule(&self) -> DayRule {
        let all_days = self.days.is_full(&DAY);
        let all_weekdays = self.weekdays.is_full(&WEEKDAY);
        if self.days_either() {
            if all_days || all_weekdays {
                DayRule::EveryDay
            } else {
                DayRule::Either
            }
        } else {
            match (all_days, all_weekdays) {
                (true, true) => DayRule::EveryDay,
                (false, true) => DayRule::DayOfMonth,
                (true, false) => DayRule::Weekday,
                (false, false) => DayRule::Both,
            }
        }
    }

    fn date_matches(&self, date: NaiveDate) -> bool {
        if !self.months.has(date.month()) {
            return false;
        }
        let day = self.days.has(date.day());
        let weekday = self.weekdays.has(date.weekday().num_days_from_sunday());
        if self.days_either() {
            day || weekday
        } else {
            day && weekday
        }
    }

    /// Runs after `after`, as wall-clock times in `zone` and the instants they are.
    fn next_runs(
        &self,
        zone: &Zone,
        after: DateTime<Utc>,
        count: usize,
    ) -> Vec<(NaiveDateTime, DateTime<Utc>)> {
        let hours = self.hours.values(&HOUR);
        let minutes = self.minutes.values(&MINUTE);
        // Around daylight-saving changes cron treats jobs whose minute or hour starts
        // with * differently from jobs at fixed times.
        let wildcard = self.minutes.starts_with_star || self.hours.starts_with_star;
        let mut runs = Vec::new();
        if count == 0 {
            return runs;
        }
        let start = wall_time(zone, after).date();
        for date in start.iter_days().take(SEARCH_DAYS) {
            if !self.date_matches(date) {
                continue;
            }
            let mut day = Vec::new();
            for &hour in &hours {
                for &minute in &minutes {
                    let wall = date.and_hms_opt(hour, minute, 0).expect("a valid time");
                    match resolve(zone, wall) {
                        Resolved::One(at) => day.push(at),
                        // In the hour that repeats, fixed-time jobs run once and
                        // wildcard jobs run in both passes.
                        Resolved::Twice(first, second) => {
                            day.push(first);
                            if wildcard {
                                day.push(second);
                            }
                        }
                        // In the hour that's skipped, fixed-time jobs run right after
                        // the jump and wildcard jobs don't run.
                        Resolved::Skipped if !wildcard => day.extend(after_gap(zone, wall)),
                        Resolved::Skipped => {}
                    }
                }
            }
            day.sort();
            day.dedup();
            for at in day {
                if at > after {
                    runs.push((wall_time(zone, at), at));
                    if runs.len() == count {
                        return runs;
                    }
                }
            }
        }
        runs
    }
}

/// The first moment after a wall-clock time that a daylight-saving change skips.
fn after_gap(zone: &Zone, wall: NaiveDateTime) -> Option<DateTime<Utc>> {
    (1..=180).find_map(
        |minutes| match resolve(zone, wall + Duration::minutes(minutes)) {
            Resolved::One(at) | Resolved::Twice(at, _) => Some(at),
            Resolved::Skipped => None,
        },
    )
}

struct Entry {
    /// None for @reboot.
    schedule: Option<Schedule>,
    command: Option<String>,
    zone: Option<Zone>,
}

fn parse_entry(line: &str, zone: Option<Zone>) -> Result<Entry, String> {
    let mut line = line.trim();
    let mut zone = zone;
    // `CRON_TZ=Asia/Tehran 0 9 * * *`, as robfig/cron and some crontabs write it.
    for prefix in ["CRON_TZ=", "TZ="] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let (name, rest) = split_fields(rest, 1);
            zone = Some(parse_zone(name.first().copied().unwrap_or_default())?);
            line = rest;
            break;
        }
    }

    if line.starts_with('@') {
        let (name, command) = split_fields(line, 1);
        let fields = match name[0].to_ascii_lowercase().as_str() {
            "@yearly" | "@annually" => "0 0 1 1 *",
            "@monthly" => "0 0 1 * *",
            "@weekly" => "0 0 * * 0",
            "@daily" | "@midnight" => "0 0 * * *",
            "@hourly" => "0 * * * *",
            "@reboot" => "",
            _ => {
                return Err(format!(
                    "unknown shortcut {:?}. Try @hourly, @daily, @weekly, @monthly, @yearly or @reboot",
                    name[0]
                ));
            }
        };
        let schedule = if fields.is_empty() {
            None
        } else {
            Some(Schedule::parse(&fields.split(' ').collect::<Vec<_>>())?)
        };
        return Ok(Entry {
            schedule,
            command: Some(command.to_string()).filter(|c| !c.is_empty()),
            zone,
        });
    }

    let (fields, command) = split_fields(line, 5);
    if fields.len() < 5 {
        return Err(format!(
            "a schedule has 5 fields (minute hour day-of-month month day-of-week), this has {}",
            fields.len()
        ));
    }
    let next = command.split_whitespace().next().unwrap_or_default();
    if !next.is_empty() && looks_like_field(next) {
        return Err(
            "this has more than 5 fields. rtools reads standard cron: minute hour day-of-month \
             month day-of-week; schedules with seconds (Spring, Quartz) or years (AWS) aren't supported"
                .to_string(),
        );
    }
    Ok(Entry {
        schedule: Some(Schedule::parse(&fields)?),
        command: Some(command.to_string()).filter(|c| !c.is_empty()),
        zone,
    })
}

/// The first `n` whitespace-separated fields, and the rest of the line as written.
fn split_fields(text: &str, n: usize) -> (Vec<&str>, &str) {
    let mut fields = Vec::new();
    let mut rest = text.trim_start();
    while fields.len() < n && !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        fields.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    (fields, rest.trim_end())
}

/// A sixth token that reads as a cron field, rather than the start of a command.
fn looks_like_field(token: &str) -> bool {
    token
        .chars()
        .all(|c| c.is_ascii_digit() || "*?/,-#".contains(c))
        || Field::parse(token, &WEEKDAY).is_ok()
        || Field::parse(token, &MONTH).is_ok()
}

// ---------------------------------------------------------------------------
// Describing

/// The pattern in a field's values, for describing it.
#[derive(Debug, PartialEq)]
enum Shape {
    Any,
    One(u32),
    /// Consecutive values.
    Range(u32, u32),
    /// From the field's first value to its end, every n.
    Every(u32),
    /// first, last, every n.
    Step(u32, u32, u32),
    List,
}

fn shape(values: &[u32], min: u32, max: u32) -> Shape {
    let n = values.len();
    let span = max - min + 1;
    if n as u32 == span {
        return Shape::Any;
    }
    if n == 1 {
        return Shape::One(values[0]);
    }
    let step = values[1] - values[0];
    if !values.windows(2).all(|w| w[1] - w[0] == step) {
        return Shape::List;
    }
    let (first, last) = (values[0], values[n - 1]);
    if step == 1 {
        Shape::Range(first, last)
    } else if first == min && last + step > max && (n >= 3 || span.is_multiple_of(step)) {
        Shape::Every(step)
    } else if n >= 3 {
        Shape::Step(first, last, step)
    } else {
        Shape::List
    }
}

fn hm(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

fn colon(minute: u32) -> String {
    format!(":{minute:02}")
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// "other" for every other, else "3rd" for every 3rd.
fn nth(n: u32) -> String {
    if n == 2 {
        "other".to_string()
    } else {
        ordinal(n)
    }
}

fn join_with(items: &[String], conjunction: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} {conjunction} {last}", rest.join(", ")),
    }
}

fn join(items: &[String]) -> String {
    join_with(items, "and")
}

/// Lists values, writing three or more consecutive ones as "a through b".
fn join_runs(values: &[u32], fmt: impl Fn(u32) -> String) -> String {
    let mut items = Vec::new();
    let mut i = 0;
    while i < values.len() {
        let mut j = i;
        while j + 1 < values.len() && values[j + 1] == values[j] + 1 {
            j += 1;
        }
        if j - i >= 2 {
            items.push(format!("{} through {}", fmt(values[i]), fmt(values[j])));
        } else {
            items.extend(values[i..=j].iter().map(|&v| fmt(v)));
        }
        i = j + 1;
    }
    join(&items)
}

impl Schedule {
    fn describe(&self) -> String {
        let (mut text, lists_times) = self.describe_time();
        let rule = self.day_rule();
        let days = self.days.values(&DAY);
        let months = self.months.values(&MONTH);
        let month_shape = shape(&months, 1, 12);

        if rule == DayRule::EveryDay && lists_times {
            text.push_str(" every day");
        }
        // "on March 15" reads better than "on the 15th of the month, in March".
        if let (DayRule::DayOfMonth, [day], Shape::One(month)) =
            (&rule, days.as_slice(), &month_shape)
        {
            return format!("{text}, on {} {day}", MONTH_NAMES[*month as usize - 1]);
        }
        if let Some(days) = self.describe_days(&rule) {
            text = format!("{text}, {days}");
        }
        let month = |m: u32| MONTH_NAMES[m as usize - 1].to_string();
        match month_shape {
            Shape::Any => {}
            Shape::One(m) => text = format!("{text}, in {}", month(m)),
            Shape::Range(a, b) => text = format!("{text}, from {} through {}", month(a), month(b)),
            _ => text = format!("{text}, in {}", join_runs(&months, month)),
        }
        text
    }

    /// The times of day, and whether they're listed one by one.
    fn describe_time(&self) -> (String, bool) {
        let minutes = self.minutes.values(&MINUTE);
        let hours = self.hours.values(&HOUR);
        let (first, last) = (minutes[0], minutes[minutes.len() - 1]);
        let minute_shape = shape(&minutes, 0, 59);
        let hour_shape = shape(&hours, 0, 23);
        let times = || {
            let times: Vec<String> = hours
                .iter()
                .flat_map(|&h| minutes.iter().map(move |&m| hm(h, m)))
                .collect();
            (format!("At {}", join(&times)), true)
        };

        let a_run_of_minutes = matches!(minute_shape, Shape::Range(..)) && minutes.len() > 2;
        if minute_shape != Shape::Any
            && hour_shape != Shape::Any
            && !a_run_of_minutes
            && minutes.len() * hours.len() <= 6
        {
            return times();
        }

        let text = match (&minute_shape, &hour_shape) {
            (Shape::One(m), Shape::Any) => format!("Every hour at {}", colon(*m)),
            (Shape::One(m), Shape::Every(n)) => format!("Every {n} hours at {}", colon(*m)),
            (Shape::One(m), Shape::Range(a, b)) => format!(
                "Every hour at {}, from {} through {}",
                colon(*m),
                hm(*a, *m),
                hm(*b, *m)
            ),
            (Shape::One(m), Shape::Step(a, b, n)) => format!(
                "Every {n} hours at {}, from {} through {}",
                colon(*m),
                hm(*a, *m),
                hm(*b, *m)
            ),
            (Shape::One(_), _) => return times(),
            (Shape::Any | Shape::Every(_), _) => {
                let every = match minute_shape {
                    Shape::Every(n) => format!("Every {n} minutes"),
                    _ => "Every minute".to_string(),
                };
                match hour_shape {
                    Shape::Any => every,
                    Shape::One(h) => {
                        format!("{every}, from {} through {}", hm(h, first), hm(h, last))
                    }
                    Shape::Range(a, b) => {
                        format!("{every}, from {} through {}", hm(a, first), hm(b, last))
                    }
                    _ => format!("{every}, during {}", hours_phrase(&hours, &hour_shape)),
                }
            }
            // Several minutes of each hour, not evenly spread from :00.
            (_, Shape::One(h)) => within_hour(&minute_shape, &minutes, |m| hm(*h, m)),
            (Shape::List, Shape::Any) => format!("Every hour at {}", join_runs(&minutes, colon)),
            (_, Shape::Any) => format!(
                "{} of every hour",
                within_hour(&minute_shape, &minutes, colon)
            ),
            _ => format!(
                "{}, during {}",
                within_hour(&minute_shape, &minutes, colon),
                hours_phrase(&hours, &hour_shape)
            ),
        };
        (text, false)
    }

    fn describe_days(&self, rule: &DayRule) -> Option<String> {
        let days = self.days.values(&DAY);
        let weekdays = self.weekdays.values(&WEEKDAY);
        let day_phrase = || match shape(&days, 1, 31) {
            Shape::Every(n) => format!("on every {} day of the month", nth(n)),
            Shape::Step(a, b, n) => format!(
                "on every {} day of the month from the {} through the {}",
                nth(n),
                ordinal(a),
                ordinal(b)
            ),
            _ => format!("on the {} of the month", join_runs(&days, ordinal)),
        };
        match rule {
            DayRule::EveryDay => None,
            DayRule::DayOfMonth => Some(day_phrase()),
            DayRule::Weekday => Some(match week_shape(&weekdays) {
                Week::One(d) => format!("every {}", WEEKDAY_NAMES[d as usize]),
                Week::Range(a, b) => {
                    format!(
                        "{} through {}",
                        WEEKDAY_NAMES[a as usize], WEEKDAY_NAMES[b as usize]
                    )
                }
                Week::List(list) => format!("on {}", join(&names(&list))),
            }),
            DayRule::Either => Some(format!(
                "{} and {}",
                day_phrase(),
                match week_shape(&weekdays) {
                    Week::One(d) => format!("every {}", WEEKDAY_NAMES[d as usize]),
                    Week::Range(a, b) => format!(
                        "every {} through {}",
                        WEEKDAY_NAMES[a as usize], WEEKDAY_NAMES[b as usize]
                    ),
                    Week::List(list) => format!("every {}", join(&names(&list))),
                }
            )),
            DayRule::Both => Some(format!(
                "{}, if it's {}",
                day_phrase(),
                match week_shape(&weekdays) {
                    Week::One(d) => format!("a {}", WEEKDAY_NAMES[d as usize]),
                    Week::Range(a, b) => format!(
                        "a {} through {}",
                        WEEKDAY_NAMES[a as usize], WEEKDAY_NAMES[b as usize]
                    ),
                    Week::List(list) => format!("a {}", join_with(&names(&list), "or")),
                }
            )),
        }
    }
}

fn within_hour(shape: &Shape, minutes: &[u32], fmt: impl Fn(u32) -> String) -> String {
    match shape {
        Shape::Range(a, b) => format!("Every minute from {} through {}", fmt(*a), fmt(*b)),
        Shape::Step(a, b, n) => format!("Every {n} minutes from {} through {}", fmt(*a), fmt(*b)),
        _ => format!("At {}", join_runs(minutes, fmt)),
    }
}

fn hours_phrase(hours: &[u32], shape: &Shape) -> String {
    match shape {
        Shape::Every(n) => format!("every {} hour", nth(*n)),
        _ => format!("hours {}", join_runs(hours, |h| h.to_string())),
    }
}

enum Week {
    One(u32),
    /// Consecutive days, possibly across the weekend: Friday through Sunday.
    Range(u32, u32),
    List(Vec<u32>),
}

fn week_shape(weekdays: &[u32]) -> Week {
    if let [day] = weekdays {
        return Week::One(*day);
    }
    // Week lists read Monday first, as cron's 1-5 means Monday through Friday.
    let mut list = weekdays.to_vec();
    list.sort_by_key(|&d| (d + 6) % 7);
    let has = |d: u32| weekdays.contains(&(d % 7));
    if list.len() >= 3 {
        // A run of days that may wrap past Saturday: find where it starts.
        if let Some(&start) = list.iter().find(|&&d| !has(d + 6)) {
            let len = list.len() as u32;
            if (0..len).all(|i| has(start + i)) {
                return Week::Range(start, (start + len - 1) % 7);
            }
        }
    }
    Week::List(list)
}

fn names(weekdays: &[u32]) -> Vec<String> {
    weekdays
        .iter()
        .map(|&d| WEEKDAY_NAMES[d as usize].to_string())
        .collect()
}

fn days_in_month(month: u32) -> u32 {
    match month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Schedule {
    /// Why the schedule can never run, if it can't.
    fn never_runs(&self) -> Option<String> {
        if !matches!(self.day_rule(), DayRule::DayOfMonth | DayRule::Both) {
            return None;
        }
        let days = self.days.values(&DAY);
        let months = self.months.values(&MONTH);
        if months
            .iter()
            .any(|&m| days.iter().any(|&d| d <= days_in_month(m)))
        {
            return None;
        }
        let month_names: Vec<String> = months
            .iter()
            .map(|&m| MONTH_NAMES[m as usize - 1].to_string())
            .collect();
        let verb = if months.len() == 1 { "has" } else { "have" };
        let days: Vec<String> = days.iter().map(|&d| ordinal(d)).collect();
        Some(format!(
            "{} never {verb} a {}.",
            join(&month_names),
            join_with(&days, "or")
        ))
    }

    /// Surprises worth pointing out.
    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        let rule = self.day_rule();

        if self.days_either() {
            let (all_days, all_weekdays) =
                (self.days.is_full(&DAY), self.weekdays.is_full(&WEEKDAY));
            if all_days != all_weekdays {
                let (full, other) = if all_days {
                    (
                        ("Day of month", &self.days),
                        ("day of week", &self.weekdays),
                    )
                } else {
                    (
                        ("Day of week", &self.weekdays),
                        ("day of month", &self.days),
                    )
                };
                notes.push(format!(
                    "{} {} covers every day, and cron runs on days that match either day field, \
                     so the {} {} has no effect.",
                    full.0, full.1.text, other.0, other.1.text
                ));
            } else if rule == DayRule::Either {
                notes.push(
                    "When both day fields are set, cron runs on days that match either one, \
                     not only on days that match both."
                        .to_string(),
                );
            }
        }
        let robfig_either = !self.days.plain_star && !self.weekdays.plain_star;
        if robfig_either != self.days_either() {
            let (linux, kubernetes) = if self.days_either() {
                ("either one", "both")
            } else {
                ("both", "either one")
            };
            notes.push(format!(
                "Linux cron (cronie, Vixie cron) runs this on days that match {linux} of the day \
                 fields, as shown here. Kubernetes and other schedulers built on Go's robfig/cron \
                 run it on days that match {kubernetes}."
            ));
        }

        for field in self.fields() {
            for (part, instead) in &field.not_linux {
                notes.push(format!(
                    "Linux cron rejects {part}; write {instead} there, which means the same. \
                     (Kubernetes accepts both.)"
                ));
            }
        }

        let minutes = self.minutes.values(&MINUTE);
        if let Shape::Every(n) = shape(&minutes, 0, 59)
            && !60u32.is_multiple_of(n)
        {
            let last = minutes[minutes.len() - 1];
            notes.push(format!(
                "Every {n} minutes starts over each hour, so {} and :00 are only {} minutes apart.",
                colon(last),
                60 - last
            ));
        }
        let hours = self.hours.values(&HOUR);
        if let Shape::Every(n) = shape(&hours, 0, 23)
            && !24u32.is_multiple_of(n)
        {
            let last = hours[hours.len() - 1];
            let gap = 24 - last;
            notes.push(format!(
                "Every {n} hours starts over at midnight, so hours {last} and 0 are only {gap} {} apart.",
                if gap == 1 { "hour" } else { "hours" }
            ));
        }
        let days = self.days.values(&DAY);
        if rule != DayRule::EveryDay
            && rule != DayRule::Weekday
            && let Shape::Every(n) = shape(&days, 1, 31)
        {
            notes.push(format!(
                "Every {} day starts over on the 1st of each month, so the gap at the end of a \
                 month varies.",
                nth(n)
            ));
        }
        let months = self.months.values(&MONTH);
        if let Shape::Every(n) = shape(&months, 1, 12)
            && !12u32.is_multiple_of(n)
        {
            let last = months[months.len() - 1];
            let gap = 13 - last;
            notes.push(format!(
                "Every {n} months starts over in January, so {} and January are only {gap} {} apart.",
                MONTH_NAMES[last as usize - 1],
                if gap == 1 { "month" } else { "months" }
            ));
        }

        if matches!(rule, DayRule::DayOfMonth | DayRule::Both) && self.never_runs().is_none() {
            let skipped: Vec<u32> = months
                .iter()
                .copied()
                .filter(|&m| days.iter().all(|&d| d > days_in_month(m)))
                .collect();
            if !skipped.is_empty() {
                let names: Vec<String> = skipped
                    .iter()
                    .map(|&m| MONTH_NAMES[m as usize - 1].to_string())
                    .collect();
                let missing: Vec<String> = days
                    .iter()
                    .filter(|&&d| skipped.iter().all(|&m| d > days_in_month(m)))
                    .map(|&d| ordinal(d))
                    .collect();
                notes.push(format!(
                    "Skips {}, which {} no {}. Cron can't say \"the last day of the month\".",
                    join(&names),
                    if skipped.len() == 1 { "has" } else { "have" },
                    join_with(&missing, "or")
                ));
            }
            if months.contains(&2) && !skipped.contains(&2) && days.iter().all(|&d| d >= 29) {
                notes.push("February 29 only comes in leap years.".to_string());
            }
        }
        notes
    }
}

// ---------------------------------------------------------------------------
// Output

const TIME_FORMAT: &str = "%a %Y-%m-%d %H:%M";

fn upcoming(
    entry: &Entry,
    schedule: &Schedule,
    local: &Zone,
    count: usize,
    now: DateTime<Utc>,
) -> Vec<(NaiveDateTime, DateTime<Utc>)> {
    schedule.next_runs(entry.zone.as_ref().unwrap_or(local), now, count)
}

/// Whether the schedule's wall clock differs from this computer's for any run.
fn differs_from_local(
    entry: &Entry,
    local: &Zone,
    runs: &[(NaiveDateTime, DateTime<Utc>)],
) -> bool {
    entry.zone.is_some() && runs.iter().any(|(wall, at)| wall_time(local, *at) != *wall)
}

fn relative(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    HumanTime::from(at - now).to_string()
}

/// `local` is this computer's time zone: the schedule's unless it names one.
fn render_entry(entry: &Entry, local: &Zone, count: usize, now: DateTime<Utc>) -> String {
    let zone = entry
        .zone
        .as_ref()
        .map(|tz| format!(" ({})", zone_name(tz)))
        .unwrap_or_default();
    let Some(schedule) = &entry.schedule else {
        let mut out = "Schedule\n  At startup, when the cron daemon starts".to_string();
        if let Some(command) = &entry.command {
            out.push_str(&format!("\n\nCommand\n  {command}"));
        }
        return out;
    };

    let mut out = format!("Schedule{zone}\n  {}", schedule.describe());
    if let Some(command) = &entry.command {
        out.push_str(&format!("\n\nCommand\n  {command}"));
    }
    if let Some(reason) = schedule.never_runs() {
        out.push_str(&format!("\n\nNext runs\n  None: {reason}"));
    } else if count > 0 {
        let runs = upcoming(entry, schedule, local, count, now);
        let both = differs_from_local(entry, local, &runs);
        if both {
            out.push_str(&format!(
                "\n\nNext runs{} → your time)",
                zone.trim_end_matches(')')
            ));
        } else {
            out.push_str(&format!("\n\nNext runs{zone}"));
        }
        for (i, (wall, at)) in runs.iter().enumerate() {
            out.push_str(&format!("\n  {}", wall.format(TIME_FORMAT)));
            if both {
                out.push_str(&format!(
                    "  →  {}",
                    wall_time(local, *at).format(TIME_FORMAT)
                ));
            }
            if i == 0 {
                out.push_str(&format!("  {}", relative(*at, now)));
            }
        }
        if runs.is_empty() {
            out.push_str("\n  None in the next 100 years");
        }
    }
    let notes = schedule.notes();
    if !notes.is_empty() {
        out.push_str("\n\nNotes");
        for note in notes {
            out.push_str(&format!("\n  - {note}"));
        }
    }
    out
}

/// A compact explanation of every entry in a crontab, and whether all of them parsed.
fn render_crontab(
    input: &str,
    zone: Option<Zone>,
    local: &Zone,
    count: usize,
    now: DateTime<Utc>,
) -> (String, bool) {
    let mut zone = zone;
    let mut entries = Vec::new();
    for line in input.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, value)) = assignment(line) {
            // `CRON_TZ=Asia/Tehran 0 9 * * *` is a schedule; any other `NAME=value` is a variable.
            let prefixed = matches!(name, "CRON_TZ" | "TZ") && value.contains(char::is_whitespace);
            if !prefixed {
                if name == "CRON_TZ" {
                    // Applies to the lines below it, as in cronie.
                    match parse_zone(value) {
                        Ok(tz) => zone = Some(tz),
                        Err(e) => entries.push((line, Err(e))),
                    }
                }
                continue;
            }
        }
        entries.push((line, parse_entry(line, zone.clone())));
    }

    if let [(_, Ok(entry))] = entries.as_slice() {
        return (render_entry(entry, local, count, now), true);
    }
    if entries.is_empty() {
        return ("No schedules found.".to_string(), true);
    }

    let mut ok = true;
    let blocks: Vec<String> = entries
        .iter()
        .map(|(line, entry)| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    ok = false;
                    return format!("{line}\n  Error: {e}");
                }
            };
            let Some(schedule) = &entry.schedule else {
                return format!("{line}\n  At startup, when the cron daemon starts");
            };
            let mut block = format!("{line}\n  {}", schedule.describe());
            if let Some(reason) = schedule.never_runs() {
                block.push_str(&format!("\n  Never runs: {reason}"));
            } else if let Some((wall, at)) = upcoming(entry, schedule, local, 1, now).first() {
                let mut next = wall.format(TIME_FORMAT).to_string();
                if let Some(tz) = &entry.zone {
                    next = format!("{next} {}", zone_name(tz));
                }
                if differs_from_local(entry, local, &[(*wall, *at)]) {
                    next = format!(
                        "{next} → {} your time",
                        wall_time(local, *at).format(TIME_FORMAT)
                    );
                }
                block.push_str(&format!("\n  Next: {next}, {}", relative(*at, now)));
            }
            for note in schedule.notes() {
                block.push_str(&format!("\n  Note: {note}"));
            }
            block
        })
        .collect();
    (blocks.join("\n\n"), ok)
}

/// A crontab variable line: `NAME=value`.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then(|| (name, value.trim().trim_matches(['"', '\''])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(text: &str) -> Schedule {
        parse_entry(text, None).unwrap().schedule.unwrap()
    }

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(y, m, d)
            .and_then(|date| date.and_hms_opt(h, min, 0))
            .unwrap()
            .and_utc()
    }

    fn zone(name: &str) -> Zone {
        Zone::get(name).unwrap()
    }

    /// The next runs after `after` as "YYYY-MM-DD HH:MM" in `zone`.
    fn next(text: &str, zone: &Zone, after: DateTime<Utc>, count: usize) -> Vec<String> {
        schedule(text)
            .next_runs(zone, after, count)
            .iter()
            .map(|(wall, _)| wall.format("%Y-%m-%d %H:%M").to_string())
            .collect()
    }

    #[test]
    fn parses_fields() {
        let s = schedule("*/15 9-17 1,15 JAN-mar mon-FRI");
        assert_eq!(s.minutes.values(&MINUTE), [0, 15, 30, 45]);
        assert_eq!(s.hours.values(&HOUR), (9..=17).collect::<Vec<_>>());
        assert_eq!(s.days.values(&DAY), [1, 15]);
        assert_eq!(s.months.values(&MONTH), [1, 2, 3]);
        assert_eq!(s.weekdays.values(&WEEKDAY), [1, 2, 3, 4, 5]);

        assert_eq!(
            schedule("5/20 * * * *").minutes.values(&MINUTE),
            [5, 25, 45]
        );
        assert_eq!(
            schedule("1-10/4 * * * *").minutes.values(&MINUTE),
            [1, 5, 9]
        );
        // 7 is Sunday too, including in ranges and as FRI-SUN.
        assert_eq!(schedule("* * * * 7").weekdays.values(&WEEKDAY), [0]);
        assert_eq!(schedule("* * * * 5-7").weekdays.values(&WEEKDAY), [0, 5, 6]);
        assert_eq!(
            schedule("* * * * FRI-SUN").weekdays.values(&WEEKDAY),
            [0, 5, 6]
        );
        assert_eq!(schedule("* * * * 7/2").weekdays.values(&WEEKDAY), [0]);
        assert_eq!(
            schedule("* * * * */2").weekdays.values(&WEEKDAY),
            [0, 2, 4, 6]
        );
        assert!(schedule("* * ? * 1").weekdays.has(1));
    }

    #[test]
    fn explains_bad_fields() {
        let error = |text: &str| parse_entry(text, None).err().unwrap();
        assert_eq!(error("60 * * * *"), "minute 60 is out of range (0-59)");
        assert_eq!(error("* 24 * * *"), "hour 24 is out of range (0-23)");
        assert_eq!(error("* * 0 * *"), "day of month 0 is out of range (1-31)");
        assert_eq!(error("* * * 13 *"), "month 13 is out of range (1-12)");
        assert_eq!(error("* * * * 8"), "day of week 8 is out of range (0-7)");
        assert!(error("0 22-2 * * *").contains("Split it in two, like 22-23,0-2"));
        assert!(error("*/0 * * * *").contains("above 0"));
        assert!(error("* * L * *").contains("Quartz"));
        assert!(error("* * 15W * *").contains("Quartz"));
        assert!(error("* * * * MON#2").contains("Quartz"));
        assert!(error("* * * * 5L").contains("Quartz"));
        assert!(error("* * Cargo.lock * *").starts_with("invalid day of month"));
        assert!(error("* * * * FOO").contains("SUN-SAT"));
        assert!(error("? * * * *").contains("only works in the day fields"));
        assert_eq!(error("1,,2 * * * *"), "empty minute value");
        assert!(error("* * *").contains("this has 3"));
        assert!(error("0 0 12 * * ?").contains("more than 5 fields"));
        assert!(error("0 0 12 * * MON").contains("more than 5 fields"));
        assert!(error("@every 5m").starts_with("unknown shortcut"));
        assert!(error("TZ=Mars/Olympus * * * * *").starts_with("unknown time zone"));
    }

    #[test]
    fn reads_crontab_lines() {
        let entry = parse_entry("  0 3 * * 0   /usr/bin/backup.sh --all  ", None).unwrap();
        assert_eq!(entry.command.as_deref(), Some("/usr/bin/backup.sh --all"));
        // The user column of /etc/crontab is part of the command here.
        let entry = parse_entry("17 * * * * root cd / && run-parts /etc/cron.hourly", None);
        assert_eq!(
            entry.unwrap().command.as_deref(),
            Some("root cd / && run-parts /etc/cron.hourly")
        );

        let entry = parse_entry("CRON_TZ=asia/tehran 0 9 * * *", None).unwrap();
        assert_eq!(entry.zone.as_ref().map(zone_name), Some("Asia/Tehran"));
        assert!(entry.command.is_none());

        let entry = parse_entry("@reboot /usr/bin/start", None).unwrap();
        assert!(entry.schedule.is_none());
        assert_eq!(entry.command.as_deref(), Some("/usr/bin/start"));

        for (shortcut, fields) in [
            ("@yearly", "0 0 1 1 *"),
            ("@annually", "0 0 1 1 *"),
            ("@monthly", "0 0 1 * *"),
            ("@weekly", "0 0 * * 0"),
            ("@daily", "0 0 * * *"),
            ("@MIDNIGHT", "0 0 * * *"),
            ("@hourly", "0 * * * *"),
        ] {
            assert_eq!(
                schedule(shortcut).describe(),
                schedule(fields).describe(),
                "{shortcut}"
            );
        }
    }

    #[test]
    fn finds_time_zones_in_any_case() {
        let name = |text: &str| parse_zone(text).map(|zone| zone_name(&zone).to_string());
        assert_eq!(name("Asia/Tehran").as_deref(), Ok("Asia/Tehran"));
        assert_eq!(name("asia/tehran").as_deref(), Ok("Asia/Tehran"));
        assert_eq!(name("utc").as_deref(), Ok("UTC"));
        assert!(parse_zone("Nowhere").is_err());
    }

    #[test]
    fn describes_schedules() {
        let cases = [
            ("* * * * *", "Every minute"),
            ("*/10 * * * *", "Every 10 minutes"),
            ("0,30 * * * *", "Every 30 minutes"),
            ("0 * * * *", "Every hour at :00"),
            ("15,45 * * * *", "Every hour at :15 and :45"),
            ("0 */2 * * *", "Every 2 hours at :00"),
            ("0 9 * * *", "At 09:00 every day"),
            ("0 9,17 * * *", "At 09:00 and 17:00 every day"),
            ("30 */6 * * *", "At 00:30, 06:30, 12:30 and 18:30 every day"),
            (
                "23 0-20/2 * * *",
                "Every 2 hours at :23, from 00:23 through 20:23",
            ),
            (
                "0 9-17 * * *",
                "Every hour at :00, from 09:00 through 17:00",
            ),
            (
                "*/15 9-17 * * 1-5",
                "Every 15 minutes, from 09:00 through 17:45, Monday through Friday",
            ),
            ("* 9 * * *", "Every minute, from 09:00 through 09:59"),
            ("0-4 9 * * *", "Every minute from 09:00 through 09:04"),
            (
                "0-29 9-17 * * *",
                "Every minute from :00 through :29, during hours 9 through 17",
            ),
            (
                "0-29 * * * *",
                "Every minute from :00 through :29 of every hour",
            ),
            (
                "*/10 */2 * * *",
                "Every 10 minutes, during every other hour",
            ),
            ("* 9,17 * * *", "Every minute, during hours 9 and 17"),
            ("5 4 * * sun", "At 04:05, every Sunday"),
            ("0 22 * * 1-5", "At 22:00, Monday through Friday"),
            ("0 12 * * 4,5", "At 12:00, on Thursday and Friday"),
            ("0 0 * * 5-7", "At 00:00, Friday through Sunday"),
            ("0 0 * * 1,3,5", "At 00:00, on Monday, Wednesday and Friday"),
            ("0 0 1 * *", "At 00:00, on the 1st of the month"),
            ("0 9 1,15 * *", "At 09:00, on the 1st and 15th of the month"),
            (
                "0 9 1-7 * *",
                "At 09:00, on the 1st through 7th of the month",
            ),
            ("0 0 */2 * *", "At 00:00, on every other day of the month"),
            ("0 0 1 1 *", "At 00:00, on January 1"),
            ("0 0 30 2 *", "At 00:00, on February 30"),
            ("* * * 1 *", "Every minute, in January"),
            ("0 9 * 3-6 *", "At 09:00 every day, from March through June"),
            (
                "0 0,12 1 */2 *",
                "At 00:00 and 12:00, on the 1st of the month, in January, March, May, July, \
                 September and November",
            ),
            (
                "0 0 1 * 1",
                "At 00:00, on the 1st of the month and every Monday",
            ),
            (
                "0 0 1,15 * 1-5",
                "At 00:00, on the 1st and 15th of the month and every Monday through Friday",
            ),
            (
                "0 0 */2 * 1",
                "At 00:00, on every other day of the month, if it's a Monday",
            ),
            ("0 0 1-31 * 1", "At 00:00 every day"),
        ];
        for (text, description) in cases {
            assert_eq!(schedule(text).describe(), description, "{text}");
        }
    }

    #[test]
    fn combines_the_day_fields_like_linux_cron() {
        // 2026-10-01 is a Thursday, 2026-10-05 a Monday.
        let thursday_1st = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let monday_5th = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let tuesday_6th = NaiveDate::from_ymd_opt(2026, 10, 6).unwrap();

        // Both set: either matches.
        let either = schedule("0 0 1 * 1");
        assert_eq!(either.day_rule(), DayRule::Either);
        assert!(either.date_matches(thursday_1st));
        assert!(either.date_matches(monday_5th));
        assert!(!either.date_matches(tuesday_6th));

        // One starts with *: both must match.
        let both = schedule("0 0 */2 * 1");
        assert_eq!(both.day_rule(), DayRule::Both);
        assert!(both.date_matches(monday_5th));
        assert!(!both.date_matches(thursday_1st));

        assert_eq!(schedule("0 0 1 * *").day_rule(), DayRule::DayOfMonth);
        assert_eq!(schedule("0 0 * * 1").day_rule(), DayRule::Weekday);
        assert_eq!(schedule("0 0 * * *").day_rule(), DayRule::EveryDay);
        assert_eq!(schedule("0 0 1-31 * 1").day_rule(), DayRule::EveryDay);
    }

    #[test]
    fn lists_next_runs() {
        let after = utc(2026, 9, 24, 23, 44);
        assert_eq!(
            next("*/15 9-17 * * 1-5", &Zone::UTC, after, 3),
            ["2026-09-25 09:00", "2026-09-25 09:15", "2026-09-25 09:30"]
        );
        // Friday's last run, then Monday.
        assert_eq!(
            next("*/15 9-17 * * 1-5", &Zone::UTC, utc(2026, 9, 25, 17, 40), 2),
            ["2026-09-25 17:45", "2026-09-28 09:00"]
        );
        // Runs strictly after the given time.
        assert_eq!(
            next("* * * * *", &Zone::UTC, utc(2026, 9, 24, 23, 44), 1),
            ["2026-09-24 23:45"]
        );
        assert_eq!(
            next("0 0 29 2 *", &Zone::UTC, after, 3),
            ["2028-02-29 00:00", "2032-02-29 00:00", "2036-02-29 00:00"]
        );
        assert_eq!(
            next("0 0 1 * 1", &Zone::UTC, after, 3),
            ["2026-09-28 00:00", "2026-10-01 00:00", "2026-10-05 00:00"]
        );
        // In the schedule's time zone: 09:00 in Tehran is 05:30 UTC.
        let runs = schedule("0 9 * * *").next_runs(&zone("Asia/Tehran"), after, 1);
        assert_eq!(runs[0].1, utc(2026, 9, 25, 5, 30));
        assert!(next("0 0 30 2 *", &Zone::UTC, after, 1).is_empty());
        assert!(next("* * * * *", &Zone::UTC, after, 0).is_empty());
    }

    #[test]
    fn handles_daylight_saving_changes_like_cron() {
        let berlin = &zone("Europe/Berlin");
        let after = utc(2026, 9, 1, 0, 0);
        // 2027-03-28: 02:00 jumps to 03:00. A fixed-time job runs right after the jump;
        // a wildcard job skips the missing hour.
        assert_eq!(next("30 2 28 3 *", berlin, after, 1), ["2027-03-28 03:00"]);
        assert_eq!(
            next("0,30 2 28 3 *", berlin, after, 1),
            ["2027-03-28 03:00"]
        );
        assert_eq!(
            next("*/30 2 28 3 *", berlin, after, 1),
            ["2028-03-28 02:00"]
        );
        // 2026-10-25: 03:00 falls back to 02:00. A fixed-time job runs once; a
        // wildcard job runs in both passes.
        assert_eq!(
            next("30 2 25 10 *", berlin, after, 2),
            ["2026-10-25 02:30", "2027-10-25 02:30"]
        );
        let runs = schedule("30 * 25 10 *").next_runs(berlin, after, 5);
        let walls: Vec<String> = runs
            .iter()
            .map(|(w, _)| w.format("%H:%M").to_string())
            .collect();
        assert_eq!(walls, ["00:30", "01:30", "02:30", "02:30", "03:30"]);
        assert_eq!(runs[3].1 - runs[2].1, Duration::hours(1));
    }

    #[test]
    fn points_out_surprises() {
        let notes = |text: &str| schedule(text).notes().join("\n");
        assert!(notes("0 0 1 * 1").contains("either one"));
        assert!(notes("0 0 1-31 * 1").contains("has no effect"));
        assert!(notes("0 0 */2 * 1").contains("robfig/cron"));
        assert!(notes("*/7 * * * *").contains(":56 and :00 are only 4 minutes apart"));
        assert!(notes("0 */5 * * *").contains("hours 20 and 0 are only 4 hours apart"));
        assert!(notes("0 0 1 */5 *").contains("November and January are only 2 months apart"));
        assert!(notes("0 0 */2 * *").contains("starts over on the 1st"));
        assert!(
            notes("0 0 31 * *").contains(
                "Skips February, April, June, September and November, which have no 31st"
            )
        );
        assert!(notes("0 0 30 * *").contains("Skips February, which has no 30th"));
        assert!(notes("0 0 29 2 *").contains("leap years"));
        assert!(notes("5/15 * * * *").contains("Linux cron rejects 5/15; write 5-59/15"));
        assert!(notes("0 0 ? * 1").contains("Linux cron rejects ?; write *"));
        for quiet in [
            "*/15 9-17 * * 1-5",
            "0 0 * * *",
            "*/10 * * * *",
            "0 0 1 * *",
        ] {
            assert!(notes(quiet).is_empty(), "{quiet}: {}", notes(quiet));
        }

        assert_eq!(
            schedule("0 0 30 2 *").never_runs().as_deref(),
            Some("February never has a 30th.")
        );
        assert_eq!(
            schedule("0 0 31 4,6 *").never_runs().as_deref(),
            Some("April and June never have a 31st.")
        );
        assert_eq!(schedule("0 0 31 * *").never_runs(), None);
        // With a weekday set, the day fields combine with "or", so it still runs.
        assert_eq!(schedule("0 0 30 2 1").never_runs(), None);
    }

    #[test]
    fn explains_a_whole_crontab() {
        let crontab = "\
# m h dom mon dow command
SHELL=/bin/bash
MAILTO=\"\"
*/5 * * * * /usr/bin/sync
@reboot /usr/bin/start
CRON_TZ=UTC
30 5 * * 1-5 /usr/bin/report
61 * * * * /bin/false
";
        let now = utc(2026, 9, 24, 23, 44);
        let tehran = zone("Asia/Tehran");
        let (text, ok) = render_crontab(crontab, None, &tehran, 5, now);
        assert!(!ok, "one line doesn't parse");
        // CRON_TZ applies to the lines below it.
        assert_eq!(
            text,
            "\
*/5 * * * * /usr/bin/sync
  Every 5 minutes
  Next: Fri 2026-09-25 03:15, in a minute

@reboot /usr/bin/start
  At startup, when the cron daemon starts

30 5 * * 1-5 /usr/bin/report
  At 05:30, Monday through Friday
  Next: Fri 2026-09-25 05:30 UTC → Fri 2026-09-25 09:00 your time, in 5 hours

61 * * * * /bin/false
  Error: minute 61 is out of range (0-59)"
        );

        // A single schedule gets the full explanation.
        let (text, ok) = render_crontab("# hi\n0 9 * * * run\n", None, &tehran, 2, now);
        assert!(ok);
        assert_eq!(
            text,
            "\
Schedule
  At 09:00 every day

Command
  run

Next runs
  Fri 2026-09-25 09:00  in 5 hours
  Sat 2026-09-26 09:00"
        );

        let (text, ok) = render_crontab("# nothing\n", None, &tehran, 5, now);
        assert!(ok);
        assert_eq!(text, "No schedules found.");
    }

    #[test]
    fn shows_times_in_both_zones() {
        let now = utc(2026, 9, 24, 23, 44);
        let tehran = zone("Asia/Tehran");
        let entry = parse_entry("30 5 * * 1-5 /usr/bin/report", Some(Zone::UTC)).unwrap();
        assert_eq!(
            render_entry(&entry, &tehran, 2, now),
            "\
Schedule (UTC)
  At 05:30, Monday through Friday

Command
  /usr/bin/report

Next runs (UTC → your time)
  Fri 2026-09-25 05:30  →  Fri 2026-09-25 09:00  in 5 hours
  Mon 2026-09-28 05:30  →  Mon 2026-09-28 09:00"
        );
        // Named, but the same clock as this computer's.
        let entry = parse_entry("CRON_TZ=Asia/Tehran */7 * * * *", None).unwrap();
        assert_eq!(
            render_entry(&entry, &tehran, 2, now),
            "\
Schedule (Asia/Tehran)
  Every 7 minutes

Next runs (Asia/Tehran)
  Fri 2026-09-25 03:21  in 7 minutes
  Fri 2026-09-25 03:28

Notes
  - Every 7 minutes starts over each hour, so :56 and :00 are only 4 minutes apart."
        );

        let never = parse_entry("0 0 30 2 *", None).unwrap();
        assert!(
            render_entry(&never, &tehran, 5, now)
                .ends_with("Next runs\n  None: February never has a 30th.")
        );
    }

    #[test]
    fn describes_values() {
        assert_eq!(shape(&[0, 15, 30, 45], 0, 59), Shape::Every(15));
        assert_eq!(
            shape(&[0, 7, 14, 21, 28, 35, 42, 49, 56], 0, 59),
            Shape::Every(7)
        );
        assert_eq!(shape(&[0, 40], 0, 59), Shape::List);
        assert_eq!(shape(&[5, 20, 35, 50], 0, 59), Shape::Step(5, 50, 15));
        assert_eq!(shape(&[9, 10, 11], 0, 23), Shape::Range(9, 11));
        assert_eq!(shape(&[3], 0, 23), Shape::One(3));
        assert_eq!(shape(&(0..24).collect::<Vec<_>>(), 0, 23), Shape::Any);

        assert_eq!(
            join_runs(&[1, 2, 3, 7, 9, 10], |v| v.to_string()),
            "1 through 3, 7, 9 and 10"
        );
        let ordinals: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 31]
            .map(ordinal)
            .to_vec();
        assert_eq!(
            ordinals.join(" "),
            "1st 2nd 3rd 4th 11th 12th 13th 21st 22nd 23rd 31st"
        );
    }
}
