//! Makes logs readable as they stream in: JSON and logfmt lines are
//! reformatted, other lines pass through, and regexes pick what to show.

use crate::zones::{self, Zone};
use chrono::{DateTime, NaiveDate, Utc};
use clap::{Parser, ValueEnum};
use regex_lite::{Regex, RegexBuilder};
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, IsTerminal, Read, Write};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  kubectl logs app -f | rtools logs
  kubectl logs app -f | rtools logs -e 'timeout|refused' -x healthz
  kubectl logs app --since 1h | rtools logs --level warn --short
  docker compose logs -f api | rtools logs -e panic -A 20

JSON lines are pretty-printed like jq, also after a prefix like the ones kubectl's
--timestamps and --prefix add. logfmt lines (level=info msg=\"...\") are read too.
Anything else, like panics and stack traces, passes through unchanged.

Regexes match anywhere in the original line. -i ignores case for English letters.")]
pub struct Args {
    /// Log file. If omitted, reads from stdin (pipe)
    file: Option<String>,

    /// Show only lines that match this regex. Repeat to allow any of several
    #[arg(short = 'e', long = "match", value_name = "REGEX")]
    matches: Vec<String>,

    /// Hide lines that match this regex. Repeat to hide several
    #[arg(short = 'x', long, value_name = "REGEX")]
    exclude: Vec<String>,

    /// Match regexes ignoring case
    #[arg(short, long)]
    ignore_case: bool,

    /// Hide JSON and logfmt entries below this level
    #[arg(short, long, value_enum)]
    level: Option<Level>,

    /// One line per entry: time, level, message, then the other fields
    #[arg(short, long)]
    short: bool,

    /// Also show N lines after each match
    #[arg(short = 'A', long, value_name = "N", default_value_t = 0)]
    after: usize,

    /// Also show N lines before each match
    #[arg(short = 'B', long, value_name = "N", default_value_t = 0)]
    before: usize,

    /// Also show N lines before and after each match
    #[arg(short = 'C', long, value_name = "N")]
    context: Option<usize>,

    /// When to use colors
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = ColorWhen::Auto)]
    color: ColorWhen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Level {
    Trace,
    Debug,
    Info,
    #[value(alias = "warning")]
    Warn,
    Error,
    Fatal,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

pub fn run(args: &Args) {
    match stream(args) {
        Ok(()) => {}
        // A reader like `head` that stops early isn't an error.
        Err(Failure::Io(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
        Err(Failure::Io(e)) => fail(&e.to_string()),
        Err(Failure::Usage(message)) => fail(&message),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

enum Failure {
    Usage(String),
    Io(io::Error),
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Failure::Io(e)
    }
}

fn stream(args: &Args) -> Result<(), Failure> {
    let filter = Filter::new(args).map_err(Failure::Usage)?;
    let source: Box<dyn Read> = match &args.file {
        Some(path) => Box::new(
            File::open(path).map_err(|e| Failure::Usage(format!("can't open {path}: {e}")))?,
        ),
        None if io::stdin().is_terminal() => {
            return Err(Failure::Usage(
                "no logs given: pipe them in, like kubectl logs app -f | rtools logs".to_string(),
            ));
        }
        None => Box::new(io::stdin()),
    };
    let mut input = BufReader::with_capacity(64 * 1024, source);
    let mut out = BufWriter::with_capacity(64 * 1024, io::stdout().lock());

    let zone = Zone::system();
    let printer = Printer {
        color: use_color(args.color),
        short: args.short,
        highlight: &filter.includes,
        today: zones::wall_time(&zone, Utc::now()).date(),
        zone,
    };
    let before = args.context.unwrap_or(args.before);
    let after = args.context.unwrap_or(args.after);
    let mut context = Context::new(before, after);

    let mut raw = Vec::new();
    let mut text = String::new();
    for number in 0.. {
        raw.clear();
        if input.read_until(b'\n', &mut raw)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches(['\n', '\r']);
        let entry = Entry::parse(line);
        let keep = filter.keeps(line, &entry);
        for (number, line) in context.step(number, line, keep) {
            text.clear();
            match number {
                Some(_) => printer.entry(&mut text, &Entry::parse(&line)),
                None => printer.paint(&mut text, "--", DIM),
            }
            text.push('\n');
            out.write_all(text.as_bytes())?;
        }
        // Show lines as they arrive, but don't flush line by line through a big file.
        if input.buffer().is_empty() {
            out.flush()?;
        }
    }
    out.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Choosing lines

struct Filter {
    includes: Vec<Regex>,
    excludes: Vec<Regex>,
    level: Option<Level>,
}

impl Filter {
    fn new(args: &Args) -> Result<Filter, String> {
        let compile = |patterns: &[String]| {
            patterns
                .iter()
                .map(|pattern| {
                    RegexBuilder::new(pattern)
                        .case_insensitive(args.ignore_case)
                        .build()
                        .map_err(|e| format!("bad regex {pattern:?}: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Filter {
            includes: compile(&args.matches)?,
            excludes: compile(&args.exclude)?,
            level: args.level,
        })
    }

    /// Lines that aren't JSON or logfmt, like stack traces, have no level and
    /// pass --level.
    fn keeps(&self, line: &str, entry: &Entry) -> bool {
        if let (Some(min), Some(record)) = (self.level, &entry.record)
            && let Some(level) = level_of(record)
            && level < min
        {
            return false;
        }
        (self.includes.is_empty() || self.includes.iter().any(|r| r.is_match(line)))
            && !self.excludes.iter().any(|r| r.is_match(line))
    }
}

/// grep-style context: lines around each kept line, and `--` where lines were skipped.
struct Context {
    before: usize,
    after: usize,
    /// Recent lines that weren't shown, in case a match follows.
    held: VecDeque<(usize, String)>,
    /// Lines still to show after the last match.
    after_left: usize,
    last_shown: Option<usize>,
}

impl Context {
    fn new(before: usize, after: usize) -> Context {
        Context {
            before,
            after,
            held: VecDeque::new(),
            after_left: 0,
            last_shown: None,
        }
    }

    /// The lines to print now: numbered lines, and None for a `--` separator.
    fn step(&mut self, number: usize, line: &str, keep: bool) -> Vec<(Option<usize>, String)> {
        let mut shown = Vec::new();
        if keep || self.after_left > 0 {
            if keep {
                shown.extend(self.held.drain(..).map(|(n, l)| (Some(n), l)));
                self.after_left = self.after;
            } else {
                self.after_left -= 1;
            }
            shown.push((Some(number), line.to_string()));
        } else if self.before > 0 {
            self.held.push_back((number, line.to_string()));
            if self.held.len() > self.before {
                self.held.pop_front();
            }
        }
        let using_context = self.before > 0 || self.after > 0;
        if let (true, Some(last), Some((Some(first), _))) =
            (using_context, self.last_shown, shown.first())
            && *first > last + 1
        {
            shown.insert(0, (None, String::new()));
        }
        if let Some((Some(n), _)) = shown.last() {
            self.last_shown = Some(*n);
        }
        shown
    }
}

// ---------------------------------------------------------------------------
// Reading entries

/// A log line, with its fields when it's JSON or logfmt.
struct Entry<'a> {
    line: &'a str,
    /// Text before the JSON, like the timestamp kubectl --timestamps adds.
    prefix: &'a str,
    record: Option<Map<String, Value>>,
}

impl<'a> Entry<'a> {
    fn parse(line: &'a str) -> Entry<'a> {
        let trimmed = line.trim_end();
        if trimmed.ends_with('}')
            && let Some(start) = trimmed.find('{')
            && let Ok(Value::Object(record)) = serde_json::from_str(&trimmed[start..])
        {
            return Entry {
                line,
                prefix: &line[..start],
                record: Some(record),
            };
        }
        Entry {
            line,
            prefix: "",
            record: logfmt(trimmed),
        }
    }
}

/// `time=… level=info msg="hello world" user=42`, as Go's slog text handler
/// and logrus write it. Values stay strings.
fn logfmt(line: &str) -> Option<Map<String, Value>> {
    let mut record = Map::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let eq = rest.find(['=', ' '])?;
        let key = &rest[..eq];
        if !rest[eq..].starts_with('=') || key.is_empty() || key.contains('"') {
            return None;
        }
        rest = &rest[eq + 1..];
        let value = if rest.starts_with('"') {
            let end = closing_quote(rest)?;
            let quoted = &rest[..=end];
            rest = &rest[end + 1..];
            serde_json::from_str(quoted).unwrap_or_else(|_| quoted[1..end].to_string())
        } else {
            let end = rest.find(' ').unwrap_or(rest.len());
            let value = rest[..end].to_string();
            rest = &rest[end..];
            value
        };
        if !rest.is_empty() && !rest.starts_with(' ') {
            return None;
        }
        rest = rest.trim_start();
        record.insert(key.to_string(), Value::String(value));
    }
    let known = ["level", "lvl", "msg", "message", "time", "ts"];
    (record.len() >= 2 && record.keys().any(|k| known.contains(&k.as_str()))).then_some(record)
}

/// Where the string that starts `text` with a quote ends.
fn closing_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text.char_indices().skip(1) {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(i),
            _ => escaped = false,
        }
    }
    None
}

const LEVEL_KEYS: [&str; 6] = [
    "level",
    "lvl",
    "severity",
    "levelname",
    "log.level",
    "loglevel",
];
const MESSAGE_KEYS: [&str; 5] = ["msg", "message", "@message", "event", "log"];
const TIME_KEYS: [&str; 6] = ["time", "ts", "timestamp", "@timestamp", "t", "datetime"];
const ERROR_KEYS: [&str; 5] = ["error", "err", "exception", "stacktrace", "stack"];

fn first_key<'m>(record: &'m Map<String, Value>, keys: &[&str]) -> Option<(&'m str, &'m Value)> {
    keys.iter()
        .find_map(|&key| record.get_key_value(key))
        .map(|(key, value)| (key.as_str(), value))
}

fn level_of(record: &Map<String, Value>) -> Option<Level> {
    parse_level(first_key(record, &LEVEL_KEYS)?.1)
}

/// Level names from zap, zerolog, slog, logrus, log4j, Python and GCP, and
/// the numbers pino, bunyan and syslog use.
fn parse_level(value: &Value) -> Option<Level> {
    match value {
        Value::String(name) => match name.to_ascii_lowercase().as_str() {
            "trace" | "trc" | "finest" | "finer" => Some(Level::Trace),
            "debug" | "dbg" | "fine" => Some(Level::Debug),
            "info" | "inf" | "information" | "informational" | "notice" | "default" => {
                Some(Level::Info)
            }
            "warn" | "warning" | "wrn" => Some(Level::Warn),
            "error" | "err" | "eror" | "severe" => Some(Level::Error),
            "fatal" | "ftl" | "panic" | "dpanic" | "critical" | "crit" | "alert" | "emerg"
            | "emergency" => Some(Level::Fatal),
            _ => None,
        },
        Value::Number(n) => match n.as_u64()? {
            // syslog severities
            0..=2 => Some(Level::Fatal),
            3 => Some(Level::Error),
            4 => Some(Level::Warn),
            5 | 6 => Some(Level::Info),
            7 => Some(Level::Debug),
            // pino and bunyan
            8..=19 => Some(Level::Trace),
            20..=29 => Some(Level::Debug),
            30..=39 => Some(Level::Info),
            40..=49 => Some(Level::Warn),
            50..=59 => Some(Level::Error),
            _ => Some(Level::Fatal),
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Printing

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const KEY: &str = "\x1b[1;34m";
const STRING: &str = "\x1b[32m";
const NUMBER: &str = "\x1b[36m";
const BOOL: &str = "\x1b[33m";
const NULL: &str = "\x1b[90m";
const RED: &str = "\x1b[31m";

fn level_style(level: Option<Level>) -> (&'static str, &'static str) {
    match level {
        Some(Level::Trace) => ("TRACE", "\x1b[90m"),
        Some(Level::Debug) => ("DEBUG", "\x1b[35m"),
        Some(Level::Info) => ("INFO ", "\x1b[32m"),
        Some(Level::Warn) => ("WARN ", "\x1b[33m"),
        Some(Level::Error) => ("ERROR", "\x1b[31m"),
        Some(Level::Fatal) => ("FATAL", "\x1b[1;41;97m"),
        None => ("", BOLD),
    }
}

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

struct Printer<'a> {
    color: bool,
    short: bool,
    /// Regexes whose matches are highlighted.
    highlight: &'a [Regex],
    zone: Zone,
    today: NaiveDate,
}

impl Printer<'_> {
    fn entry(&self, out: &mut String, entry: &Entry) {
        let Some(record) = &entry.record else {
            self.paint(out, entry.line, "");
            return;
        };
        // kubectl --timestamps puts the time in front: in short lines it stands
        // in for an entry without a time of its own.
        let prefix_time = DateTime::parse_from_rfc3339(entry.prefix.trim())
            .ok()
            .map(|t| t.to_utc());
        if self.short && prefix_time.is_some() {
            self.short_entry(out, record, prefix_time);
            return;
        }
        if !entry.prefix.trim().is_empty() {
            self.paint(out, entry.prefix, DIM);
        }
        if self.short {
            self.short_entry(out, record, None);
        } else {
            self.object(out, record, 0);
        }
    }

    /// Writes `text` in `color`, highlighting what the --match regexes match.
    fn paint(&self, out: &mut String, text: &str, color: &str) {
        if !self.color {
            out.push_str(text);
            return;
        }
        let mut ranges: Vec<(usize, usize)> = self
            .highlight
            .iter()
            .flat_map(|r| r.find_iter(text).map(|m| (m.start(), m.end())))
            .filter(|(start, end)| end > start)
            .collect();
        ranges.sort_unstable();
        out.push_str(color);
        let mut at = 0;
        for (start, end) in ranges {
            let start = start.max(at);
            if start >= end {
                continue;
            }
            out.push_str(&text[at..start]);
            out.push_str("\x1b[7m");
            out.push_str(&text[start..end]);
            out.push_str("\x1b[27m");
            at = end;
        }
        out.push_str(&text[at..]);
        if !color.is_empty() {
            out.push_str(RESET);
        }
    }

    /// jq-style: indented, with the level colored by how serious it is.
    fn value(&self, out: &mut String, value: &Value, depth: usize) {
        match value {
            Value::Null => self.paint(out, "null", NULL),
            Value::Bool(b) => self.paint(out, &b.to_string(), BOOL),
            Value::Number(n) => self.paint(out, &n.to_string(), NUMBER),
            Value::String(s) => self.paint(out, &Value::String(s.clone()).to_string(), STRING),
            Value::Array(items) if items.is_empty() => out.push_str("[]"),
            Value::Array(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&"  ".repeat(depth + 1));
                    self.value(out, item, depth + 1);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str(&"  ".repeat(depth));
                out.push(']');
            }
            Value::Object(map) => self.object(out, map, depth),
        }
    }

    fn object(&self, out: &mut String, map: &Map<String, Value>, depth: usize) {
        if map.is_empty() {
            out.push_str("{}");
            return;
        }
        let level_key = if depth == 0 {
            first_key(map, &LEVEL_KEYS).map(|(key, _)| key)
        } else {
            None
        };
        out.push_str("{\n");
        for (i, (key, value)) in map.iter().enumerate() {
            out.push_str(&"  ".repeat(depth + 1));
            self.paint(out, &Value::String(key.clone()).to_string(), KEY);
            out.push_str(": ");
            if Some(key.as_str()) == level_key {
                let (_, color) = level_style(parse_level(value));
                self.paint(out, &value.to_string(), color);
            } else {
                self.value(out, value, depth + 1);
            }
            out.push_str(if i + 1 < map.len() { ",\n" } else { "\n" });
        }
        out.push_str(&"  ".repeat(depth));
        out.push('}');
    }

    /// `10:04:05.123 ERROR payment failed  order=42 error="card declined"`
    fn short_entry(
        &self,
        out: &mut String,
        record: &Map<String, Value>,
        prefix_time: Option<DateTime<Utc>>,
    ) {
        let time = first_key(record, &TIME_KEYS);
        let level = first_key(record, &LEVEL_KEYS);
        let message = first_key(record, &MESSAGE_KEYS).filter(|(_, v)| !v.is_object());
        let mut parts = 0;

        let clock = match time {
            Some((_, value)) => Some(self.time(value)),
            None => prefix_time.map(|at| self.clock(at)),
        };
        if let Some(clock) = clock {
            self.paint(out, &clock, DIM);
            parts += 1;
        }
        if let Some((_, value)) = level {
            let (label, color) = level_style(parse_level(value));
            let label = if label.is_empty() {
                format!("{:<5}", plain(value).to_uppercase())
            } else {
                label.to_string()
            };
            if parts > 0 {
                out.push(' ');
            }
            self.paint(out, &label, color);
            parts += 1;
        }
        if let Some((_, value)) = message {
            if parts > 0 {
                out.push(' ');
            }
            self.paint(out, &plain(value), BOLD);
            parts += 1;
        }

        let used: Vec<&str> = [time, level, message]
            .iter()
            .flatten()
            .map(|(key, _)| *key)
            .collect();
        let mut fields = Vec::new();
        for (key, value) in record {
            if !used.contains(&key.as_str()) {
                flatten(key.clone(), value, &mut fields);
            }
        }
        let mut blocks = Vec::new();
        let mut first = true;
        for (key, value) in &fields {
            if let Value::String(s) = value
                && s.contains('\n')
            {
                blocks.push((key, s));
                continue;
            }
            // Two spaces set the fields apart from the message.
            out.push_str(match (first, parts > 0) {
                (true, true) => "  ",
                (true, false) => "",
                (false, _) => " ",
            });
            first = false;
            self.paint(out, key, DIM);
            out.push('=');
            let color = if ERROR_KEYS.contains(&key.as_str()) {
                RED
            } else {
                ""
            };
            self.paint(out, &logfmt_value(value), color);
        }
        // Multi-line values like stack traces, below the line.
        for (key, text) in blocks {
            out.push_str("\n  ");
            self.paint(out, &format!("{key}:"), DIM);
            for line in text.lines() {
                out.push_str("\n    ");
                self.paint(
                    out,
                    line,
                    if ERROR_KEYS.contains(&key.as_str()) {
                        RED
                    } else {
                        ""
                    },
                );
            }
        }
    }

    /// A timestamp in this computer's time: the time of day if it's today.
    fn time(&self, value: &Value) -> String {
        let at = match value {
            Value::String(s) => DateTime::parse_from_rfc3339(s).ok().map(|t| t.to_utc()),
            Value::Number(n) => n.as_f64().and_then(from_epoch),
            _ => None,
        };
        match at {
            Some(at) => self.clock(at),
            None => plain(value),
        }
    }

    fn clock(&self, at: DateTime<Utc>) -> String {
        let wall = zones::wall_time(&self.zone, at);
        if wall.date() == self.today {
            wall.format("%H:%M:%S%.3f").to_string()
        } else {
            wall.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
        }
    }
}

/// Seconds, milliseconds, microseconds or nanoseconds since 1970, by size.
fn from_epoch(value: f64) -> Option<DateTime<Utc>> {
    let seconds = match value.abs() {
        v if v >= 1e17 => value / 1e9,
        v if v >= 1e14 => value / 1e6,
        v if v >= 1e11 => value / 1e3,
        _ => value,
    };
    let whole = seconds.floor();
    DateTime::from_timestamp(whole as i64, ((seconds - whole) * 1e9) as u32)
}

/// Strings without quotes, anything else as JSON.
fn plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A value for `key=value`: quoted only when it has spaces or quotes.
fn logfmt_value(value: &Value) -> String {
    match value {
        Value::String(s) if !s.is_empty() && !s.contains([' ', '"', '=', '\t']) => s.clone(),
        other => other.to_string(),
    }
}

/// Nested objects become dotted keys: {"user": {"id": 1}} is user.id=1.
fn flatten(key: String, value: &Value, fields: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                flatten(format!("{key}.{k}"), v, fields);
            }
        }
        other => fields.push((key, other.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer<'a>(highlight: &'a [Regex], short: bool, color: bool) -> Printer<'a> {
        Printer {
            color,
            short,
            highlight,
            zone: Zone::UTC,
            today: NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
        }
    }

    fn show(line: &str, short: bool) -> String {
        let mut out = String::new();
        printer(&[], short, false).entry(&mut out, &Entry::parse(line));
        out
    }

    fn filter(
        matches: &[&str],
        exclude: &[&str],
        level: Option<Level>,
        ignore_case: bool,
    ) -> Filter {
        let args = Args::parse_from(
            ["logs"]
                .into_iter()
                .chain(matches.iter().flat_map(|m| ["-e", m]))
                .chain(exclude.iter().flat_map(|x| ["-x", x]))
                .chain(ignore_case.then_some("-i")),
        );
        Filter {
            level,
            ..Filter::new(&args).unwrap()
        }
    }

    #[test]
    fn reads_json_logfmt_and_plain_lines() {
        let entry = Entry::parse(r#"{"level":"info","msg":"hi"}"#);
        assert_eq!(entry.record.unwrap()["msg"], "hi");

        // kubectl --timestamps and --prefix put text before the JSON.
        let entry = Entry::parse(r#"2026-09-27T10:00:06Z {"msg":"hi"}"#);
        assert_eq!(entry.prefix, "2026-09-27T10:00:06Z ");
        assert!(entry.record.is_some());
        let entry = Entry::parse(r#"[pod/api/app] {"msg":"hi"}"#);
        assert_eq!(entry.prefix, "[pod/api/app] ");

        let entry = Entry::parse(
            r#"time=2026-09-27T10:00:03Z level=ERROR msg="payment failed" order=9912 note="say \"hi\"""#,
        );
        let record = entry.record.unwrap();
        assert_eq!(record["msg"], "payment failed");
        assert_eq!(record["order"], "9912");
        assert_eq!(record["note"], "say \"hi\"");

        for plain in [
            "Starting server on :8080",
            "got {x} from the queue",
            "panic: runtime error: invalid memory address",
            "a = b and c=d",
            "retries=3",
            r#"{"unterminated": "#,
            "[1, 2, 3]",
        ] {
            assert!(Entry::parse(plain).record.is_none(), "{plain}");
        }
    }

    #[test]
    fn knows_levels_from_many_loggers() {
        let level = |v: Value| parse_level(&v);
        assert_eq!(level("info".into()), Some(Level::Info));
        assert_eq!(level("WARNING".into()), Some(Level::Warn));
        assert_eq!(level("ERR".into()), Some(Level::Error));
        assert_eq!(level("dpanic".into()), Some(Level::Fatal));
        assert_eq!(level("CRITICAL".into()), Some(Level::Fatal));
        assert_eq!(level("DEFAULT".into()), Some(Level::Info));
        assert_eq!(level(30.into()), Some(Level::Info));
        assert_eq!(level(50.into()), Some(Level::Error));
        assert_eq!(level(3.into()), Some(Level::Error));
        assert_eq!(level("verbose-ish".into()), None);
        let record: Map<String, Value> =
            serde_json::from_str(r#"{"severity":"WARNING","message":"x"}"#).unwrap();
        assert_eq!(level_of(&record), Some(Level::Warn));
    }

    #[test]
    fn filters_lines() {
        let error = Entry::parse(r#"{"level":"error","msg":"payment failed"}"#);
        let info = Entry::parse(r#"{"level":"info","msg":"healthz ok"}"#);
        let plain = Entry::parse("panic: boom");

        let f = filter(&["failed", "panic"], &[], None, false);
        assert!(f.keeps(error.line, &error));
        assert!(!f.keeps(info.line, &info));
        assert!(f.keeps(plain.line, &plain));

        let f = filter(&[], &["healthz"], None, false);
        assert!(!f.keeps(info.line, &info));
        assert!(f.keeps(error.line, &error));

        assert!(!filter(&["FAILED"], &[], None, false).keeps(error.line, &error));
        assert!(filter(&["FAILED"], &[], None, true).keeps(error.line, &error));

        // --level drops entries below it; lines without a level stay.
        let f = filter(&[], &[], Some(Level::Warn), false);
        assert!(f.keeps(error.line, &error));
        assert!(!f.keeps(info.line, &info));
        assert!(f.keeps(plain.line, &plain));

        let args = Args::parse_from(["logs", "-e", "("]);
        assert!(
            Filter::new(&args)
                .err()
                .unwrap()
                .starts_with("bad regex \"(\"")
        );
    }

    #[test]
    fn shows_context_like_grep() {
        let lines = ["a", "b", "MATCH", "c", "d", "e", "MATCH", "f"];
        let mut context = Context::new(1, 1);
        let mut shown = Vec::new();
        for (n, line) in lines.iter().enumerate() {
            for (number, text) in context.step(n, line, line.starts_with("MATCH")) {
                shown.push(if number.is_some() {
                    text
                } else {
                    "--".to_string()
                });
            }
        }
        assert_eq!(shown, ["b", "MATCH", "c", "--", "e", "MATCH", "f"]);

        // No context: just the matches, without separators.
        let mut context = Context::new(0, 0);
        let shown: Vec<String> = lines
            .iter()
            .enumerate()
            .flat_map(|(n, line)| context.step(n, line, line.starts_with("MATCH")))
            .map(|(_, text)| text)
            .collect();
        assert_eq!(shown, ["MATCH", "MATCH"]);
    }

    #[test]
    fn pretty_prints_like_jq() {
        assert_eq!(
            show(
                r#"{"level":"info","msg":"hi","user":{"id":1,"tags":["a"]},"empty":{},"list":[]}"#,
                false
            ),
            "{\n  \"level\": \"info\",\n  \"msg\": \"hi\",\n  \"user\": {\n    \"id\": 1,\n    \"tags\": [\n      \"a\"\n    ]\n  },\n  \"empty\": {},\n  \"list\": []\n}"
        );
        assert_eq!(show("plain text", false), "plain text");
        assert_eq!(
            show(r#"[pod/x] {"msg":"hi"}"#, false),
            "[pod/x] {\n  \"msg\": \"hi\"\n}"
        );
    }

    #[test]
    fn prints_one_line_per_entry() {
        assert_eq!(
            show(
                r#"{"level":"info","ts":1790503200.5,"msg":"server started","port":8080,"user":{"id":42,"name":"Sara Ahmadi"}}"#,
                true
            ),
            "10:00:00.500 INFO  server started  port=8080 user.id=42 user.name=\"Sara Ahmadi\""
        );
        // Another day shows the date; logfmt reads the same way.
        assert_eq!(
            show(
                r#"time=2026-09-20T10:00:03Z level=ERROR msg="payment failed" retry=false"#,
                true
            ),
            "2026-09-20 10:00:03.000 ERROR payment failed  retry=false"
        );
        // Multi-line values go below the line.
        assert_eq!(
            show(
                r#"{"level":"error","msg":"boom","stacktrace":"main.go:1\nmain.go:2"}"#,
                true
            ),
            "ERROR boom\n  stacktrace:\n    main.go:1\n    main.go:2"
        );
        // A kubectl timestamp stands in for a missing time.
        assert_eq!(
            show(r#"2026-09-27T10:00:06Z {"level":"warn","msg":"hi"}"#, true),
            "10:00:06.000 WARN  hi"
        );
        assert_eq!(show(r#"{"a":1,"b":"x"}"#, true), "a=1 b=x");
        assert_eq!(
            show(r#"{"level":"notice-ish","msg":"x"}"#, true),
            "NOTICE-ISH x"
        );
    }

    #[test]
    fn reads_epoch_times_of_any_precision() {
        let expected = DateTime::from_timestamp(1_790_503_200, 0).unwrap();
        for value in [
            1_790_503_200.0,
            1_790_503_200_000.0,
            1_790_503_200_000_000.0,
            1.7905032e18,
        ] {
            assert_eq!(from_epoch(value), Some(expected), "{value}");
        }
    }

    #[test]
    fn highlights_matches_in_color() {
        let highlight = [Regex::new("fail").unwrap()];
        let mut out = String::new();
        printer(&highlight, false, true).paint(&mut out, "failed to fail", STRING);
        assert_eq!(
            out,
            "\x1b[32m\x1b[7mfail\x1b[27med to \x1b[7mfail\x1b[27m\x1b[0m"
        );
        let mut out = String::new();
        printer(&highlight, false, false).paint(&mut out, "failed", STRING);
        assert_eq!(out, "failed");
    }
}
