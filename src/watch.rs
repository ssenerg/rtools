//! Reruns a command every few seconds and highlights what changed.

use chrono::Local;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue};
use regex_lite::Regex;
use std::io::{self, IsTerminal, Read, Write};
use std::process::{Child, Command, Stdio, exit};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools watch kubectl get pods
  rtools watch -n 5 'kubectl get pods | grep api'
  rtools watch --until Running kubectl get pod api-0
  rtools watch -g -n 10 curl -s https://example.com/status

Press q or Ctrl-C to quit, and space to run it again right away. A single quoted
argument runs through the shell (sh, or cmd on Windows), so it can use pipes.
When the output isn't a terminal, it's printed each time it changes.")]
pub struct Args {
    /// Time between runs: seconds like 2 or 0.5, or 500ms, 30s, 1m
    #[arg(short = 'n', long, value_name = "TIME", default_value = "2", value_parser = parse_interval)]
    interval: Duration,

    /// Stop once the output changes
    #[arg(short = 'g', long)]
    until_changed: bool,

    /// Stop once the output matches this regex
    #[arg(short, long, value_name = "REGEX")]
    until: Option<String>,

    /// Don't highlight what changed
    #[arg(long)]
    no_highlight: bool,

    /// Hide the header line
    #[arg(short = 't', long)]
    no_title: bool,

    /// The command to run, with its arguments
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "COMMAND"
    )]
    command: Vec<String>,
}

fn parse_interval(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, unit) = match text.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => text.split_at(i),
        None => (text, "s"),
    };
    let number: f64 = number
        .parse()
        .map_err(|_| format!("{text:?} isn't a time like 2, 0.5, 500ms, 30s or 1m"))?;
    let seconds = match unit {
        "ms" => number / 1000.0,
        "s" => number,
        "m" => number * 60.0,
        "h" => number * 3600.0,
        _ => {
            return Err(format!(
                "{text:?} isn't a time like 2, 0.5, 500ms, 30s or 1m"
            ));
        }
    };
    if !(0.1..=86_400.0).contains(&seconds) {
        return Err("the time between runs has to be between 0.1 seconds and a day".to_string());
    }
    Ok(Duration::from_secs_f64(seconds))
}

pub fn run(args: &Args) {
    let until = args.until.as_deref().map(|pattern| {
        Regex::new(pattern).unwrap_or_else(|e| fail(&format!("bad regex {pattern:?}: {e}")))
    });
    let watcher = Watcher { args, until };
    let result = if io::stdout().is_terminal() {
        watcher.full_screen()
    } else {
        watcher.log()
    };
    match result {
        Ok(Stop::Quit) => {}
        Ok(Stop::Done { output, reason }) => {
            print!("{output}");
            eprintln!("rtools watch: stopped, {reason}");
        }
        Err(e) => fail(&e),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

enum Stop {
    /// The user quit.
    Quit,
    /// --until or --until-changed was met.
    Done { output: String, reason: String },
}

/// One run of the command.
struct Run {
    output: String,
    /// The exit code, when it isn't 0.
    failed: Option<String>,
    at: chrono::DateTime<Local>,
}

struct Watcher<'a> {
    args: &'a Args,
    until: Option<Regex>,
}

impl Watcher<'_> {
    fn header(&self) -> String {
        let seconds = self.args.interval.as_secs_f64();
        format!("Every {seconds}s: {}", self.args.command.join(" "))
    }

    /// Why to stop after this run, if it's time to.
    fn stop_reason(&self, run: &Run, previous: Option<&Run>) -> Option<String> {
        if let Some(until) = &self.until
            && until.is_match(&run.output)
        {
            return Some(format!("the output matches {}", until.as_str()));
        }
        if self.args.until_changed && previous.is_some_and(|p| p.output != run.output) {
            return Some("the output changed".to_string());
        }
        None
    }

    /// The whole terminal, redrawn after every run.
    fn full_screen(&self) -> Result<Stop, String> {
        let screen = Screen::enter().map_err(|e| format!("can't set up the terminal: {e}"))?;
        let mut previous: Option<Run> = None;
        loop {
            let started = Instant::now();
            let Some(run) = run_command(&self.args.command, true)? else {
                return Ok(Stop::Quit);
            };
            self.draw(&run, previous.as_ref())
                .map_err(|e| format!("can't draw: {e}"))?;
            if let Some(reason) = self.stop_reason(&run, previous.as_ref()) {
                drop(screen);
                return Ok(Stop::Done {
                    output: run.output,
                    reason,
                });
            }
            // Wait for the next run, handling keys and resizes meanwhile.
            loop {
                let left = self.args.interval.saturating_sub(started.elapsed());
                if left.is_zero() {
                    break;
                }
                match next_key(left)? {
                    Key::Quit => return Ok(Stop::Quit),
                    Key::Again => break,
                    Key::Redraw => self
                        .draw(&run, previous.as_ref())
                        .map_err(|e| format!("can't draw: {e}"))?,
                    Key::Nothing => {}
                }
            }
            previous = Some(run);
        }
    }

    fn draw(&self, run: &Run, previous: Option<&Run>) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let (width, height) = (width as usize, height as usize);
        let mut out = io::stdout().lock();
        queue!(out, cursor::MoveTo(0, 0), Clear(ClearType::All))?;
        let mut row = 0;
        if !self.args.no_title {
            let right = match &run.failed {
                Some(code) => format!("exit {code}  {}", run.at.format("%H:%M:%S")),
                None => run.at.format("%H:%M:%S").to_string(),
            };
            let left: String = self
                .header()
                .chars()
                .take(width.saturating_sub(right.chars().count() + 2))
                .collect();
            let gap = width.saturating_sub(left.chars().count() + right.chars().count());
            queue!(
                out,
                SetAttribute(Attribute::Bold),
                Print(&left),
                SetAttribute(Attribute::Reset),
                Print(" ".repeat(gap)),
            )?;
            if run.failed.is_some() {
                queue!(out, SetForegroundColor(Color::Red))?;
            }
            queue!(out, Print(&right), ResetColor)?;
            row = 2;
        }

        let old: Vec<&str> = match previous {
            Some(p) if !self.args.no_highlight => p.output.lines().collect(),
            _ => Vec::new(),
        };
        for (i, line) in run.output.lines().enumerate() {
            if row >= height {
                break;
            }
            queue!(out, cursor::MoveTo(0, row as u16))?;
            let visible: String = line.chars().take(width).collect();
            let before = (previous.is_some() && !self.args.no_highlight)
                .then(|| old.get(i).copied().unwrap_or(""));
            for (text, changed) in segments(&visible, before) {
                if changed {
                    queue!(
                        out,
                        SetAttribute(Attribute::Reverse),
                        Print(text),
                        SetAttribute(Attribute::NoReverse)
                    )?;
                } else {
                    queue!(out, Print(text))?;
                }
            }
            row += 1;
        }
        out.flush()
    }

    /// Output that isn't a terminal: each run's output, when it changed.
    fn log(&self) -> Result<Stop, String> {
        let mut previous: Option<Run> = None;
        loop {
            let started = Instant::now();
            let run = run_command(&self.args.command, false)?.expect("only a key press quits");
            if previous.as_ref().is_none_or(|p| p.output != run.output) {
                let status = run
                    .failed
                    .as_ref()
                    .map(|code| format!(", exit {code}"))
                    .unwrap_or_default();
                let mut out = io::stdout().lock();
                let written = writeln!(out, "── {}{status} ──", run.at.format("%Y-%m-%d %H:%M:%S"))
                    .and_then(|_| write!(out, "{}", run.output))
                    .and_then(|_| out.flush());
                if let Err(e) = written {
                    // A reader like `head` that stops early ends the watch.
                    if e.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(Stop::Quit);
                    }
                    return Err(format!("can't write: {e}"));
                }
            }
            if let Some(reason) = self.stop_reason(&run, previous.as_ref()) {
                eprintln!("rtools watch: stopped, {reason}");
                return Ok(Stop::Quit);
            }
            previous = Some(run);
            thread::sleep(self.args.interval.saturating_sub(started.elapsed()));
        }
    }
}

/// Splits `line` into runs that are the same as in the line before it and
/// runs that changed. A word with any changed character is highlighted whole,
/// so Pending → Running reads as one change. No line before: nothing changed.
fn segments(line: &str, before: Option<&str>) -> Vec<(String, bool)> {
    let chars: Vec<char> = line.chars().collect();
    let old: Vec<char> = before.map_or(Vec::new(), |b| b.chars().collect());
    let mut changed: Vec<bool> = chars
        .iter()
        .enumerate()
        .map(|(col, c)| before.is_some() && old.get(col) != Some(c))
        .collect();
    let mut start = 0;
    while start < chars.len() {
        let end = (start..chars.len())
            .find(|&i| chars[i].is_whitespace() != chars[start].is_whitespace())
            .unwrap_or(chars.len());
        let word = !chars[start].is_whitespace();
        let any = changed[start..end].iter().any(|&c| c);
        changed[start..end].fill(word && any);
        start = end;
    }
    let mut runs: Vec<(String, bool)> = Vec::new();
    for (c, changed) in chars.into_iter().zip(changed) {
        match runs.last_mut() {
            Some((text, same)) if *same == changed => text.push(c),
            _ => runs.push((c.to_string(), changed)),
        }
    }
    runs
}

/// The alternate screen in raw mode, put back however the watch ends.
struct Screen;

impl Screen {
    fn enter() -> io::Result<Screen> {
        terminal::enable_raw_mode()?;
        let screen = Screen;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        Ok(screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

enum Key {
    Quit,
    Again,
    Redraw,
    Nothing,
}

fn next_key(wait: Duration) -> Result<Key, String> {
    let error = |e: io::Error| format!("can't read the keyboard: {e}");
    if !event::poll(wait).map_err(error)? {
        return Ok(Key::Nothing);
    }
    Ok(match event::read().map_err(error)? {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) => match code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => Key::Quit,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Key::Quit,
            KeyCode::Char(' ') | KeyCode::Enter => Key::Again,
            _ => Key::Nothing,
        },
        Event::Resize(..) => Key::Redraw,
        _ => Key::Nothing,
    })
}

fn build(command: &[String]) -> Command {
    let mut cmd = match command {
        [line] => shell(line),
        [program, rest @ ..] => {
            let mut cmd = Command::new(program);
            cmd.args(rest);
            cmd
        }
        [] => unreachable!("clap requires a command"),
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

#[cfg(not(windows))]
fn shell(line: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(line);
    cmd
}

#[cfg(windows)]
fn shell(line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").raw_arg(line);
    cmd
}

/// Runs the command to the end and collects its output, or None when the
/// user quits first (only watched for on a terminal).
fn run_command(command: &[String], keys: bool) -> Result<Option<Run>, String> {
    let mut child = build(command)
        .spawn()
        .map_err(|e| format!("can't run {:?}: {e}", command[0]))?;
    let stdout = read_all(child.stdout.take());
    let stderr = read_all(child.stderr.take());
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if keys {
            if let Key::Quit = next_key(Duration::from_millis(50))? {
                stop(&mut child);
                return Ok(None);
            }
        } else {
            thread::sleep(Duration::from_millis(20));
        }
    };
    let mut output = clean(&stdout.join().unwrap_or_default());
    let errors = clean(&stderr.join().unwrap_or_default());
    if !errors.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&errors);
    }
    let failed = match status.code() {
        Some(0) => None,
        Some(code) => Some(code.to_string()),
        None => Some("by a signal".to_string()),
    };
    Ok(Some(Run {
        output,
        failed,
        at: Local::now(),
    }))
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_all(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// Output as plain text for comparing and drawing: colors and other escape
/// sequences removed, tabs expanded, and progress lines (\r) reduced to what
/// they end up showing.
fn clean(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (line, newline) = match line.strip_suffix('\n') {
            Some(line) => (line.strip_suffix('\r').unwrap_or(line), "\n"),
            None => (line, ""),
        };
        let line = line.rsplit('\r').next().unwrap_or(line);
        let mut column = 0;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\x1b' => skip_escape(&mut chars),
                '\t' => {
                    let spaces = 8 - column % 8;
                    out.push_str(&" ".repeat(spaces));
                    column += spaces;
                }
                c if c.is_control() => {}
                c => {
                    out.push(c);
                    column += 1;
                }
            }
        }
        out.push_str(newline);
    }
    out
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars>) {
    match chars.next() {
        // CSI: ESC [ parameters, ending in a letter.
        Some('[') => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        // OSC: ESC ] text, ending in BEL or ESC \.
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_intervals() {
        assert_eq!(parse_interval("2"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_interval("0.5"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_interval("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_interval("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_interval("1m"), Ok(Duration::from_secs(60)));
        assert!(parse_interval("0").is_err());
        assert!(parse_interval("2d").is_err());
        assert!(parse_interval("fast").is_err());
    }

    #[test]
    fn highlights_changed_characters() {
        let plain =
            |line: &str, before: Option<&str>| -> Vec<(String, bool)> { segments(line, before) };
        assert_eq!(plain("10:04:05", None), [("10:04:05".to_string(), false)]);
        assert_eq!(
            plain("api-7 5m", Some("api-7 4m")),
            [("api-7 ".to_string(), false), ("5m".to_string(), true)]
        );
        // Longer than before: the new part changed; a new line is all new.
        assert_eq!(
            plain("api-1 Running", Some("api-1 Pending")),
            [("api-1 ".to_string(), false), ("Running".to_string(), true)]
        );
        assert_eq!(plain("new", Some("")), [("new".to_string(), true)]);
        assert_eq!(plain("same", Some("same")), [("same".to_string(), false)]);
    }

    #[test]
    fn cleans_output_for_comparing() {
        assert_eq!(clean(b"\x1b[32mok\x1b[0m\n"), "ok\n");
        assert_eq!(clean(b"a\tb\n"), "a       b\n");
        assert_eq!(clean(b"10%\r50%\r100%\n"), "100%\n");
        assert_eq!(clean(b"line\r\n"), "line\n");
        assert_eq!(clean(b"\x1b]0;title\x07text"), "text");
        assert_eq!(clean("سلام\n".as_bytes()), "سلام\n");
    }

    #[test]
    fn runs_one_argument_through_the_shell() {
        let run = |command: &[&str]| {
            let command: Vec<String> = command.iter().map(|s| s.to_string()).collect();
            run_command(&command, false).unwrap().unwrap()
        };
        if cfg!(windows) {
            assert_eq!(run(&["echo hi"]).output.trim(), "hi");
        } else {
            assert_eq!(run(&["echo a | tr a b"]).output, "b\n");
            // Several arguments run the program directly: no shell, so `|` is text.
            assert_eq!(run(&["echo", "a", "|", "b"]).output, "a | b\n");
            let failed = run(&["echo out; echo err >&2; exit 3"]);
            assert_eq!(failed.output, "out\nerr\n");
            assert_eq!(failed.failed.as_deref(), Some("3"));
        }
    }
}
