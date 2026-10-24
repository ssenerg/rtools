//! Summarizes numbers piped in: count, mean, percentiles and a histogram.
//! Units like 38ms, 1.5GiB and 45% are understood, so durations and sizes
//! add up even when they are written in different units. --count tallies how
//! often each value appears instead.

use crate::{interrupt, utils};
use clap::{Parser, ValueEnum};
use regex_lite::Regex;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::process::exit;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  seq 100 | rtools stats
  grep took= app.log | rtools stats                  numbers with units, like took=38ms
  kubectl logs api --since 1h | rtools stats -k latency_ms -u ms
  ls -l | rtools stats -f 5 -u B                     file sizes
  kubectl logs api -f | rtools stats -e 'took (\\S+)'  Ctrl-C shows the results so far
  awk '{print $1}' access.log | rtools stats --count  the most common IPs

Each line gives one number. Without -f, -k or -e it is the number on the line, or
the first one with a unit when there are several. Dates, times, versions and IDs
are skipped.

Units: ns us ms s min h d for durations (Go's 1m30s too), B KB MB GB TB (1000)
and KiB MiB GiB TiB, Ki Mi Gi and K M G (1024) for sizes, and %. Note that a bare
m is minutes.")]
pub struct Args {
    /// Files to read. If omitted, reads from stdin (pipe)
    files: Vec<String>,

    /// Take the number from field N: 1 is the first, -1 the last
    #[arg(short, long, value_name = "N", allow_negative_numbers = true,
          conflicts_with_all = ["key", "regex"])]
    field: Option<i64>,

    /// Split fields on this text instead of on spaces
    #[arg(short, long, value_name = "TEXT", requires = "field")]
    delimiter: Option<String>,

    /// Take this key's value from JSON or key=value lines (a.b for nested JSON)
    #[arg(short, long, value_name = "KEY", conflicts_with = "regex")]
    key: Option<String>,

    /// Take what this regex matches, or its first group
    #[arg(short = 'e', long = "match", value_name = "REGEX")]
    regex: Option<String>,

    /// Unit of numbers written without one, like ms, s, B, KiB or %
    #[arg(short, long)]
    unit: Option<String>,

    /// Percentiles to show
    #[arg(short, long, value_name = "LIST", value_delimiter = ',',
          default_values_t = [50.0, 90.0, 95.0, 99.0])]
    percentiles: Vec<f64>,

    /// Bars in the histogram; 0 hides it
    #[arg(short, long, value_name = "N", default_value_t = 10)]
    bins: usize,

    /// Count how often each value appears: the whole line, or what -f, -k or -e pick
    #[arg(long, conflicts_with_all = ["unit", "percentiles", "bins"])]
    count: bool,

    /// With --count, how many of the most common values to show; 0 shows all
    #[arg(short = 'n', long, value_name = "N", default_value_t = 10)]
    top: usize,

    /// Print the results as JSON, with durations in seconds and sizes in bytes
    #[arg(long)]
    json: bool,

    /// When to use colors
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = ColorWhen::Auto)]
    color: ColorWhen,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

pub fn run(args: &Args, copy: bool) {
    let config = Arc::new(Config::new(args).unwrap_or_else(|e| fail(&e)));
    if args.files.is_empty() && io::stdin().is_terminal() {
        fail("nothing to read: pipe numbers in, like seq 10 | rtools stats, or give a file");
    }
    // Open every file first, so a typo fails before anything is read.
    let mut sources: Vec<Box<dyn Read + Send>> = Vec::new();
    for path in &args.files {
        let file = File::open(path).unwrap_or_else(|e| fail(&format!("can't open {path}: {e}")));
        sources.push(Box::new(file));
    }
    if sources.is_empty() {
        sources.push(Box::new(io::stdin()));
    }

    let state = Arc::new(Mutex::new(if args.count {
        State::Count(Counts::default())
    } else {
        State::Numbers(Numbers::default())
    }));
    // After Ctrl-C, show the results so far instead of dying. The command
    // feeding the pipe gets the Ctrl-C too, so the input usually ends as well.
    interrupt::catch(false);

    // Reading happens on its own thread so that Ctrl-C can end a stream that
    // never does, like kubectl logs -f, and still show what was read.
    let (done, finished) = mpsc::channel();
    {
        let state = Arc::clone(&state);
        let config = Arc::clone(&config);
        thread::spawn(move || {
            let _ = done.send(read_all(sources, &config, &state));
        });
    }

    let live = !args.json && io::stderr().is_terminal();
    let colors = use_color(args.color);
    let started = Instant::now();
    let mut status_shown = false;
    loop {
        match finished.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(())) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Err(e)) => {
                clear_status(status_shown);
                fail(&e);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if interrupt::presses() > 0 {
            break;
        }
        if live && started.elapsed() >= Duration::from_millis(400) {
            // Only copy under the lock, so reading isn't held up by the sorting.
            let snapshot = state.lock().unwrap().snapshot();
            let line = snapshot.line();
            show_status(&line, colors);
            status_shown = true;
        }
    }
    clear_status(status_shown);

    let state = state.lock().unwrap();
    let report = match &*state {
        State::Numbers(numbers) => numbers.report(&config),
        State::Count(counts) => counts.report(args.top),
    };
    let report = report.unwrap_or_else(|e| fail(&e));
    let text = if args.json {
        serde_json::to_string_pretty(&report.json).unwrap()
    } else {
        render(&report.lines, colors)
    };
    if let Err(e) = write_out(&text) {
        fail(&e);
    }
    if copy {
        let plain = if args.json {
            text
        } else {
            render(&report.lines, false)
        };
        if let Err(e) = utils::copy_to_clipboard(&plain) {
            fail(&e);
        }
    }
    if let Some(note) = report.note {
        eprintln!("{note}");
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

fn write_out(text: &str) -> Result<(), String> {
    match writeln!(io::stdout(), "{text}") {
        // A reader that stops early, like `| head`, isn't an error.
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => {
            Err(format!("failed to write the output: {e}"))
        }
        _ => Ok(()),
    }
}

// --- Picking the value out of each line --------------------------------------

struct Config {
    pick: Pick,
    /// Unit given with -u for numbers written without one.
    unit: Option<Unit>,
    percentiles: Vec<f64>,
    bins: usize,
}

enum Pick {
    /// The number on the line, preferring one with a unit.
    Auto,
    Field {
        index: i64,
        delimiter: Option<String>,
    },
    /// A JSON path for JSON lines, and a regex for key=value and key: value text.
    Key {
        path: Vec<String>,
        text: Regex,
    },
    Match(Regex),
}

impl Config {
    fn new(args: &Args) -> Result<Config, String> {
        let pick = if let Some(index) = args.field {
            if index == 0 {
                return Err("fields count from 1, or from -1 for the last".to_string());
            }
            Pick::Field {
                index,
                delimiter: args.delimiter.clone().filter(|d| !d.is_empty()),
            }
        } else if let Some(key) = &args.key {
            let text = format!(
                r#"(?:^|[^\w.-])"?{}"?\s*[=:]\s*"?([^\s",;}}\]]+)"#,
                regex_lite::escape(key)
            );
            Pick::Key {
                path: key.split('.').map(str::to_string).collect(),
                text: Regex::new(&text).map_err(|e| e.to_string())?,
            }
        } else if let Some(regex) = &args.regex {
            Pick::Match(Regex::new(regex).map_err(|e| format!("invalid regex {regex:?}: {e}"))?)
        } else {
            Pick::Auto
        };
        let unit = match &args.unit {
            Some(name) => Some(unit(name.trim()).ok_or_else(|| {
                format!("unknown unit {name:?}: use one like ms, s, min, B, KB, KiB or %")
            })?),
            None => None,
        };
        if let Some(p) = args
            .percentiles
            .iter()
            .find(|p| !(0.0..=100.0).contains(*p))
        {
            return Err(format!("percentiles go from 0 to 100, not {p}"));
        }
        Ok(Config {
            pick,
            unit,
            percentiles: args.percentiles.clone(),
            bins: args.bins,
        })
    }

    /// The text that -f, -k or -e pick from the line.
    fn pick<'a>(&self, line: &'a str) -> Option<Cow<'a, str>> {
        match &self.pick {
            Pick::Auto => Some(Cow::Borrowed(line.trim())),
            Pick::Field { index, delimiter } => {
                let fields: Vec<&str> = match delimiter {
                    Some(d) => line.split(d.as_str()).collect(),
                    None => line.split_whitespace().collect(),
                };
                let at = if *index > 0 {
                    usize::try_from(*index - 1).ok()?
                } else {
                    fields
                        .len()
                        .checked_sub(usize::try_from(index.unsigned_abs()).ok()?)?
                };
                fields.get(at).map(|f| Cow::Borrowed(f.trim()))
            }
            Pick::Key { path, text } => {
                if let Some(value) = json_value(line, path) {
                    return match value {
                        Value::String(s) => Some(Cow::Owned(s)),
                        Value::Number(n) => Some(Cow::Owned(n.to_string())),
                        Value::Bool(b) => Some(Cow::Owned(b.to_string())),
                        _ => None,
                    };
                }
                text.captures(line)
                    .and_then(|c| c.get(1))
                    .map(|m| Cow::Borrowed(m.as_str()))
            }
            Pick::Match(regex) => {
                let captures = regex.captures(line)?;
                captures
                    .get(1)
                    .or_else(|| captures.get(0))
                    .map(|m| Cow::Borrowed(m.as_str()))
            }
        }
    }
}

/// The value at `path` in the JSON object on this line, after any prefix like
/// the timestamps kubectl --timestamps adds.
fn json_value(line: &str, path: &[String]) -> Option<Value> {
    let start = line.find('{')?;
    let mut value: Value = serde_json::from_str(line[start..].trim_end()).ok()?;
    for key in path {
        value = match value {
            Value::Object(mut map) => map.remove(key)?,
            _ => return None,
        };
    }
    Some(value)
}

// --- Numbers and units ------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Plain,
    Duration,
    Bytes,
    Percent,
}

const KINDS: [Kind; 4] = [Kind::Plain, Kind::Duration, Kind::Bytes, Kind::Percent];

#[derive(Clone, Copy, Debug, PartialEq)]
struct Unit {
    kind: Kind,
    /// Seconds or bytes per unit.
    scale: f64,
    /// For sizes: whether the unit counts in 1024s (Some(true)), 1000s, or neither (B).
    binary: Option<bool>,
}

fn unit(name: &str) -> Option<Unit> {
    let duration = |scale| {
        Some(Unit {
            kind: Kind::Duration,
            scale,
            binary: None,
        })
    };
    let bytes = |scale, binary| {
        Some(Unit {
            kind: Kind::Bytes,
            scale,
            binary,
        })
    };
    const K: f64 = 1024.0;
    match name {
        "ns" | "nsec" => duration(1e-9),
        "us" | "µs" | "μs" | "usec" => duration(1e-6),
        "ms" | "msec" => duration(1e-3),
        "s" | "sec" | "secs" | "second" | "seconds" => duration(1.0),
        "m" | "min" | "mins" | "minute" | "minutes" => duration(60.0),
        "h" | "hr" | "hrs" | "hour" | "hours" => duration(3600.0),
        "d" | "day" | "days" => duration(86400.0),
        "B" | "byte" | "bytes" => bytes(1.0, None),
        "kB" | "KB" | "kb" => bytes(1e3, Some(false)),
        "MB" | "mb" => bytes(1e6, Some(false)),
        "GB" | "gb" => bytes(1e9, Some(false)),
        "TB" | "tb" => bytes(1e12, Some(false)),
        "PB" | "pb" => bytes(1e15, Some(false)),
        "K" | "Ki" | "KiB" => bytes(K, Some(true)),
        "M" | "Mi" | "MiB" => bytes(K * K, Some(true)),
        "G" | "Gi" | "GiB" => bytes(K * K * K, Some(true)),
        "T" | "Ti" | "TiB" => bytes(K * K * K * K, Some(true)),
        "P" | "Pi" | "PiB" => bytes(K * K * K * K * K, Some(true)),
        "%" => Some(Unit {
            kind: Kind::Percent,
            scale: 1.0,
            binary: None,
        }),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Quantity {
    /// In seconds for durations and bytes for sizes.
    value: f64,
    unit: Option<Unit>,
}

impl Quantity {
    fn kind(&self) -> Kind {
        self.unit.map_or(Kind::Plain, |u| u.kind)
    }
}

/// Persian and Arabic digits read as 0-9.
fn ascii_digits(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| match c {
                '۰'..='۹' => char::from(b'0' + (c as u32 - '۰' as u32) as u8),
                '٠'..='٩' => char::from(b'0' + (c as u32 - '٠' as u32) as u8),
                '٫' => '.',
                _ => c,
            })
            .collect(),
    )
}

/// Characters a number may follow, so `took=38ms` and `(12)` count but the
/// digits inside `v1.2`, `abc123` and `#42` don't.
fn starts_after(c: char) -> bool {
    c.is_whitespace() || "=:([{\"',;<>|".contains(c)
}

/// Characters a number may end at, so `10:30`, `2024-01-02`, `1/1` and
/// `10.0.0.1` aren't read as numbers.
fn ends_before(chars: &[char], at: usize) -> bool {
    match chars.get(at) {
        None => true,
        Some(&c) if c.is_whitespace() || ",;)]}\"'<>|".contains(c) => true,
        // The full stop at the end of a sentence.
        Some('.') => chars.get(at + 1).is_none_or(|c| c.is_whitespace()),
        _ => false,
    }
}

/// Reads a number and its unit starting at `start`, returning where it ends.
/// `spaced` allows any unit after a space, as in `38 ms`; otherwise only
/// longer units do, so that the s in `3 s` of running text isn't taken.
fn read_quantity(chars: &[char], start: usize, spaced: bool) -> Option<(Quantity, usize)> {
    let mut at = start;
    let (first, end) = read_number(chars, at)?;
    at = end;
    let (found, end) = match read_unit(chars, at) {
        Some((name, end)) => (Some(unit(&name)?), end),
        None if chars.get(at) == Some(&' ') => match read_unit(chars, at + 1) {
            Some((name, end)) if spaced || name.chars().count() >= 2 || name == "%" => {
                match unit(&name) {
                    Some(u) => (Some(u), end),
                    None => (None, at),
                }
            }
            _ => (None, at),
        },
        None => (None, at),
    };
    at = end;
    let mut value = first * found.map_or(1.0, |u| u.scale);
    // Go writes durations like 1h2m3.5s.
    if found.is_some_and(|u| u.kind == Kind::Duration) {
        while chars.get(at).is_some_and(char::is_ascii_digit) {
            let (part, end) = read_number(chars, at)?;
            let (name, end) = read_unit(chars, end)?;
            let part_unit = unit(&name).filter(|u| u.kind == Kind::Duration)?;
            value += part * part_unit.scale;
            at = end;
        }
    }
    Some((Quantity { value, unit: found }, at))
}

fn read_number(chars: &[char], start: usize) -> Option<(f64, usize)> {
    let mut at = start;
    let mut text = String::new();
    if let Some(&sign @ ('-' | '+')) = chars.get(at) {
        text.push(sign);
        at += 1;
    }
    let digits = |at: &mut usize, text: &mut String| {
        let from = *at;
        while let Some(&c) = chars.get(*at).filter(|c| c.is_ascii_digit()) {
            text.push(c);
            *at += 1;
        }
        *at > from
    };
    let whole = digits(&mut at, &mut text);
    if chars.get(at) == Some(&'.') && chars.get(at + 1).is_some_and(char::is_ascii_digit) {
        text.push('.');
        at += 1;
        digits(&mut at, &mut text);
    } else if !whole {
        return None;
    }
    // An exponent, as in 1.5e3, but not the e of a word.
    if let Some('e' | 'E') = chars.get(at) {
        let mut exponent = String::from("e");
        let mut end = at + 1;
        if let Some(&sign @ ('-' | '+')) = chars.get(end) {
            exponent.push(sign);
            end += 1;
        }
        if digits(&mut end, &mut exponent) && !chars.get(end).is_some_and(|c| c.is_alphabetic()) {
            text.push_str(&exponent);
            at = end;
        }
    }
    text.parse().ok().map(|n| (n, at))
}

fn read_unit(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start) == Some(&'%') {
        return Some(("%".to_string(), start + 1));
    }
    let mut at = start;
    while chars
        .get(at)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == 'µ' || *c == 'μ')
    {
        at += 1;
    }
    (at > start).then(|| (chars[start..at].iter().collect(), at))
}

/// The numbers on a line, skipping the digits in dates, times, versions,
/// addresses and IDs.
fn scan(line: &str) -> Vec<Quantity> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let begins = c.is_ascii_digit()
            || (matches!(c, '-' | '+') && chars.get(at + 1).is_some_and(char::is_ascii_digit));
        if begins && (at == 0 || starts_after(chars[at - 1])) {
            if let Some((quantity, end)) = read_quantity(&chars, at, false)
                && ends_before(&chars, end)
            {
                found.push(quantity);
                at = end;
                continue;
            }
            // Skip the rest of the word, so `10.0.0.1` doesn't yield `0.1`.
            while at < chars.len() && !chars[at].is_whitespace() {
                at += 1;
            }
            continue;
        }
        at += 1;
    }
    found
}

/// A whole field or value as a quantity, like `38ms`, `"1.5 GiB"` or `12,`.
fn parse_quantity(text: &str) -> Option<Quantity> {
    let text = ascii_digits(text);
    let text = text
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | ',' | ';'))
        .trim();
    let chars: Vec<char> = text.chars().collect();
    match read_quantity(&chars, 0, true)? {
        (quantity, end) if end == chars.len() => Some(quantity),
        _ => None,
    }
}

// --- Collecting -------------------------------------------------------------

enum State {
    Numbers(Numbers),
    Count(Counts),
}

impl State {
    fn snapshot(&self) -> Status {
        match self {
            State::Numbers(numbers) => {
                let (kind, _) = numbers.main_kind();
                Status::Numbers {
                    values: numbers.values_of(kind),
                    style: numbers.style(kind),
                    without: numbers.without,
                }
            }
            State::Count(counts) => Status::Count(counts.status()),
        }
    }
}

/// What the one-line summary shown while a stream is still coming in needs.
enum Status {
    Numbers {
        values: Vec<f64>,
        style: Style,
        without: usize,
    },
    Count(String),
}

impl Status {
    fn line(self) -> String {
        let (mut values, style, without) = match self {
            Status::Count(line) => return line,
            Status::Numbers {
                values,
                style,
                without,
            } => (values, style, without),
        };
        if values.is_empty() {
            return format!(
                "no numbers yet in {} lines · Ctrl-C to finish",
                group(without)
            );
        }
        let n = values.len();
        // The nearest value is close enough for a line that changes as it's read.
        let mut at = |p: f64| {
            let rank = (p / 100.0 * (n - 1) as f64).round() as usize;
            *values.select_nth_unstable_by(rank, f64::total_cmp).1
        };
        let (p50, p95, max) = (at(50.0), at(95.0), at(100.0));
        format!(
            "{} values · p50 {} · p95 {} · max {} · Ctrl-C to finish",
            group(n),
            style.format(p50),
            style.format(p95),
            style.format(max),
        )
    }
}

fn read_all(
    sources: Vec<Box<dyn Read + Send>>,
    config: &Config,
    state: &Mutex<State>,
) -> Result<(), String> {
    for source in sources {
        let mut reader = BufReader::new(source);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("failed to read: {e}")),
            }
            let line = String::from_utf8_lossy(&buffer);
            let line = line.trim_end_matches(['\n', '\r']);
            match &mut *state.lock().unwrap() {
                State::Numbers(numbers) => numbers.add(line, config),
                State::Count(counts) => counts.add(line, config),
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Numbers {
    /// Values by kind; the most common kind is the one reported.
    values: HashMap<Kind, Vec<f64>>,
    /// Sizes seen in 1024s and in 1000s, to show them the same way.
    binary: usize,
    decimal: usize,
    /// Lines with no number, not counting blank ones.
    without: usize,
    /// Lines where the first of several numbers was taken.
    ambiguous: usize,
}

impl Numbers {
    fn add(&mut self, line: &str, config: &Config) {
        if line.trim().is_empty() {
            return;
        }
        let quantity = match &config.pick {
            Pick::Auto => {
                let line = ascii_digits(line);
                let found = scan(&line);
                let with_unit: Vec<&Quantity> = found.iter().filter(|q| q.unit.is_some()).collect();
                let choices = if with_unit.is_empty() {
                    found.len()
                } else {
                    with_unit.len()
                };
                if choices > 1 {
                    self.ambiguous += 1;
                }
                with_unit.first().copied().or(found.first()).copied()
            }
            _ => config.pick(line).and_then(|text| parse_quantity(&text)),
        };
        let Some(mut quantity) = quantity else {
            self.without += 1;
            return;
        };
        if quantity.unit.is_none()
            && let Some(unit) = config.unit
        {
            quantity = Quantity {
                value: quantity.value * unit.scale,
                unit: Some(unit),
            };
        }
        match quantity.unit.and_then(|u| u.binary) {
            Some(true) => self.binary += 1,
            Some(false) => self.decimal += 1,
            None => {}
        }
        self.values
            .entry(quantity.kind())
            .or_default()
            .push(quantity.value);
    }

    /// The kind most values have, and how many values of other kinds there are.
    fn main_kind(&self) -> (Kind, usize) {
        let count = |kind: &Kind| self.values.get(kind).map_or(0, Vec::len);
        let kind = KINDS
            .iter()
            .rev()
            .max_by_key(|k| count(k))
            .copied()
            .unwrap_or(Kind::Plain);
        let total: usize = KINDS.iter().map(count).sum();
        (kind, total - count(&kind) - self.zeros_joining(kind))
    }

    /// Zeros written without a unit, like du's 0, which are zero in any unit.
    fn zeros_joining(&self, kind: Kind) -> usize {
        if kind == Kind::Plain {
            return 0;
        }
        self.values
            .get(&Kind::Plain)
            .map_or(0, |v| v.iter().filter(|x| **x == 0.0).count())
    }

    /// The values of `kind`.
    fn values_of(&self, kind: Kind) -> Vec<f64> {
        let mut values = self.values.get(&kind).cloned().unwrap_or_default();
        values.extend(std::iter::repeat_n(0.0, self.zeros_joining(kind)));
        values
    }

    /// The values of `kind`, sorted.
    fn sorted(&self, kind: Kind) -> Vec<f64> {
        let mut sorted = self.values_of(kind);
        sorted.sort_by(f64::total_cmp);
        sorted
    }

    fn style(&self, kind: Kind) -> Style {
        Style {
            kind,
            binary: self.binary > self.decimal,
        }
    }

    fn report(&self, config: &Config) -> Result<Report, String> {
        let (kind, other_units) = self.main_kind();
        let sorted = self.sorted(kind);
        if sorted.is_empty() {
            return Err(match self.without {
                0 => "no input".to_string(),
                1 => "no number in the 1 line read".to_string(),
                n => format!("no numbers in the {} lines read", group(n)),
            });
        }
        let style = self.style(kind);
        let n = sorted.len();
        let sum: f64 = sorted.iter().sum();
        let mean = sum / n as f64;
        let variance = sorted.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
        let stddev = variance.sqrt();
        let (min, max) = (sorted[0], sorted[n - 1]);

        let mut skipped = Vec::new();
        if self.without > 0 {
            let lines = if self.without == 1 { "line" } else { "lines" };
            skipped.push(format!("{} {lines} without a number", group(self.without)));
        }
        if other_units > 0 {
            skipped.push(format!("{} in other units", group(other_units)));
        }
        let mut count = group(n);
        if !skipped.is_empty() {
            count = format!("{count}   (skipped {})", skipped.join(", "));
        }

        let mut lines = vec![Line::Stat("count", count)];
        // A sum of percentages means nothing.
        if kind != Kind::Percent {
            lines.push(Line::Stat("sum", style.format(sum)));
        }
        lines.push(Line::Stat("min", style.format(min)));
        lines.push(Line::Stat("mean", style.format(mean)));
        lines.push(Line::Stat("stddev", style.format(stddev)));
        lines.push(Line::Stat("max", style.format(max)));
        if !config.percentiles.is_empty() {
            lines.push(Line::Blank);
            for &p in &config.percentiles {
                lines.push(Line::Percentile(p, style.format(percentile(&sorted, p))));
            }
        }
        let bins = if config.bins > 0 {
            histogram(&sorted, config.bins, style)
        } else {
            Vec::new()
        };
        if bins.len() > 1 {
            lines.push(Line::Blank);
            let most = bins.iter().map(|b| b.count).max().unwrap_or(1);
            for bin in &bins {
                // 0 ms rather than a bare 0 next to 25 ms.
                let from = if bin.from == 0.0 {
                    let (name, size) = style.unit_for(bin.to.abs());
                    format!("{} {name}", short(0.0 / size))
                        .trim_end()
                        .to_string()
                } else {
                    style.format(bin.from)
                };
                lines.push(Line::Bar {
                    label: (from, style.format(bin.to)),
                    count: bin.count,
                    share: bin.count as f64 / n as f64,
                    width: bin.count as f64 / most as f64,
                });
            }
        }

        let percentiles: Map<String, Value> = config
            .percentiles
            .iter()
            .map(|&p| {
                (
                    format!("p{}", trim_number(p)),
                    json!(percentile(&sorted, p)),
                )
            })
            .collect();
        let json = json!({
            "count": n,
            "unit": match kind {
                Kind::Plain => Value::Null,
                Kind::Duration => json!("seconds"),
                Kind::Bytes => json!("bytes"),
                Kind::Percent => json!("percent"),
            },
            "sum": sum,
            "min": min,
            "mean": mean,
            "stddev": stddev,
            "max": max,
            "percentiles": percentiles,
            "histogram": bins.iter().map(|b| json!({"from": b.from, "to": b.to, "count": b.count})).collect::<Vec<_>>(),
            "skipped": { "without_number": self.without, "other_units": other_units },
        });

        let note = (matches!(config.pick, Pick::Auto) && self.ambiguous > 0).then(|| {
            let lines = if self.ambiguous == 1 {
                "line had"
            } else {
                "lines had"
            };
            format!(
                "Note: {} {lines} several numbers and the first was used. \
                 Pick one with -f, -k or -e.",
                group(self.ambiguous)
            )
        });
        Ok(Report { lines, json, note })
    }
}

#[derive(Default)]
struct Counts {
    /// How often each value appeared, and when it first did, to break ties.
    seen: HashMap<String, (usize, usize)>,
    total: usize,
    /// Lines -f, -k or -e found nothing in.
    without: usize,
}

impl Counts {
    fn add(&mut self, line: &str, config: &Config) {
        if line.trim().is_empty() {
            return;
        }
        let Some(value) = config.pick(line).filter(|v| !v.is_empty()) else {
            self.without += 1;
            return;
        };
        let order = self.seen.len();
        self.seen.entry(value.into_owned()).or_insert((0, order)).0 += 1;
        self.total += 1;
    }

    fn ranked(&self) -> Vec<(&str, usize)> {
        let mut ranked: Vec<(&str, usize, usize)> = self
            .seen
            .iter()
            .map(|(v, &(count, order))| (v.as_str(), count, order))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        ranked.into_iter().map(|(v, count, _)| (v, count)).collect()
    }

    fn status(&self) -> String {
        let top = self
            .seen
            .iter()
            .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.1.1.cmp(&a.1.1)));
        let top = top.map(|(v, (c, _))| format!(" · top {v} ({})", group(*c)));
        format!(
            "{} lines · {} different{} · Ctrl-C to finish",
            group(self.total),
            group(self.seen.len()),
            top.unwrap_or_default()
        )
    }

    fn report(&self, top: usize) -> Result<Report, String> {
        if self.total == 0 {
            return Err(match self.without {
                0 => "no input".to_string(),
                n => format!("nothing picked from the {} lines read", group(n)),
            });
        }
        let ranked = self.ranked();
        let shown = if top == 0 {
            ranked.len()
        } else {
            top.min(ranked.len())
        };
        let mut summary = format!(
            "{} lines · {} different",
            group(self.total),
            group(ranked.len())
        );
        if self.without > 0 {
            summary.push_str(&format!(
                "   (skipped {} without the value)",
                group(self.without)
            ));
        }
        let mut lines = vec![Line::Text(summary), Line::Blank];
        let most = ranked[0].1;
        for &(value, count) in &ranked[..shown] {
            lines.push(Line::Tally {
                label: value.to_string(),
                count,
                share: count as f64 / self.total as f64,
                width: count as f64 / most as f64,
            });
        }
        let rest = &ranked[shown..];
        if !rest.is_empty() {
            let count: usize = rest.iter().map(|(_, c)| c).sum();
            let others = if rest.len() == 1 { "other" } else { "others" };
            lines.push(Line::Tally {
                label: format!("({} {others})", group(rest.len())),
                count,
                share: count as f64 / self.total as f64,
                width: 0.0,
            });
        }
        let json = json!({
            "total": self.total,
            "different": ranked.len(),
            "values": ranked[..shown]
                .iter()
                .map(|(value, count)| json!({"value": value, "count": count}))
                .collect::<Vec<_>>(),
        });
        Ok(Report {
            lines,
            json,
            note: None,
        })
    }
}

/// The value below which `p` percent of the sorted values fall, interpolating
/// between neighbors.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = p / 100.0 * (sorted.len() - 1) as f64;
    let (below, above) = (rank.floor() as usize, rank.ceil() as usize);
    sorted[below] + (sorted[above] - sorted[below]) * (rank - below as f64)
}

// --- Histogram ----------------------------------------------------------------

#[derive(Debug, PartialEq)]
struct Bin {
    from: f64,
    to: f64,
    count: usize,
}

/// About `wanted` bins with round edges. Values that span several orders of
/// magnitude with a long tail, like latencies, get edges that grow by a factor
/// (1, 2, 5, 10, …) so that the fast majority and the slow tail both show.
fn histogram(sorted: &[f64], wanted: usize, style: Style) -> Vec<Bin> {
    let (min, max) = (sorted[0], sorted[sorted.len() - 1]);
    if min == max {
        return vec![Bin {
            from: min,
            to: max,
            count: sorted.len(),
        }];
    }
    // Growing bins suit values bunched near the low end with a long tail.
    let median = percentile(sorted, 50.0);
    let skewed = (median - min) / (max - min) < 0.2;
    let edges = if min > 0.0 && max / min >= 100.0 && skewed {
        growing_edges(min, max, wanted, style)
    } else {
        even_edges(
            min,
            max,
            wanted,
            style,
            sorted.iter().all(|x| x.fract() == 0.0),
        )
    };
    let mut bins: Vec<Bin> = edges
        .windows(2)
        .map(|w| Bin {
            from: w[0],
            to: w[1],
            count: 0,
        })
        .collect();
    let last = bins.len() - 1;
    let mut index = 0;
    for &x in sorted {
        // The last bin includes its upper edge.
        while index < last && x >= bins[index].to {
            index += 1;
        }
        bins[index].count += 1;
    }
    while bins.first().is_some_and(|b| b.count == 0) {
        bins.remove(0);
    }
    while bins.last().is_some_and(|b| b.count == 0) {
        bins.pop();
    }
    bins
}

fn even_edges(min: f64, max: f64, wanted: usize, style: Style, whole: bool) -> Vec<f64> {
    // Round steps in the unit the values are shown in: 5 ms, not 0.005 s.
    let scale = style.unit_for(max.abs().max(min.abs())).1;
    let mut step = nice((max - min) / scale / wanted as f64) * scale;
    if whole && style.kind == Kind::Plain {
        step = step.max(1.0);
    }
    let first = (min / step).floor();
    let mut edges = Vec::new();
    let mut i = 0.0;
    loop {
        let edge = (first + i) * step;
        edges.push(edge);
        if edge >= max {
            break;
        }
        i += 1.0;
    }
    edges
}

/// The smallest of 1, 2, 2.5 and 5 times a power of ten that's at least `x`.
fn nice(x: f64) -> f64 {
    let power = 10f64.powf(x.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * power)
        .find(|&s| s >= x * (1.0 - 1e-9))
        .unwrap()
}

fn growing_edges(min: f64, max: f64, wanted: usize, style: Style) -> Vec<f64> {
    // Candidate ladders from fine to coarse; the finest that fits is used.
    let ladders: Vec<Vec<f64>> = if style.kind == Kind::Bytes && style.binary {
        [1, 2, 3, 5, 10]
            .iter()
            .map(|&k| vec![2f64.powi(k)])
            .collect()
    } else {
        vec![vec![1.0, 2.0, 5.0], vec![1.0, 3.0], vec![1.0], vec![1.0]]
    };
    let mut edges = Vec::new();
    for (i, ladder) in ladders.iter().enumerate() {
        edges = if ladder.len() == 1 && ladder[0] > 1.0 {
            powers(min, max, ladder[0])
        } else if i == 3 {
            // Every other power of ten.
            powers(min, max, 100.0)
        } else {
            decade_ladder(min, max, ladder)
        };
        if edges.len() <= wanted + 2 {
            break;
        }
    }
    edges
}

/// Edges at powers of `factor`, from the one at or below `min` to the one at or above `max`.
fn powers(min: f64, max: f64, factor: f64) -> Vec<f64> {
    let mut exponent = (min.ln() / factor.ln()).floor() as i32;
    let mut edges = vec![];
    loop {
        let edge = factor.powi(exponent);
        if edge > min * (1.0 + 1e-12) && edges.is_empty() {
            exponent -= 1;
            continue;
        }
        edges.push(edge);
        if edge >= max {
            return edges;
        }
        exponent += 1;
    }
}

/// Edges at the ladder's steps in each power of ten, like 1, 2, 5, 10, 20, 50.
fn decade_ladder(min: f64, max: f64, ladder: &[f64]) -> Vec<f64> {
    let mut power = min.log10().floor() as i32;
    let mut step = 0;
    let at = |power: i32, step: usize| ladder[step] * 10f64.powi(power);
    // Step back to the largest edge at or below min.
    while at(power, step) > min * (1.0 + 1e-12) {
        if step == 0 {
            power -= 1;
            step = ladder.len() - 1;
        } else {
            step -= 1;
        }
    }
    let mut edges = Vec::new();
    loop {
        let edge = at(power, step);
        edges.push(edge);
        if edge >= max * (1.0 - 1e-12) {
            return edges;
        }
        step += 1;
        if step == ladder.len() {
            step = 0;
            power += 1;
        }
    }
}

// --- Formatting -----------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Style {
    kind: Kind,
    /// Sizes in KiB, MiB, … rather than KB, MB, …
    binary: bool,
}

impl Style {
    /// The unit to show `magnitude` in, and its size in seconds or bytes.
    fn unit_for(&self, magnitude: f64) -> (&'static str, f64) {
        let pick = |units: &[(&'static str, f64)]| {
            units
                .iter()
                .rev()
                .find(|(_, size)| magnitude >= *size)
                .copied()
                .unwrap_or(units[0])
        };
        match self.kind {
            Kind::Plain | Kind::Percent => ("", 1.0),
            Kind::Duration => pick(&[
                ("ns", 1e-9),
                ("µs", 1e-6),
                ("ms", 1e-3),
                ("s", 1.0),
                ("min", 60.0),
                ("h", 3600.0),
                ("d", 86400.0),
            ]),
            Kind::Bytes if self.binary => pick(&[
                ("B", 1.0),
                ("KiB", 1024.0),
                ("MiB", 1024f64.powi(2)),
                ("GiB", 1024f64.powi(3)),
                ("TiB", 1024f64.powi(4)),
                ("PiB", 1024f64.powi(5)),
            ]),
            Kind::Bytes => pick(&[
                ("B", 1.0),
                ("KB", 1e3),
                ("MB", 1e6),
                ("GB", 1e9),
                ("TB", 1e12),
                ("PB", 1e15),
            ]),
        }
    }

    fn format(&self, value: f64) -> String {
        match self.kind {
            Kind::Plain => plain(value),
            Kind::Percent => format!("{}%", short(value)),
            _ if value == 0.0 => "0".to_string(),
            _ => {
                let (name, size) = self.unit_for(value.abs());
                format!("{} {name}", short(value / size))
            }
        }
    }
}

/// Whole numbers exactly, others to 4 significant digits.
fn plain(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{x:.0}")
    } else {
        significant(x, 4)
    }
}

/// Whole numbers exactly, others to 3 significant digits, for values with a unit.
fn short(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{x:.0}")
    } else {
        significant(x, 3)
    }
}

fn significant(x: f64, digits: usize) -> String {
    if x == 0.0 || !x.is_finite() {
        return x.to_string();
    }
    let rounded: f64 = format!("{:.*e}", digits - 1, x).parse().unwrap();
    let exponent = rounded.abs().log10().floor() as i32;
    if !(-6..15).contains(&exponent) {
        return format!("{:.*e}", digits - 1, rounded).replace(".0e", "e");
    }
    let decimals = (digits as i32 - 1 - exponent).max(0) as usize;
    trim_number_text(format!("{rounded:.decimals$}"))
}

fn trim_number(x: f64) -> String {
    trim_number_text(format!("{x}"))
}

fn trim_number_text(text: String) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// 1234567 as 1,234,567.
fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

struct Report {
    lines: Vec<Line>,
    json: Value,
    /// Advice for stderr, after the results.
    note: Option<String>,
}

enum Line {
    Blank,
    Text(String),
    Stat(&'static str, String),
    Percentile(f64, String),
    /// A histogram bar: the bin's edges, its count, its share of all values,
    /// and its length relative to the longest bar.
    Bar {
        label: (String, String),
        count: usize,
        share: f64,
        width: f64,
    },
    /// A --count row.
    Tally {
        label: String,
        count: usize,
        share: f64,
        width: f64,
    },
}

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const BAR: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

fn render(lines: &[Line], color: bool) -> String {
    let paint = |text: &str, style: &str| {
        if color {
            format!("{style}{text}{RESET}")
        } else {
            text.to_string()
        }
    };
    let from_width = lines
        .iter()
        .filter_map(|l| match l {
            Line::Bar { label, .. } => Some(label.0.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let to_width = lines
        .iter()
        .filter_map(|l| match l {
            Line::Bar { label, .. } => Some(label.1.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let count_width = lines
        .iter()
        .filter_map(|l| match l {
            Line::Bar { count, .. } | Line::Tally { count, .. } => Some(group(*count).len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    let mut out = Vec::new();
    for line in lines {
        out.push(match line {
            Line::Blank => String::new(),
            Line::Text(text) => text.clone(),
            Line::Stat(name, value) => {
                // Keep the note after count dim, so the number stands out.
                let (value, rest) = value.split_once("   ").unwrap_or((value, ""));
                let rest = if rest.is_empty() {
                    String::new()
                } else {
                    format!("   {}", paint(rest, DIM))
                };
                format!(
                    "{}  {}{rest}",
                    paint(&format!("{name:<6}"), DIM),
                    paint(value, BOLD)
                )
            }
            Line::Percentile(p, value) => {
                let name = format!("p{}", trim_number(*p));
                format!(
                    "{}  {}",
                    paint(&format!("{name:<6}"), DIM),
                    paint(value, BOLD)
                )
            }
            Line::Bar {
                label,
                count,
                share,
                width,
            } => format!(
                "{:>from_width$} – {:<to_width$}  {}  {:>count_width$}  {}",
                label.0,
                label.1,
                paint(&bar(*width, 30, *count > 0), BAR),
                group(*count),
                paint(&format!("{:>5.1}%", share * 100.0), DIM),
            ),
            Line::Tally {
                label,
                count,
                share,
                width,
            } => format!(
                "{:>count_width$}  {}  {}  {label}",
                group(*count),
                paint(&format!("{:>5.1}%", share * 100.0), DIM),
                paint(&bar(*width, 20, *width > 0.0), BAR),
            ),
        });
    }
    out.join("\n")
}

/// A bar `fraction` of `full` characters long, in eighths, padded to `full`.
/// Nonzero bars get at least a sliver.
fn bar(fraction: f64, full: usize, nonzero: bool) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (fraction * full as f64 * 8.0).round() as usize;
    let eighths = if nonzero { eighths.max(1) } else { eighths };
    let mut text = "█".repeat(eighths / 8);
    if eighths % 8 > 0 {
        text.push(PARTS[eighths % 8]);
    }
    let length = text.chars().count();
    text + &" ".repeat(full.saturating_sub(length))
}

// --- Terminal ----------------------------------------------------------------

fn use_color(when: ColorWhen) -> bool {
    let wanted = match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        }
    };
    // Windows consoles need colors switched on first.
    #[cfg(windows)]
    let wanted = wanted && crossterm::ansi_support::supports_ansi();
    wanted
}

fn show_status(line: &str, color: bool) {
    #[cfg(windows)]
    let _ = crossterm::ansi_support::supports_ansi();
    let width = terminal_size::terminal_size_of(io::stderr()).map_or(80, |(w, _)| w.0 as usize);
    let line: String = line.chars().take(width.saturating_sub(1)).collect();
    let line = if color {
        format!("{DIM}{line}{RESET}")
    } else {
        line
    };
    let mut err = io::stderr();
    let _ = write!(err, "\r\x1b[K{line}");
    let _ = err.flush();
}

fn clear_status(shown: bool) {
    if shown {
        let mut err = io::stderr();
        let _ = write!(err, "\r\x1b[K");
        let _ = err.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Args {
        let mut all = vec!["stats"];
        all.extend_from_slice(extra);
        Args::parse_from(all)
    }

    fn numbers(input: &str, extra: &[&str]) -> (Numbers, Config) {
        let config = Config::new(&args(extra)).unwrap();
        let mut numbers = Numbers::default();
        for line in input.lines() {
            numbers.add(line, &config);
        }
        (numbers, config)
    }

    fn values(input: &str, extra: &[&str]) -> Vec<f64> {
        let (numbers, _) = numbers(input, extra);
        let (kind, _) = numbers.main_kind();
        numbers.values.get(&kind).cloned().unwrap_or_default()
    }

    fn text(input: &str, extra: &[&str]) -> String {
        let (numbers, config) = numbers(input, extra);
        render(&numbers.report(&config).unwrap().lines, false)
    }

    fn found(line: &str) -> Vec<String> {
        scan(line)
            .iter()
            .map(|q| match q.unit {
                None => plain(q.value),
                Some(u) => Style {
                    kind: u.kind,
                    binary: u.binary == Some(true),
                }
                .format(q.value),
            })
            .collect()
    }

    #[test]
    fn finds_numbers_but_not_dates_versions_or_ids() {
        assert_eq!(found("42"), ["42"]);
        assert_eq!(found("-3.5 and +2e3"), ["-3.5", "2000"]);
        assert_eq!(
            found("2024-05-01T10:00:00Z GET /api 200 took=38ms"),
            ["200", "38 ms"]
        );
        assert_eq!(
            found("10.0.0.1 v1.2.3 abc123 #42 1/1 10:30 3fa2"),
            Vec::<String>::new()
        );
        assert_eq!(found(r#"{"latency":38,"status":200}"#), ["38", "200"]);
        assert_eq!(found("done in 1.5 s."), ["1.5"]);
        assert_eq!(found("took 38 ms, used 12.5 MB"), ["38 ms", "12.5 MB"]);
        assert_eq!(found("cpu 45% (load 1.2)"), ["45%", "1.2"]);
        assert_eq!(found("elapsed=1m30s size=4.0K"), ["1.5 min", "4 KiB"]);
        assert_eq!(found("۱۲ تا"), Vec::<String>::new()); // only after ascii_digits
        assert_eq!(found(&ascii_digits("۱۲ تا")), ["12"]);
        assert_eq!(found("3 seconds"), ["3 s"]);
        assert_eq!(found("5 apples"), ["5"]);
    }

    #[test]
    fn reads_units() {
        let q = |t| parse_quantity(t).map(|q| (q.value, q.kind()));
        assert_eq!(q("38ms"), Some((0.038, Kind::Duration)));
        assert_eq!(q("1h2m3.5s"), Some((3723.5, Kind::Duration)));
        assert_eq!(q("250µs"), Some((250e-6, Kind::Duration)));
        assert_eq!(
            q("\"1.5 GiB\","),
            Some((1.5 * 1024f64.powi(3), Kind::Bytes))
        );
        assert_eq!(q("128Mi"), Some((128.0 * 1024f64.powi(2), Kind::Bytes)));
        assert_eq!(q("2MB"), Some((2e6, Kind::Bytes)));
        assert_eq!(q("12"), Some((12.0, Kind::Plain)));
        assert_eq!(q("99.5%"), Some((99.5, Kind::Percent)));
        assert_eq!(q("12 s"), Some((12.0, Kind::Duration)));
        assert_eq!(q("12apples"), None);
        assert_eq!(q("abc"), None);
        assert_eq!(q("1.2.3"), None);
    }

    #[test]
    fn summarizes_plain_numbers() {
        let input: String = (1..=100).map(|n| format!("{n}\n")).collect();
        assert_eq!(
            text(&input, &["-b", "0"]),
            "count   100\nsum     5050\nmin     1\nmean    50.5\nstddev  28.87\nmax     100\n\n\
             p50     50.5\np90     90.1\np95     95.05\np99     99.01"
        );
    }

    #[test]
    fn mixes_units_of_one_kind() {
        let (numbers, config) = numbers("took 500ms\ntook 1.5s\ntook 2s\nno number here\n", &[]);
        let report = numbers.report(&config).unwrap();
        let text = render(&report.lines, false);
        assert!(text.starts_with("count   3   (skipped 1 line without a number)\nsum     4 s\nmin     500 ms\nmean    1.33 s\n"), "{text}");
        assert_eq!(report.json["unit"], "seconds");
        assert_eq!(report.json["sum"], 4.0);
        // The kind most values have wins, and the others are skipped.
        let text = text_of("1s\n2s\n3 MB\n");
        assert!(
            text.starts_with("count   2   (skipped 1 in other units)"),
            "{text}"
        );
        // A zero without a unit is zero in any unit.
        let text = text_of("0\n4K\n8K\n");
        assert!(
            text.starts_with("count   3\nsum     12 KiB\nmin     0\n"),
            "{text}"
        );
    }

    fn text_of(input: &str) -> String {
        text(input, &[])
    }

    #[test]
    fn shows_sizes_the_way_they_were_written() {
        assert!(text_of("1.5GiB\n512MiB\n").contains("max     1.5 GiB"));
        assert!(text_of("1.5GB\n512MB\n").contains("max     1.5 GB"));
        assert!(text("4096\n1000\n", &["-u", "B"]).contains("max     4.1 KB"));
    }

    #[test]
    fn picks_fields_keys_and_matches() {
        let lines = "a 1 x\nb 2 y\nc 3 z\n";
        assert_eq!(values(lines, &["-f", "2"]), [1.0, 2.0, 3.0]);
        assert_eq!(
            values("1,2,30\n4,5,60\n", &["-f", "-1", "-d", ","]),
            [30.0, 60.0]
        );
        let json = r#"2024-05-01T10:00:00Z {"http":{"latency_ms":38,"status":200}}
{"http":{"latency_ms":"12.5","status":500}}"#;
        assert_eq!(
            values(json, &["-k", "http.latency_ms", "-u", "ms"]),
            [0.038, 0.0125]
        );
        assert_eq!(
            values("level=info took=38ms\ntook: 2s\n", &["-k", "took"]),
            [0.038, 2.0]
        );
        assert_eq!(
            values("x=1 took 40ms\nx=2 took 2ms\n", &["-e", r"took (\S+)"]),
            [0.04, 0.002]
        );
        assert_eq!(values("a12b\n", &["-e", r"\d+"]), [12.0]);
    }

    #[test]
    fn notes_lines_with_several_numbers() {
        let (numbers, config) = numbers("GET 200 38\nGET 404 2\n", &[]);
        let report = numbers.report(&config).unwrap();
        assert_eq!(report.json["max"], 404.0);
        assert!(report.note.unwrap().contains("2 lines had several numbers"));
        // A unit settles it.
        let (numbers, config) = numbers_of("GET 200 38ms\n");
        assert!(numbers.report(&config).unwrap().note.is_none());
    }

    fn numbers_of(input: &str) -> (Numbers, Config) {
        numbers(input, &[])
    }

    #[test]
    fn reports_when_nothing_was_found() {
        let (numbers, config) = numbers("no\nnumbers\n", &[]);
        assert_eq!(
            numbers.report(&config).err().unwrap(),
            "no numbers in the 2 lines read"
        );
    }

    #[test]
    fn computes_percentiles() {
        let sorted = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&sorted, 0.0), 1.0);
        assert_eq!(percentile(&sorted, 50.0), 2.5);
        assert_eq!(percentile(&sorted, 100.0), 4.0);
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
        assert!(Config::new(&args(&["-p", "101"])).is_err());
        assert!(text("1\n2\n", &["-p", "99.9"]).contains("p99.9   1.999"));
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(plain(1234567.0), "1234567");
        assert_eq!(plain(2.0 / 3.0), "0.6667");
        assert_eq!(plain(1e20 / 3.0), "3.333e19");
        assert_eq!(plain(0.000012346), "0.00001235");
        assert_eq!(group(1234567), "1,234,567");
        assert_eq!(group(12), "12");
        let duration = Style {
            kind: Kind::Duration,
            binary: false,
        };
        assert_eq!(duration.format(0.0613), "61.3 ms");
        assert_eq!(duration.format(1.204), "1.2 s");
        assert_eq!(duration.format(90.0), "1.5 min");
        assert_eq!(duration.format(250e-9), "250 ns");
        let bytes = Style {
            kind: Kind::Bytes,
            binary: true,
        };
        assert_eq!(bytes.format(1536.0), "1.5 KiB");
        assert_eq!(bytes.format(100.0), "100 B");
    }

    #[test]
    fn draws_histograms() {
        let style = Style {
            kind: Kind::Plain,
            binary: false,
        };
        let sorted: Vec<f64> = (1..=100).map(f64::from).collect();
        let bins = histogram(&sorted, 10, style);
        assert_eq!(bins.len(), 10);
        assert_eq!((bins[0].from, bins[0].to, bins[0].count), (0.0, 10.0, 9));
        assert_eq!((bins[9].from, bins[9].to, bins[9].count), (90.0, 100.0, 11));
        assert_eq!(bins.iter().map(|b| b.count).sum::<usize>(), 100);

        // Latencies spanning orders of magnitude get growing bins.
        let duration = Style {
            kind: Kind::Duration,
            binary: false,
        };
        let sorted = [0.003, 0.004, 0.02, 0.03, 0.04, 0.2, 3.2];
        let bins = histogram(&sorted, 10, duration);
        let edges: Vec<String> = bins.iter().map(|b| duration.format(b.from)).collect();
        assert_eq!(
            edges,
            [
                "2 ms", "5 ms", "10 ms", "20 ms", "50 ms", "100 ms", "200 ms", "500 ms", "1 s",
                "2 s"
            ]
        );
        assert_eq!(duration.format(bins.last().unwrap().to), "5 s");
        assert_eq!(bins.iter().map(|b| b.count).sum::<usize>(), 7);

        let same = histogram(&[5.0, 5.0], 10, style);
        assert_eq!(
            same,
            [Bin {
                from: 5.0,
                to: 5.0,
                count: 2
            }]
        );
    }

    #[test]
    fn draws_bars() {
        assert_eq!(bar(1.0, 4, true), "████");
        assert_eq!(bar(0.5, 4, true), "██  ");
        assert_eq!(bar(0.0001, 4, true), "▏   ");
        assert_eq!(bar(0.0, 4, false), "    ");
    }

    #[test]
    fn counts_values() {
        let config = Config::new(&args(&["--count", "-n", "2"])).unwrap();
        let mut counts = Counts::default();
        for line in "b\na\nb\nc\nb\na\n\n".lines() {
            counts.add(line, &config);
        }
        let report = counts.report(2).unwrap();
        assert_eq!(
            render(&report.lines, false),
            "6 lines · 3 different\n\n3   50.0%  ████████████████████  b\n\
             2   33.3%  █████████████▍        a\n1   16.7%                        (1 other)"
        );
        assert_eq!(report.json["values"][0], json!({"value": "b", "count": 3}));

        let config = Config::new(&args(&["--count", "-f", "1"])).unwrap();
        let mut counts = Counts::default();
        for line in "10.0.0.1 GET\n10.0.0.2 GET\n10.0.0.1 POST\n".lines() {
            counts.add(line, &config);
        }
        assert_eq!(counts.ranked(), [("10.0.0.1", 2), ("10.0.0.2", 1)]);
    }

    #[test]
    fn rejects_bad_options() {
        assert!(Config::new(&args(&["-u", "parsecs"])).is_err());
        assert!(Config::new(&args(&["-f", "0"])).is_err());
        assert!(Config::new(&args(&["-e", "("])).is_err());
        assert!(Args::try_parse_from(["stats", "--count", "-u", "ms"]).is_err());
    }
}
