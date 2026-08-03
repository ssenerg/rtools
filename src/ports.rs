use crate::utils;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, IsTerminal, Write};
use std::process::{Command, exit};
use std::thread::sleep;
use std::time::Duration;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const INVERSE: &str = "\x1b[7m";

const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

/// How long the OS needs to release a socket before a refresh shows the truth.
const RELEASE_GRACE: Duration = Duration::from_millis(250);

#[derive(Parser, Debug)]
#[command(after_help = "Interactive keys:\n  \
    up/down  move          space  select\n  \
    k        kill (TERM)   K      force-kill (KILL -9)\n  \
    enter    kill          r      refresh\n  \
    /        filter        q      quit")]
pub struct Args {
    /// Print ports as tab-separated text instead of opening the picker
    #[arg(short, long)]
    list: bool,
}

#[derive(Clone)]
struct PortRow {
    command: String,
    pid: String,
    user: String,
    port: u16,
    addrs: Vec<String>,
}

impl PortRow {
    fn key(&self) -> String {
        format!("{}:{}", self.pid, self.port)
    }
}

pub fn run(args: &Args, copy: bool) {
    let result = if args.list || !std::io::stdout().is_terminal() {
        print_list(copy)
    } else {
        run_interactive()
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        exit(1);
    }
}

fn print_list(copy: bool) -> Result<(), String> {
    let rows = read_ports()?;
    if rows.is_empty() {
        println!("No listening ports.");
        return Ok(());
    }

    let mut out = String::from("PORT\tPID\tCOMMAND\tUSER\tADDR");
    for r in &rows {
        out.push_str(&format!(
            "\n{}\t{}\t{}\t{}\t{}",
            r.port,
            r.pid,
            r.command,
            r.user,
            r.addrs.join(",")
        ));
    }
    utils::emit(&out, copy)
}

/// Read all TCP ports in LISTEN state via lsof, one row per PID+port.
fn read_ports() -> Result<Vec<PortRow>, String> {
    let output = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output()
        .map_err(|e| match e.kind() {
            // lsof ships with macOS; on Linux it may need installing.
            ErrorKind::NotFound => "lsof not found — this tool needs lsof".to_string(),
            _ => format!("failed to run lsof: {}", e),
        })?;

    // lsof exits non-zero when nothing is listening — that's not a real failure.
    let raw = String::from_utf8_lossy(&output.stdout);

    let mut ps_cache: HashMap<String, String> = HashMap::new();
    let mut by_key: HashMap<String, PortRow> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for line in raw.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 9 {
            continue;
        }

        let Some(addr) = listen_address(&cols) else {
            continue;
        };
        let Some(port) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) else {
            continue;
        };

        let pid = cols[1].to_string();
        let key = format!("{}:{}", pid, port);

        if let Some(existing) = by_key.get_mut(&key) {
            if !existing.addrs.contains(&addr) {
                existing.addrs.push(addr);
            }
            continue;
        }

        order.push(key.clone());
        by_key.insert(
            key,
            PortRow {
                command: full_command(&pid, cols[0], &mut ps_cache),
                user: cols[2].to_string(),
                pid,
                port,
                addrs: vec![addr],
            },
        );
    }

    let mut rows: Vec<PortRow> = order.iter().filter_map(|k| by_key.remove(k)).collect();
    rows.sort_by_key(|r| r.port);
    Ok(rows)
}

/// The NAME column (`*:7000`, `127.0.0.1:44950`, `[::1]:8080`) — normally column 9,
/// but its index shifts when lsof omits a column, so scan from there for an addr:port.
fn listen_address(cols: &[&str]) -> Option<String> {
    cols.iter()
        .skip(8)
        .find(|token| {
            token
                .rsplit(':')
                .next()
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|token| token.to_string())
}

/// lsof truncates command names to ~9 chars — recover the real name from ps.
fn full_command(pid: &str, fallback: &str, cache: &mut HashMap<String, String>) -> String {
    if let Some(name) = cache.get(pid) {
        return name.clone();
    }

    let name = Command::new("ps")
        .args(["-p", pid, "-o", "comm="])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|out| !out.is_empty())
        .and_then(|out| out.rsplit('/').next().map(|n| n.to_string()))
        // The process may have exited between lsof and ps — keep the lsof name.
        .unwrap_or_else(|| fallback.to_string());

    cache.insert(pid.to_string(), name.clone());
    name
}

fn kill_pid(pid: &str, force: bool) -> bool {
    Command::new("kill")
        .args([if force { "-9" } else { "-15" }, pid])
        .status()
        .is_ok_and(|status| status.success())
}

// ── Interactive TUI ──────────────────────────────────────────────────────────

struct App {
    rows: Vec<PortRow>,
    cursor: usize,
    selected: HashSet<String>,
    status: String,
    filter: String,
    filter_mode: bool,
}

fn run_interactive() -> Result<(), String> {
    let mut app = App {
        rows: read_ports()?,
        cursor: 0,
        selected: HashSet::new(),
        status: String::new(),
        filter: String::new(),
        filter_mode: false,
    };

    enable_raw_mode().map_err(|e| format!("failed to enter raw mode: {}", e))?;
    let result = event_loop(&mut app);
    let _ = disable_raw_mode();
    print!("{}", CLEAR_SCREEN);
    let _ = std::io::stdout().flush();

    result
}

fn event_loop(app: &mut App) -> Result<(), String> {
    app.render()?;

    loop {
        let Event::Key(key) = event::read().map_err(|e| format!("failed to read key: {}", e))?
        else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(());
        }

        if app.filter_mode {
            app.handle_filter_key(key.code, key.modifiers);
            app.render()?;
            continue;
        }

        app.status.clear();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            // 'k' kills, 'K' force-kills — vim j/k movement is intentionally unbound.
            KeyCode::Char('k') | KeyCode::Enter => app.kill_targets(false)?,
            KeyCode::Char('K') => app.kill_targets(true)?,
            KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
            KeyCode::Down => {
                let last = app.visible().len().saturating_sub(1);
                app.cursor = (app.cursor + 1).min(last);
            }
            KeyCode::Char(' ') => app.toggle_selected(),
            KeyCode::Char('r') => {
                app.rows = read_ports()?;
                app.status = "Refreshed".to_string();
            }
            KeyCode::Char('/') => {
                app.filter_mode = true;
                app.filter.clear();
            }
            _ => {}
        }
        app.render()?;
    }
}

impl App {
    fn visible(&self) -> Vec<&PortRow> {
        if self.filter.is_empty() {
            return self.rows.iter().collect();
        }
        let query = self.filter.to_lowercase();
        self.rows
            .iter()
            .filter(|r| {
                r.port.to_string().contains(&query)
                    || r.command.to_lowercase().contains(&query)
                    || r.pid.contains(&query)
            })
            .collect()
    }

    fn handle_filter_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Enter => self.filter_mode = false,
            KeyCode::Esc => {
                self.filter_mode = false;
                self.filter.clear();
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter.push(c)
            }
            _ => {}
        }
        self.cursor = 0;
    }

    fn toggle_selected(&mut self) {
        let Some(key) = self.visible().get(self.cursor).map(|r| r.key()) else {
            return;
        };
        if !self.selected.remove(&key) {
            self.selected.insert(key);
        }
    }

    fn kill_targets(&mut self, force: bool) -> Result<(), String> {
        // With nothing explicitly selected, act on the row under the cursor.
        let keys: Vec<String> = if self.selected.is_empty() {
            self.visible()
                .get(self.cursor)
                .map(|r| vec![r.key()])
                .unwrap_or_default()
        } else {
            self.selected.iter().cloned().collect()
        };

        let targets: Vec<PortRow> = keys
            .iter()
            .filter_map(|key| self.rows.iter().find(|r| &r.key() == key).cloned())
            .collect();
        if targets.is_empty() {
            return Ok(());
        }

        let mut killed_pids: HashSet<String> = HashSet::new();
        let mut ok = 0;
        for target in &targets {
            // One process can hold several ports — signal it once.
            if killed_pids.contains(&target.pid) {
                ok += 1;
                continue;
            }
            if kill_pid(&target.pid, force) {
                ok += 1;
                killed_pids.insert(target.pid.clone());
            }
        }

        self.status = format!(
            "{} {}/{} ({} process{})",
            if force { "Force-killed" } else { "Killed" },
            ok,
            targets.len(),
            killed_pids.len(),
            if killed_pids.len() == 1 { "" } else { "es" }
        );
        self.selected.clear();
        self.render()?;

        // Give the OS a beat to release the sockets, then refresh.
        sleep(RELEASE_GRACE);
        self.rows = read_ports()?;
        Ok(())
    }

    fn render(&mut self) -> Result<(), String> {
        let count = self.visible().len();
        if self.cursor >= count {
            self.cursor = count.saturating_sub(1);
        }

        let mut lines = vec![
            format!(
                "{}{}  PORTS{}{}  — active listening ports{}",
                BOLD, CYAN, RESET, DIM, RESET
            ),
            String::new(),
        ];

        if count == 0 {
            let message = if self.filter.is_empty() {
                "  no listening ports 🎉"
            } else {
                "  no ports match filter"
            };
            lines.push(format!("{}{}{}", DIM, message, RESET));
        } else {
            lines.push(format!(
                "{}     {:<8}{:<9}{:<22}{}{}",
                DIM, "PORT", "PID", "COMMAND", "ADDRESS", RESET
            ));
            for (i, row) in self.visible().iter().enumerate() {
                let mark = if self.selected.contains(&row.key()) {
                    format!("{}●{}", YELLOW, RESET)
                } else {
                    " ".to_string()
                };
                let command: String = row.command.chars().take(20).collect();
                let text = format!(
                    "{}  {:<8}{:<9}{:<22}{}",
                    mark,
                    row.port,
                    row.pid,
                    command,
                    row.addrs.join(", ")
                );
                if i == self.cursor {
                    lines.push(format!("{} ›{}{}", INVERSE, text, RESET));
                } else {
                    lines.push(format!("  {}", text));
                }
            }
        }

        lines.push(String::new());
        if self.filter_mode {
            lines.push(format!(
                "{}  /filter: {}▌{}{}  (enter to apply · esc to clear){}",
                YELLOW, self.filter, RESET, DIM, RESET
            ));
        } else {
            let hint = if self.selected.is_empty() {
                "↑/↓ move · space select · k kill · K force-kill".to_string()
            } else {
                format!("{} selected · k kill · K force-kill", self.selected.len())
            };
            lines.push(format!(
                "{}  {} · r refresh · / filter · q quit{}",
                DIM, hint, RESET
            ));
        }
        if !self.status.is_empty() {
            lines.push(format!("{}  {}{}", GREEN, self.status, RESET));
        }

        // Raw mode swallows the implicit carriage return, so emit it explicitly.
        let frame = format!("{}{}\r\n", CLEAR_SCREEN, lines.join("\r\n"));
        let mut stdout = std::io::stdout();
        stdout
            .write_all(frame.as_bytes())
            .and_then(|_| stdout.flush())
            .map_err(|e| format!("failed to draw: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ipv4_ipv6_and_wildcard_addresses() {
        let ipv4 = vec![
            "node",
            "123",
            "me",
            "23u",
            "IPv4",
            "0x1",
            "0t0",
            "TCP",
            "127.0.0.1:44950",
            "(LISTEN)",
        ];
        let ipv6 = vec![
            "node",
            "123",
            "me",
            "23u",
            "IPv6",
            "0x1",
            "0t0",
            "TCP",
            "[::1]:8080",
            "(LISTEN)",
        ];
        let wildcard = vec![
            "node", "123", "me", "23u", "IPv4", "0x1", "0t0", "TCP", "*:7000", "(LISTEN)",
        ];

        assert_eq!(listen_address(&ipv4).as_deref(), Some("127.0.0.1:44950"));
        assert_eq!(listen_address(&ipv6).as_deref(), Some("[::1]:8080"));
        assert_eq!(listen_address(&wildcard).as_deref(), Some("*:7000"));
    }

    #[test]
    fn ignores_rows_without_a_port() {
        let cols = vec![
            "node",
            "123",
            "me",
            "23u",
            "IPv4",
            "0x1",
            "0t0",
            "TCP",
            "no-port-here",
        ];
        assert!(listen_address(&cols).is_none());
    }
}
