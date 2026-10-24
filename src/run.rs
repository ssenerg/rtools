//! Runs several commands at once, like an API, a frontend and a port-forward
//! during development. Each line is labeled with its command's name, Ctrl-C
//! stops them all, and --restart starts a command again when it exits.

use crate::logs::{self, ColorWhen, ShortLines};
use crate::{interrupt, update, watch};
use chrono::Local;
use clap::Parser;
use regex_lite::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio, exit};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools run api='go run ./cmd/api' web='npm run dev'
  rtools run --restart db='kubectl port-forward svc/postgres 5432'
  rtools run api='go run .' worker='go run ./worker' -e 'error|panic'
  rtools run api='go run .' web='npm run dev' --save dev    then later: rtools run dev
  rtools run                                                runs the Procfile here

Each command is one argument, run through the shell (sh, or cmd on Windows), so
quote it. name='command' names it; otherwise it's named after its program.
JSON and logfmt lines are shown on one line like `rtools logs --short`.

Ctrl-C stops every command, along with the programs they started, and a second
Ctrl-C kills them at once. With --restart, a command that exits starts again
after 1s, waiting longer (up to 30s) while it keeps failing quickly.")]
pub struct Args {
    /// Commands to run, each as one quoted argument: name='command' or 'command'.
    /// The name of a saved setup runs that setup. With none, runs the Procfile here
    #[arg(value_name = "COMMAND")]
    commands: Vec<String>,

    /// Start a command again whenever it exits
    #[arg(short, long)]
    restart: bool,

    /// When one command exits, stop the others and exit with its code
    #[arg(short, long, conflicts_with = "restart")]
    stop_all: bool,

    /// Show only output lines that match this regex. Repeat to allow any of several
    #[arg(short = 'e', long = "match", value_name = "REGEX")]
    matches: Vec<String>,

    /// Hide output lines that match this regex. Repeat to hide several
    #[arg(short = 'x', long, value_name = "REGEX")]
    exclude: Vec<String>,

    /// Match regexes ignoring case
    #[arg(short, long)]
    ignore_case: bool,

    /// Show the time each line arrived
    #[arg(short, long)]
    timestamps: bool,

    /// Show lines as they are, without putting JSON and logfmt on one line
    #[arg(long)]
    raw: bool,

    /// Save these commands and options as NAME, to start them later with `rtools run NAME`
    #[arg(long, value_name = "NAME")]
    save: Option<String>,

    /// List the saved setups
    #[arg(long, conflicts_with_all = ["commands", "save", "delete"])]
    list: bool,

    /// Delete a saved setup
    #[arg(long, value_name = "NAME", conflicts_with_all = ["commands", "save"])]
    delete: Option<String>,

    /// When to use colors
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = ColorWhen::Auto)]
    color: ColorWhen,
}

pub fn run(args: &Args) {
    let saved_path = update::config_dir().map(|dir| dir.join("run.json"));
    if args.list {
        print!(
            "{}",
            list(&load(saved_path.as_deref()).unwrap_or_else(|e| fail(&e)))
        );
        return;
    }
    if let Some(name) = &args.delete {
        delete(saved_path.as_deref(), name).unwrap_or_else(|e| fail(&e));
        println!("Deleted {name}.");
        return;
    }

    let saved = load(saved_path.as_deref()).unwrap_or_else(|e| fail(&e));
    let setup = Setup::from_args(args, &saved, Path::new("Procfile")).unwrap_or_else(|e| fail(&e));
    // Check the regexes before saving them.
    let filter = Filter::new(&setup).unwrap_or_else(|e| fail(&e));
    if let Some(name) = &args.save {
        let mut setup = setup.clone();
        if setup.dir.is_none() {
            setup.dir = std::env::current_dir().ok();
        }
        save(saved_path.as_deref(), name, setup).unwrap_or_else(|e| fail(&e));
        eprintln!("Saved as {name}. Next time, start it with: rtools run {name}");
    }

    interrupt::catch(true);
    let options = Options {
        color: logs::use_color(args.color),
        first_delay: Duration::from_secs(1),
        grace: Duration::from_secs(5),
        max_restarts: None,
    };
    let code = Runner::new(&setup, filter, &options, io::stdout()).run();
    if code != 0 {
        exit(code);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

// --- What to run --------------------------------------------------------------

/// The commands and options of one run, as saved by --save.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Setup {
    /// Where the commands run; saved setups remember the folder they were saved in.
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<PathBuf>,
    commands: Vec<Named>,
    restart: bool,
    stop_all: bool,
    raw: bool,
    timestamps: bool,
    ignore_case: bool,
    matches: Vec<String>,
    exclude: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Named {
    name: String,
    command: String,
}

impl Setup {
    fn from_args(
        args: &Args,
        saved: &BTreeMap<String, Setup>,
        procfile: &Path,
    ) -> Result<Setup, String> {
        let mut setup = match args.commands.as_slice() {
            [] => match fs::read_to_string(procfile) {
                Ok(text) => Setup {
                    commands: parse_procfile(&text)?,
                    ..Setup::default()
                },
                Err(_) => {
                    return Err("no commands given, and there's no Procfile here. \
                         Give them like: rtools run api='go run .' web='npm run dev'"
                        .to_string());
                }
            },
            [name] if saved.contains_key(name) => saved[name].clone(),
            commands => Setup {
                commands: name_commands(commands)?,
                ..Setup::default()
            },
        };
        // Options given now add to a saved setup's.
        setup.restart |= args.restart;
        setup.stop_all |= args.stop_all;
        setup.raw |= args.raw;
        setup.timestamps |= args.timestamps;
        setup.ignore_case |= args.ignore_case;
        setup.matches.extend(args.matches.iter().cloned());
        setup.exclude.extend(args.exclude.iter().cloned());
        if setup.restart && setup.stop_all {
            return Err("--restart and --stop-all don't go together".to_string());
        }
        Ok(setup)
    }
}

/// Names each command: from name='command', or after its program.
fn name_commands(commands: &[String]) -> Result<Vec<Named>, String> {
    let mut named = Vec::new();
    for arg in commands {
        let (name, command) = match arg.split_once('=') {
            Some((name, command)) if valid_name(name) => (name.to_string(), command.trim()),
            _ => (program_name(arg), arg.trim()),
        };
        if command.is_empty() {
            return Err(format!("{name} has no command to run"));
        }
        named.push(Named {
            name,
            command: command.to_string(),
        });
    }
    unique_names(&mut named);
    Ok(named)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "_-.:".contains(c))
}

/// `go` for `go run .`, `server` for `./bin/server --port 80`.
fn program_name(command: &str) -> String {
    let program = command.split_whitespace().next().unwrap_or("");
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    if name.is_empty() {
        "cmd".to_string()
    } else {
        name.to_string()
    }
}

/// Two commands named go become go and go2.
fn unique_names(named: &mut [Named]) {
    for i in 1..named.len() {
        let base = named[i].name.clone();
        let mut n = 1;
        while named[..i].iter().any(|other| other.name == named[i].name) {
            n += 1;
            named[i].name = format!("{base}{n}");
        }
    }
}

/// A Procfile's `name: command` lines, as foreman and Heroku read them.
fn parse_procfile(text: &str) -> Result<Vec<Named>, String> {
    let mut named = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once(':') {
            Some((name, command)) if valid_name(name.trim()) && !command.trim().is_empty() => {
                named.push(Named {
                    name: name.trim().to_string(),
                    command: command.trim().to_string(),
                });
            }
            _ => {
                return Err(format!(
                    "Procfile line {}: expected name: command",
                    number + 1
                ));
            }
        }
    }
    if named.is_empty() {
        return Err("the Procfile has no commands".to_string());
    }
    unique_names(&mut named);
    Ok(named)
}

// --- Saved setups ---------------------------------------------------------------

fn load(path: Option<&Path>) -> Result<BTreeMap<String, Setup>, String> {
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("can't read the saved setups in {}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("can't read {}: {e}", path.display())),
    }
}

fn store(path: Option<&Path>, saved: &BTreeMap<String, Setup>) -> Result<(), String> {
    let path = path.ok_or("no config folder found to save in")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(saved).unwrap() + "\n";
    fs::write(path, text).map_err(|e| format!("can't write {}: {e}", path.display()))
}

fn save(path: Option<&Path>, name: &str, setup: Setup) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!(
            "{name:?} can't be a name: use letters, digits, and _ - . or :"
        ));
    }
    let mut saved = load(path)?;
    saved.insert(name.to_string(), setup);
    store(path, &saved)
}

fn delete(path: Option<&Path>, name: &str) -> Result<(), String> {
    let mut saved = load(path)?;
    if saved.remove(name).is_none() {
        return Err(format!("nothing is saved as {name}"));
    }
    store(path, &saved)
}

fn list(saved: &BTreeMap<String, Setup>) -> String {
    if saved.is_empty() {
        return "Nothing saved yet. Save commands with: rtools run NAME='COMMAND' … --save SETUP\n"
            .to_string();
    }
    let mut out = String::new();
    for (name, setup) in saved {
        let mut notes = Vec::new();
        if let Some(dir) = &setup.dir {
            notes.push(format!("in {}", dir.display()));
        }
        for (on, flag) in [
            (setup.restart, "--restart"),
            (setup.stop_all, "--stop-all"),
            (setup.timestamps, "--timestamps"),
            (setup.raw, "--raw"),
            (setup.ignore_case, "--ignore-case"),
        ] {
            if on {
                notes.push(flag.to_string());
            }
        }
        notes.extend(setup.matches.iter().map(|m| format!("-e {m:?}")));
        notes.extend(setup.exclude.iter().map(|x| format!("-x {x:?}")));
        out.push_str(name);
        if !notes.is_empty() {
            out.push_str(&format!("  ({})", notes.join(", ")));
        }
        out.push('\n');
        let width = setup
            .commands
            .iter()
            .map(|c| c.name.chars().count())
            .max()
            .unwrap_or(0);
        for named in &setup.commands {
            out.push_str(&format!("  {:<width$}  {}\n", named.name, named.command));
        }
    }
    out
}

// --- Choosing lines -------------------------------------------------------------

struct Filter {
    includes: Vec<Regex>,
    excludes: Vec<Regex>,
}

impl Filter {
    fn new(setup: &Setup) -> Result<Filter, String> {
        let compile = |patterns: &[String]| {
            patterns
                .iter()
                .map(|pattern| {
                    RegexBuilder::new(pattern)
                        .case_insensitive(setup.ignore_case)
                        .build()
                        .map_err(|e| format!("bad regex {pattern:?}: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Filter {
            includes: compile(&setup.matches)?,
            excludes: compile(&setup.exclude)?,
        })
    }

    fn keeps(&self, line: &str) -> bool {
        (self.includes.is_empty() || self.includes.iter().any(|r| r.is_match(line)))
            && !self.excludes.iter().any(|r| r.is_match(line))
    }
}

// --- Running --------------------------------------------------------------------

struct Options {
    color: bool,
    /// How long to wait before the first restart; it doubles while a command
    /// keeps failing quickly.
    first_delay: Duration,
    /// How long commands get to stop before they're killed.
    grace: Duration,
    /// Stop restarting after this many restarts, for tests.
    max_restarts: Option<u32>,
}

/// A command that ran this long had been working, so its next restart is quick again.
const STEADY: Duration = Duration::from_secs(30);
const LONGEST_DELAY: Duration = Duration::from_secs(30);

enum Event {
    Line {
        index: usize,
        text: String,
    },
    Exited {
        index: usize,
        status: io::Result<ExitStatus>,
    },
}

#[derive(Debug, PartialEq)]
enum State {
    /// To be started at this time.
    Waiting(Instant),
    Running {
        pid: u32,
        started: Instant,
    },
    Done,
}

struct Process {
    name: String,
    command: String,
    color: &'static str,
    state: State,
    /// Times it was restarted.
    restarts: u32,
    /// Exits in a row that came soon after starting.
    quick_exits: u32,
}

/// Why everything is stopping, which decides rtools' own exit code.
#[derive(Clone, Copy)]
enum Stopping {
    CtrlC,
    /// One command exited with this code and --stop-all stops the rest.
    Exited(i32),
    /// The output went away, as when `| head` has read enough.
    OutputClosed,
}

const COLORS: [&str; 6] = [
    "\x1b[1;36m",
    "\x1b[1;35m",
    "\x1b[1;33m",
    "\x1b[1;32m",
    "\x1b[1;34m",
    "\x1b[1;91m",
];
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

struct Runner<'a, W: Write> {
    setup: &'a Setup,
    filter: Filter,
    options: &'a Options,
    formatter: Option<ShortLines>,
    out: W,
    output_closed: bool,
    processes: Vec<Process>,
    width: usize,
    events: (Sender<Event>, mpsc::Receiver<Event>),
    stopping: Option<(Stopping, Instant)>,
    killed: bool,
    first_failure: Option<i32>,
}

impl<'a, W: Write> Runner<'a, W> {
    fn new(setup: &'a Setup, filter: Filter, options: &'a Options, out: W) -> Self {
        let now = Instant::now();
        let processes: Vec<Process> = setup
            .commands
            .iter()
            .enumerate()
            .map(|(i, named)| Process {
                name: named.name.clone(),
                command: named.command.clone(),
                color: COLORS[i % COLORS.len()],
                state: State::Waiting(now),
                restarts: 0,
                quick_exits: 0,
            })
            .collect();
        Runner {
            setup,
            filter,
            options,
            formatter: (!setup.raw).then(|| ShortLines::new(options.color)),
            out,
            output_closed: false,
            width: processes
                .iter()
                .map(|p| p.name.chars().count())
                .max()
                .unwrap_or(0),
            processes,
            events: mpsc::channel(),
            stopping: None,
            killed: false,
            first_failure: None,
        }
    }

    /// Runs until every command is done, returning the exit code for rtools.
    fn run(mut self) -> i32 {
        let mut presses = interrupt::presses();
        loop {
            if self.stopping.is_none() {
                self.start_due();
            }
            let now_pressed = interrupt::presses();
            if now_pressed > presses {
                presses = now_pressed;
                if self.stopping.is_none() {
                    self.notice("Stopping everything (Ctrl-C again to kill at once)");
                    self.stop(Stopping::CtrlC);
                } else {
                    self.kill_all();
                }
            }
            if let Some((_, deadline)) = self.stopping
                && Instant::now() >= deadline
                && !self.killed
            {
                self.notice("Killing what's still running");
                self.kill_all();
            }
            if self.processes.iter().all(|p| p.state == State::Done) {
                break;
            }
            match self.events.1.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Line { index, text }) => self.output(index, &text),
                Ok(Event::Exited { index, status }) => self.exited(index, status),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
            }
        }
        // Lines that came in after the last exit.
        while let Ok(Event::Line { index, text }) = self.events.1.try_recv() {
            self.output(index, &text);
        }
        let _ = self.out.flush();
        match self.stopping {
            Some((Stopping::CtrlC, _)) => 130,
            Some((Stopping::Exited(code), _)) => code,
            Some((Stopping::OutputClosed, _)) => 0,
            None => self.first_failure.unwrap_or(0),
        }
    }

    fn start_due(&mut self) {
        let now = Instant::now();
        for index in 0..self.processes.len() {
            if matches!(self.processes[index].state, State::Waiting(at) if at <= now) {
                self.start(index);
            }
        }
    }

    fn start(&mut self, index: usize) {
        let process = &self.processes[index];
        let mut command = watch::shell(&process.command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.setup.dir {
            command.current_dir(dir);
        }
        // Output goes through a pipe, where most programs drop their colors.
        if self.options.color {
            for name in ["FORCE_COLOR", "CLICOLOR_FORCE"] {
                if std::env::var_os(name).is_none() {
                    command.env(name, "1");
                }
            }
        }
        // Its own process group, so stopping it stops what it started, like
        // the server `go run` builds.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);

        let restarts = process.restarts;
        let text = process.command.clone();
        match command.spawn() {
            Ok(child) => {
                let pid = child.id();
                self.processes[index].state = State::Running {
                    pid,
                    started: Instant::now(),
                };
                let note = match restarts {
                    0 => format!("$ {text}"),
                    n => format!("$ {text}   (restart {n})"),
                };
                self.status(index, &note);
                watch_child(index, child, self.events.0.clone());
            }
            Err(e) => {
                self.status(index, &format!("can't start {text}: {e}"));
                self.processes[index].state = State::Running {
                    pid: 0,
                    started: Instant::now(),
                };
                self.exited(index, Err(e));
            }
        }
    }

    fn exited(&mut self, index: usize, status: io::Result<ExitStatus>) {
        let State::Running { pid, started } = self.processes[index].state else {
            return;
        };
        // Programs it started and left behind would keep its port busy.
        if pid != 0 {
            kill_group(pid);
        }
        let code = match &status {
            Ok(status) => exit_code(status),
            Err(_) => 127,
        };
        let description = match &status {
            Ok(status) => describe(status),
            Err(_) => "couldn't start".to_string(),
        };
        if self.stopping.is_some() {
            self.processes[index].state = State::Done;
            self.status(index, &description);
            return;
        }

        let process = &mut self.processes[index];
        let may_restart = self
            .options
            .max_restarts
            .is_none_or(|max| process.restarts < max);
        if self.setup.restart && may_restart {
            if started.elapsed() >= STEADY {
                process.quick_exits = 0;
            }
            let delay = restart_delay(self.options.first_delay, process.quick_exits);
            process.quick_exits += 1;
            process.restarts += 1;
            process.state = State::Waiting(Instant::now() + delay);
            self.status(
                index,
                &format!("{description} · restarting in {}", seconds(delay)),
            );
            return;
        }

        process.state = State::Done;
        self.status(index, &description);
        if code != 0 && self.first_failure.is_none() {
            self.first_failure = Some(code);
        }
        if self.setup.stop_all {
            self.notice("Stopping the others");
            self.stop(Stopping::Exited(code));
        }
    }

    /// Asks every running command to stop, and kills them after the grace time.
    fn stop(&mut self, why: Stopping) {
        self.stopping = Some((why, Instant::now() + self.options.grace));
        for process in &mut self.processes {
            match process.state {
                State::Waiting(_) => process.state = State::Done,
                State::Running { pid, .. } if pid != 0 => {
                    // On Windows, Ctrl-C already reached every program in the console.
                    #[cfg(unix)]
                    signal_group(
                        pid,
                        if matches!(why, Stopping::CtrlC) {
                            libc::SIGINT
                        } else {
                            libc::SIGTERM
                        },
                    );
                    #[cfg(windows)]
                    if !matches!(why, Stopping::CtrlC) {
                        kill_tree(pid);
                    }
                }
                _ => {}
            }
        }
    }

    fn kill_all(&mut self) {
        self.killed = true;
        for process in &self.processes {
            if let State::Running { pid, .. } = process.state
                && pid != 0
            {
                #[cfg(unix)]
                signal_group(pid, libc::SIGKILL);
                #[cfg(windows)]
                kill_tree(pid);
            }
        }
    }

    // --- Output ---

    fn output(&mut self, index: usize, text: &str) {
        if !self.filter.keeps(text) {
            return;
        }
        let formatted = match &self.formatter {
            Some(formatter) => formatter.format(text),
            None => text.to_string(),
        };
        let mut lines = String::new();
        for line in formatted.split('\n') {
            lines.push_str(&self.prefix(index));
            lines.push_str(line);
            lines.push('\n');
        }
        self.write(&lines);
    }

    /// A line from rtools about a command, like that it exited.
    fn status(&mut self, index: usize, text: &str) {
        let line = format!("{}{}\n", self.prefix(index), self.paint(text, DIM));
        self.write(&line);
    }

    /// A line from rtools about everything.
    fn notice(&mut self, text: &str) {
        let line = format!("{}\n", self.paint(&format!("── {text}"), DIM));
        self.write(&line);
    }

    fn prefix(&self, index: usize) -> String {
        let process = &self.processes[index];
        let name = format!("{:<width$}", process.name, width = self.width);
        let mut prefix = format!(
            "{} {} ",
            self.paint(&name, process.color),
            self.paint("│", DIM)
        );
        if self.setup.timestamps {
            prefix.push_str(&self.paint(&Local::now().format("%H:%M:%S").to_string(), DIM));
            prefix.push(' ');
        }
        prefix
    }

    fn paint(&self, text: &str, style: &str) -> String {
        if self.options.color {
            format!("{style}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    fn write(&mut self, text: &str) {
        if self.output_closed {
            return;
        }
        if self
            .out
            .write_all(text.as_bytes())
            .and_then(|()| self.out.flush())
            .is_err()
        {
            // Nobody is reading anymore, as after `| head`: stop everything.
            self.output_closed = true;
            if self.stopping.is_none() {
                self.stop(Stopping::OutputClosed);
            }
        }
    }
}

/// Reads the child's output line by line and reports when it exits, after
/// its last lines.
fn watch_child(index: usize, mut child: Child, events: Sender<Event>) {
    let (done, readers_done) = mpsc::channel::<()>();
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ];
    for pipe in pipes.into_iter().flatten() {
        let events = events.clone();
        let done = done.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut buffer = Vec::new();
            loop {
                buffer.clear();
                match reader.read_until(b'\n', &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = clean_line(&String::from_utf8_lossy(&buffer));
                        if events.send(Event::Line { index, text }).is_err() {
                            break;
                        }
                    }
                }
            }
            drop(done);
        });
    }
    drop(done);
    thread::spawn(move || {
        let status = child.wait();
        // Programs it started may still hold its output open, so don't wait
        // for that long.
        let deadline = Instant::now() + Duration::from_millis(500);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if let Err(RecvTimeoutError::Disconnected) = readers_done.recv_timeout(left) {
                break;
            }
        }
        let _ = events.send(Event::Exited { index, status });
    });
}

/// A line without its line ending, and only the last of the updates a
/// progress bar writes over each other with \r.
fn clean_line(line: &str) -> String {
    let line = line.trim_end_matches(['\n', '\r']);
    line.rsplit('\r')
        .find(|part| !part.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 1s, 2s, 4s, … up to 30s while a command keeps failing quickly.
fn restart_delay(first: Duration, quick_exits: u32) -> Duration {
    first
        .saturating_mul(1 << quick_exits.min(16))
        .min(LONGEST_DELAY.max(first))
}

fn seconds(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs >= 1.0 && secs.fract() == 0.0 {
        format!("{secs}s")
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn exit_code(status: &ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
        return 128 + signal;
    }
    1
}

fn describe(status: &ExitStatus) -> String {
    match status.code() {
        Some(0) => "exited".to_string(),
        Some(code) => format!("exited with code {code}"),
        None => {
            #[cfg(unix)]
            if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
                return match signal {
                    libc::SIGINT | libc::SIGTERM => "stopped".to_string(),
                    libc::SIGKILL => "killed".to_string(),
                    _ => format!("ended by signal {signal}"),
                };
            }
            "stopped".to_string()
        }
    }
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: i32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill only sends a signal; a group that's gone just fails.
        unsafe {
            libc::kill(-pid, signal);
        }
    }
}

/// Kills what's left of a command's process group.
fn kill_group(pid: u32) {
    #[cfg(unix)]
    signal_group(pid, libc::SIGKILL);
    #[cfg(windows)]
    let _ = pid;
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        Args::parse_from(std::iter::once("run").chain(list.iter().copied()))
    }

    fn names(named: &[Named]) -> Vec<(&str, &str)> {
        named
            .iter()
            .map(|n| (n.name.as_str(), n.command.as_str()))
            .collect()
    }

    #[test]
    fn names_commands() {
        let commands: Vec<String> = [
            "api=go run ./cmd/api",
            "go run ./worker",
            "go test ./...",
            "./bin/server --port=80",
            "npm run dev -- --port=3000",
            "C:\\tools\\agent.exe",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            names(&name_commands(&commands).unwrap()),
            [
                ("api", "go run ./cmd/api"),
                ("go", "go run ./worker"),
                ("go2", "go test ./..."),
                ("server", "./bin/server --port=80"),
                ("npm", "npm run dev -- --port=3000"),
                ("agent", "C:\\tools\\agent.exe"),
            ]
        );
        assert!(name_commands(&["api=".to_string()]).is_err());
    }

    #[test]
    fn reads_procfiles() {
        let text = "# dev\nweb: npm start\n\nworker:  go run ./worker  \n";
        assert_eq!(
            names(&parse_procfile(text).unwrap()),
            [("web", "npm start"), ("worker", "go run ./worker")]
        );
        assert!(parse_procfile("just a command\n").is_err());
        assert!(parse_procfile("# nothing\n").is_err());
    }

    #[test]
    fn builds_setups_from_arguments_saved_ones_and_procfiles() {
        let dir = tempfile::tempdir().unwrap();
        let procfile = dir.path().join("Procfile");
        let mut saved = BTreeMap::new();
        saved.insert(
            "dev".to_string(),
            Setup {
                commands: vec![Named {
                    name: "a".into(),
                    command: "echo a".into(),
                }],
                matches: vec!["x".into()],
                ..Setup::default()
            },
        );

        let setup = Setup::from_args(&args(&["dev", "-r", "-e", "y"]), &saved, &procfile).unwrap();
        assert_eq!(names(&setup.commands), [("a", "echo a")]);
        assert!(setup.restart);
        assert_eq!(setup.matches, ["x", "y"]);

        let setup = Setup::from_args(&args(&["echo hi"]), &saved, &procfile).unwrap();
        assert_eq!(names(&setup.commands), [("echo", "echo hi")]);

        assert!(
            Setup::from_args(&args(&[]), &saved, &procfile)
                .unwrap_err()
                .contains("Procfile")
        );
        fs::write(&procfile, "web: npm start\n").unwrap();
        let setup = Setup::from_args(&args(&[]), &saved, &procfile).unwrap();
        assert_eq!(names(&setup.commands), [("web", "npm start")]);

        assert!(Args::try_parse_from(["run", "a", "-r", "-s"]).is_err());
    }

    #[test]
    fn saves_lists_and_deletes_setups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rtools").join("run.json");
        let path = Some(path.as_path());
        assert!(load(path).unwrap().is_empty());
        assert!(list(&load(path).unwrap()).starts_with("Nothing saved yet"));

        let setup = Setup {
            dir: Some(PathBuf::from("/code/shop")),
            commands: vec![
                Named {
                    name: "api".into(),
                    command: "go run .".into(),
                },
                Named {
                    name: "web".into(),
                    command: "npm run dev".into(),
                },
            ],
            restart: true,
            matches: vec!["error".into()],
            ..Setup::default()
        };
        save(path, "dev", setup.clone()).unwrap();
        assert_eq!(load(path).unwrap()["dev"], setup);
        assert_eq!(
            list(&load(path).unwrap()),
            "dev  (in /code/shop, --restart, -e \"error\")\n  api  go run .\n  web  npm run dev\n"
        );
        assert!(save(path, "my setup", setup).is_err());
        delete(path, "dev").unwrap();
        assert!(load(path).unwrap().is_empty());
        assert!(delete(path, "dev").is_err());
    }

    #[test]
    fn cleans_lines() {
        assert_eq!(clean_line("hello\r\n"), "hello");
        assert_eq!(clean_line("10%\r50%\r100%\n"), "100%");
        assert_eq!(clean_line("done\r\r\n"), "done");
        assert_eq!(clean_line("\n"), "");
    }

    #[test]
    fn waits_longer_between_quick_restarts() {
        let second = Duration::from_secs(1);
        let delays: Vec<u64> = (0..7).map(|n| restart_delay(second, n).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(restart_delay(second, 1000).as_secs(), 30);
        assert_eq!(seconds(Duration::from_secs(4)), "4s");
        assert_eq!(seconds(Duration::from_millis(20)), "20ms");
    }

    #[cfg(unix)]
    fn run_setup(setup: Setup, max_restarts: Option<u32>) -> (i32, String, Duration) {
        let options = Options {
            color: false,
            first_delay: Duration::from_millis(10),
            grace: Duration::from_millis(500),
            max_restarts,
        };
        let filter = Filter::new(&setup).unwrap();
        let mut out = Vec::new();
        let started = Instant::now();
        let code = Runner::new(&setup, filter, &options, &mut out).run();
        (code, String::from_utf8(out).unwrap(), started.elapsed())
    }

    #[cfg(unix)]
    fn setup(commands: &[&str]) -> Setup {
        let commands: Vec<String> = commands.iter().map(|c| c.to_string()).collect();
        Setup {
            commands: name_commands(&commands).unwrap(),
            ..Setup::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn runs_commands_side_by_side() {
        let (code, out, _) = run_setup(
            setup(&[
                "one=echo first; echo second",
                "two=printf 'no newline'",
                "three=exit 3",
            ]),
            None,
        );
        assert_eq!(code, 3);
        let mut lines: Vec<&str> = out.lines().collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "one   │ $ echo first; echo second",
                "one   │ exited",
                "one   │ first",
                "one   │ second",
                "three │ $ exit 3",
                "three │ exited with code 3",
                "two   │ $ printf 'no newline'",
                "two   │ exited",
                "two   │ no newline",
            ]
        );
        // A command's own lines stay in order.
        let first = out.find("one   │ first").unwrap();
        assert!(first < out.find("one   │ second").unwrap());
        assert!(out.find("one   │ second").unwrap() < out.find("one   │ exited").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn filters_and_reformats_lines() {
        let mut setup =
            setup(&[r#"api=echo '{"level":"error","msg":"boom"}'; echo plain; echo skip me"#]);
        setup.exclude = vec!["skip".to_string()];
        let (_, out, _) = run_setup(setup, None);
        assert!(out.contains("api │ ERROR boom\n"), "{out}");
        assert!(out.contains("api │ plain\n"));
        assert!(!out.contains("│ skip me\n"), "{out}");

        let mut raw = self::setup(&[r#"api=echo '{"msg":"hi"}'"#]);
        raw.raw = true;
        let (_, out, _) = run_setup(raw, None);
        assert!(out.contains("api │ {\"msg\":\"hi\"}\n"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn restarts_commands_that_exit() {
        let mut setup = setup(&["flaky=echo try; exit 1"]);
        setup.restart = true;
        let (code, out, _) = run_setup(setup, Some(2));
        assert_eq!(code, 1);
        assert_eq!(out.matches("flaky │ try\n").count(), 3, "{out}");
        assert!(out.contains("flaky │ exited with code 1 · restarting in 10ms\n"));
        assert!(out.contains("flaky │ exited with code 1 · restarting in 20ms\n"));
        assert!(out.contains("flaky │ $ echo try; exit 1   (restart 2)\n"));
    }

    #[cfg(unix)]
    #[test]
    fn stops_the_others_when_one_exits() {
        let mut setup = setup(&["slow=sleep 30", "quick=sleep 0.2; exit 2"]);
        setup.stop_all = true;
        let (code, out, elapsed) = run_setup(setup, None);
        assert_eq!(code, 2);
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
        assert!(out.contains("── Stopping the others\n"), "{out}");
        assert!(out.contains("slow  │ stopped\n"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn stops_what_commands_started() {
        // sh starts sleep; stopping sh's group stops sleep too.
        let mut setup = setup(&["parent=sleep 30 & wait", "quick=exit 0"]);
        setup.stop_all = true;
        let (_, _, elapsed) = run_setup(setup, None);
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn reports_commands_that_cant_start() {
        let mut setup = setup(&["nowhere=true"]);
        setup.dir = Some(PathBuf::from("/no/such/folder"));
        let (code, out, _) = run_setup(setup, None);
        assert_eq!(code, 127);
        assert!(out.contains("nowhere │ can't start true:"), "{out}");
        assert!(out.contains("nowhere │ couldn't start\n"), "{out}");
    }
}
