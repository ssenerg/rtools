//! Browses a git diff file by file, with line numbers, in the full terminal.

use crate::watch::{self, Screen};
use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{cursor, execute, queue};
use std::io::{self, IsTerminal, Write};
use std::process::{Command, exit};

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools gitdiff                     uncommitted changes, against HEAD
  rtools gitdiff main                main against the working tree
  rtools gitdiff v0.10.0 v0.11.0     between two tags
  rtools gitdiff HEAD~3 HEAD -- src  only what changed under src

Keys:
  j/k        scroll          up/down p/n  previous and next file
  pgup/pgdn  scroll a page   left/right   scroll sideways
  g/G        top and bottom  q            quit

The mouse wheel scrolls, and clicking a file in the list opens it.

New files show once they're added with git add. When the output isn't a
terminal, the diff is printed as git prints it.")]
pub struct Args {
    /// What to compare: two commits, branches or tags, or FROM..TO. With one, it's compared with the working tree
    #[arg(value_name = "REV", num_args = 0..=2)]
    revs: Vec<String>,

    /// Only show changes to these files or folders
    #[arg(last = true, value_name = "PATH")]
    paths: Vec<String>,
}

pub fn run(args: &Args) {
    let patch = git_diff(args).unwrap_or_else(|e| fail(&e));
    if !io::stdout().is_terminal() {
        // A reader that stops early, like `| head`, isn't an error.
        if let Err(e) = io::stdout().write_all(&patch)
            && e.kind() != io::ErrorKind::BrokenPipe
        {
            fail(&format!("can't write: {e}"));
        }
        return;
    }
    let files = parse(&String::from_utf8_lossy(&patch));
    if files.is_empty() {
        println!("No differences.");
        return;
    }
    let viewer = Viewer {
        files,
        title: title(&args.revs),
        current: 0,
        scroll: 0,
        pan: 0,
        list_width: 0,
        list_first: 0,
    };
    if let Err(e) = viewer.show() {
        fail(&e);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

fn git_diff(args: &Args) -> Result<Vec<u8>, String> {
    let mut git = Command::new("git");
    // Settles what git's config could change about the output, like colors or a/ b/ prefixes.
    git.args([
        "-c",
        "core.quotePath=false",
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--find-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--end-of-options",
    ]);
    if args.revs.is_empty() {
        git.arg("HEAD");
    } else {
        git.args(&args.revs);
    }
    let output = git
        .arg("--")
        .args(&args.paths)
        .output()
        .map_err(|e| format!("can't run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(output.stdout)
}

/// What's being compared, for the bottom bar.
fn title(revs: &[String]) -> String {
    match revs {
        [] => "HEAD → working tree".to_string(),
        [range] if range.contains("..") => range.clone(),
        [from] => format!("{from} → working tree"),
        [from, to, ..] => format!("{from} → {to}"),
    }
}

#[derive(Debug, PartialEq)]
enum Kind {
    /// What git says about the file, like `rename from` or `Binary files differ`.
    Info,
    Hunk,
    Context,
    Added,
    Removed,
}

struct Line {
    kind: Kind,
    old: Option<u32>,
    new: Option<u32>,
    text: String,
}

impl Line {
    fn new(kind: Kind, old: Option<u32>, new: Option<u32>, text: &str) -> Line {
        // Escape sequences in a file would garble the screen, and tabs need expanding.
        let text = watch::clean(text.as_bytes());
        Line {
            kind,
            old,
            new,
            text,
        }
    }
}

struct FileDiff {
    path: String,
    /// A added, D deleted, R renamed or M modified.
    status: char,
    added: usize,
    removed: usize,
    lines: Vec<Line>,
}

/// Splits `git diff` output into files, numbering each line as it was before and after.
fn parse(patch: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old, mut new) = (0, 0);
    let mut in_hunk = false;
    for raw in patch.lines() {
        if let Some(names) = raw.strip_prefix("diff --git ") {
            files.push(FileDiff {
                path: header_path(names),
                status: 'M',
                added: 0,
                removed: 0,
                lines: Vec::new(),
            });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        // Inside a hunk every line starts with a sign, so `--- x` there is a removed line.
        if raw.starts_with("@@") {
            in_hunk = true;
            (old, new) = hunk_start(raw).unwrap_or_default();
            file.lines.push(Line::new(Kind::Hunk, None, None, raw));
        } else if !in_hunk {
            if let Some(path) = raw.strip_prefix("rename to ") {
                file.path = path.to_string();
                file.status = 'R';
            } else if raw.starts_with("new file mode") {
                file.status = 'A';
            } else if raw.starts_with("deleted file mode") {
                file.status = 'D';
            }
            // The rest repeats what the file list and line numbers show.
            if !["index ", "--- ", "+++ "]
                .iter()
                .any(|p| raw.starts_with(p))
            {
                file.lines.push(Line::new(Kind::Info, None, None, raw));
            }
        } else if let Some(text) = raw.strip_prefix('+') {
            file.added += 1;
            file.lines
                .push(Line::new(Kind::Added, None, Some(new), text));
            new += 1;
        } else if let Some(text) = raw.strip_prefix('-') {
            file.removed += 1;
            file.lines
                .push(Line::new(Kind::Removed, Some(old), None, text));
            old += 1;
        } else if raw.starts_with('\\') {
            file.lines.push(Line::new(Kind::Info, None, None, raw));
        } else {
            let text = raw.strip_prefix(' ').unwrap_or(raw);
            file.lines
                .push(Line::new(Kind::Context, Some(old), Some(new), text));
            old += 1;
            new += 1;
        }
    }
    files
}

/// The path in `diff --git a/<path> b/<path>`, whose halves are the same path
/// except for renames, which name the new path in a later `rename to` line.
fn header_path(names: &str) -> String {
    // shortcut: a path git quotes (one with ", \ or control characters) shows as the whole quoted line, unquote it if that comes up.
    let half = names.len().saturating_sub(1) / 2;
    names
        .get(..half)
        .and_then(|a| a.strip_prefix("a/"))
        .unwrap_or(names)
        .to_string()
}

/// The first old and new line numbers in a `@@ -12,5 +12,7 @@` line.
fn hunk_start(line: &str) -> Option<(u32, u32)> {
    let mut ranges = line.split(' ').skip(1);
    let start = |range: &str| range.split(',').next()?.parse().ok();
    let old = start(ranges.next()?.strip_prefix('-')?)?;
    let new = start(ranges.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

struct Viewer {
    files: Vec<FileDiff>,
    title: String,
    current: usize,
    /// The first diff line on screen.
    scroll: usize,
    /// How many columns the diff is scrolled sideways.
    pan: usize,
    /// Where the file list was last drawn, for clicks: its width and its first file.
    list_width: usize,
    list_first: usize,
}

impl Viewer {
    fn show(mut self) -> Result<(), String> {
        let setup = |e: io::Error| format!("can't set up the terminal: {e}");
        let _screen = Screen::enter().map_err(setup)?;
        let _mouse = Mouse::enable().map_err(setup)?;
        loop {
            let rows = self.draw().map_err(|e| format!("can't draw: {e}"))?;
            let event = event::read().map_err(|e| format!("can't read the keyboard: {e}"))?;
            let (code, modifiers) = match event {
                Event::Key(KeyEvent {
                    code,
                    modifiers,
                    kind: KeyEventKind::Press,
                    ..
                }) => (code, modifiers),
                Event::Mouse(mouse) => {
                    self.mouse(mouse, rows);
                    continue;
                }
                // Anything else, like a resize, just redraws.
                _ => continue,
            };
            match code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                KeyCode::Char('j') => self.scroll += 1,
                KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => self.scroll += rows,
                KeyCode::PageUp | KeyCode::Char('b') => {
                    self.scroll = self.scroll.saturating_sub(rows)
                }
                KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
                // draw() brings it back to the last screenful.
                KeyCode::End | KeyCode::Char('G') => self.scroll = usize::MAX,
                KeyCode::Right | KeyCode::Char('l') => self.pan += 8,
                KeyCode::Left | KeyCode::Char('h') => self.pan = self.pan.saturating_sub(8),
                KeyCode::Down | KeyCode::Char('n') | KeyCode::Tab => self.open(self.current + 1),
                KeyCode::Up | KeyCode::Char('p') | KeyCode::BackTab => {
                    if let Some(previous) = self.current.checked_sub(1) {
                        self.open(previous);
                    }
                }
                _ => {}
            }
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, rows: usize) {
        let row = mouse.row as usize;
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll += 3,
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::Down(MouseButton::Left)
                if (mouse.column as usize) < self.list_width && (1..=rows).contains(&row) =>
            {
                self.open(self.list_first + row - 1)
            }
            _ => {}
        }
    }

    fn open(&mut self, index: usize) {
        if index < self.files.len() {
            self.current = index;
            self.scroll = 0;
            self.pan = 0;
        }
    }

    /// Draws the screen and returns how many diff lines fit on it.
    fn draw(&mut self) -> io::Result<usize> {
        let (width, height) = terminal::size()?;
        let (width, height) = (width as usize, height as usize);
        let rows = height.saturating_sub(2);
        let side = sidebar_width(&self.files, width);
        let pane = width.saturating_sub(side);
        self.list_width = side;
        let file = &self.files[self.current];
        let digits = file
            .lines
            .iter()
            .filter_map(|l| l.old.max(l.new))
            .max()
            .unwrap_or(0)
            .to_string()
            .len();
        // Two line numbers, a space after each and the sign.
        let text_width = pane.saturating_sub(2 * digits + 3);
        let longest = file.lines.iter().map(|l| l.text.chars().count()).max();
        self.scroll = self.scroll.min(file.lines.len().saturating_sub(rows));
        self.pan = self
            .pan
            .min(longest.unwrap_or(0).saturating_sub(text_width));

        let mut out = io::stdout().lock();
        queue!(out, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        let position = format!(
            "{}  {}/{} ",
            counts(file),
            self.current + 1,
            self.files.len()
        );
        let path = format!(" {} {}", file.status, file.path);
        bar(&mut out, &path, &position, width, Attribute::Reverse)?;

        if side > 0 {
            // Keeps the open file in the middle of the list.
            let first = self
                .current
                .saturating_sub(rows / 2)
                .min(self.files.len().saturating_sub(rows));
            self.list_first = first;
            for row in 0..rows {
                let y = (row + 1) as u16;
                queue!(out, cursor::MoveTo(0, y))?;
                if let Some(f) = self.files.get(first + row) {
                    entry(&mut out, f, side - 1, first + row == self.current)?;
                }
                queue!(
                    out,
                    cursor::MoveTo((side - 1) as u16, y),
                    SetAttribute(Attribute::Dim),
                    Print('│'),
                    SetAttribute(Attribute::Reset)
                )?;
            }
        }

        let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        for (row, line) in file.lines.iter().skip(self.scroll).take(rows).enumerate() {
            queue!(out, cursor::MoveTo(side as u16, (row + 1) as u16))?;
            let whole: String = line.text.chars().take(pane).collect();
            match line.kind {
                Kind::Hunk => queue!(
                    out,
                    SetForegroundColor(Color::Cyan),
                    Print(whole),
                    ResetColor
                )?,
                Kind::Info => queue!(
                    out,
                    SetAttribute(Attribute::Dim),
                    Print(whole),
                    SetAttribute(Attribute::Reset)
                )?,
                _ => {
                    let (sign, color) = match line.kind {
                        Kind::Added => ('+', Color::Green),
                        Kind::Removed => ('-', Color::Red),
                        _ => (' ', Color::Reset),
                    };
                    let gutter = format!(
                        "{:>digits$} {:>digits$} ",
                        number(line.old),
                        number(line.new)
                    );
                    let text: String = line.text.chars().skip(self.pan).take(text_width).collect();
                    queue!(
                        out,
                        SetAttribute(Attribute::Dim),
                        Print(gutter),
                        SetAttribute(Attribute::Reset),
                        SetForegroundColor(color),
                        Print(sign),
                        Print(text),
                        ResetColor
                    )?;
                }
            }
        }

        queue!(out, cursor::MoveTo(0, height.saturating_sub(1) as u16))?;
        let title = format!(" {}", self.title);
        let mut keys = "j/k scroll  ↑/↓ file  ←/→ pan  q quit ";
        // On a narrow terminal the title matters more than the reminder.
        if title.chars().count() + keys.chars().count() + 2 > width {
            keys = "";
        }
        bar(&mut out, &title, keys, width, Attribute::Dim)?;
        out.flush()?;
        Ok(rows)
    }
}

/// Mouse reporting, so the wheel scrolls instead of arriving as ↑/↓ and files
/// can be clicked, turned off however the viewer ends.
struct Mouse;

impl Mouse {
    fn enable() -> io::Result<Mouse> {
        execute!(io::stdout(), EnableMouseCapture)?;
        Ok(Mouse)
    }
}

impl Drop for Mouse {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
}

/// `+12 -3`, or nothing for a file without changed lines, like a rename or a binary file.
fn counts(file: &FileDiff) -> String {
    if file.added + file.removed == 0 {
        return String::new();
    }
    format!("+{} -{}", file.added, file.removed)
}

/// The file list's width with its border, or 0 when the terminal is too narrow for it.
fn sidebar_width(files: &[FileDiff], width: usize) -> usize {
    if width < 60 {
        return 0;
    }
    let widest = files
        .iter()
        .map(|f| f.path.chars().count() + counts(f).len() + 3)
        .max()
        .unwrap_or(0);
    (widest + 1).min(width / 3)
}

/// One file in the list: its status, path and counts.
fn entry(out: &mut impl Write, file: &FileDiff, width: usize, open: bool) -> io::Result<()> {
    let counts = counts(file);
    // "M path +1 -2": the status, a space, the path, a space and the counts.
    let room = width.saturating_sub(counts.len() + 3);
    let path = tail(&file.path, room);
    let gap = " ".repeat(room - path.chars().count() + 1);
    // The open file is in reverse video, where colors would turn into backgrounds.
    let color = |c: Color| if open { Color::Reset } else { c };
    let status = match file.status {
        'A' => Color::Green,
        'D' => Color::Red,
        'R' => Color::Cyan,
        _ => Color::Yellow,
    };
    if open {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    queue!(
        out,
        SetForegroundColor(color(status)),
        Print(file.status),
        ResetColor,
        Print(' '),
        Print(path),
        Print(gap),
        SetForegroundColor(color(Color::Green)),
        Print(counts.split(' ').next().unwrap_or("")),
        ResetColor,
        Print(if counts.is_empty() { "" } else { " " }),
        SetForegroundColor(color(Color::Red)),
        Print(counts.split(' ').nth(1).unwrap_or("")),
        ResetColor,
        SetAttribute(Attribute::Reset)
    )
}

/// The end of `text` that fits in `width` columns, starting with … when it's
/// cut, since a path's file name is at its end.
fn tail(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let end: String = text.chars().skip(count - width + 1).collect();
    format!("…{end}")
}

/// A full-width bar with `left` at its start and `right` at its end.
fn bar(
    out: &mut impl Write,
    left: &str,
    right: &str,
    width: usize,
    style: Attribute,
) -> io::Result<()> {
    let right: String = right.chars().take(width).collect();
    let room = width - right.chars().count();
    let left: String = left.chars().take(room).collect();
    let gap = " ".repeat(room - left.chars().count());
    queue!(
        out,
        SetAttribute(style),
        Print(left),
        Print(gap),
        Print(right),
        SetAttribute(Attribute::Reset)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/app.rs b/src/app.rs
index 83db48f..bf269f4 100644
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,3 +1,3 @@ fn main() {
 one
--- not a header
+two
 three
@@ -10,2 +10,3 @@
 ten
+eleven
 twelve
\\ No newline at end of file
diff --git a/old name.txt b/new name.txt
similarity index 100%
rename from old name.txt
rename to new name.txt
diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..e69de29
Binary files /dev/null and b/logo.png differ
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index e69de29..0000000
";

    #[test]
    fn splits_a_diff_into_numbered_files() {
        let files = parse(PATCH);
        let summary: Vec<_> = files
            .iter()
            .map(|f| (f.status, f.path.as_str(), f.added, f.removed))
            .collect();
        assert_eq!(
            summary,
            [
                ('M', "src/app.rs", 2, 1),
                ('R', "new name.txt", 0, 0),
                ('A', "logo.png", 0, 0),
                ('D', "gone.txt", 0, 0),
            ]
        );

        let lines: Vec<_> = files[0]
            .lines
            .iter()
            .map(|l| (&l.kind, l.old, l.new, l.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (&Kind::Hunk, None, None, "@@ -1,3 +1,3 @@ fn main() {"),
                (&Kind::Context, Some(1), Some(1), "one"),
                (&Kind::Removed, Some(2), None, "-- not a header"),
                (&Kind::Added, None, Some(2), "two"),
                (&Kind::Context, Some(3), Some(3), "three"),
                (&Kind::Hunk, None, None, "@@ -10,2 +10,3 @@"),
                (&Kind::Context, Some(10), Some(10), "ten"),
                (&Kind::Added, None, Some(11), "eleven"),
                (&Kind::Context, Some(11), Some(12), "twelve"),
                (&Kind::Info, None, None, "\\ No newline at end of file"),
            ]
        );
        // The binary file keeps git's note; the index line is dropped.
        let notes: Vec<_> = files[2].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            notes,
            [
                "new file mode 100644",
                "Binary files /dev/null and b/logo.png differ"
            ]
        );
    }

    #[test]
    fn reads_paths_and_hunk_headers() {
        assert_eq!(header_path("a/src/main.rs b/src/main.rs"), "src/main.rs");
        assert_eq!(header_path("a/x b/y b/x b/y"), "x b/y");
        assert_eq!(hunk_start("@@ -12,5 +14,7 @@ fn run()"), Some((12, 14)));
        assert_eq!(hunk_start("@@ -1 +1 @@"), Some((1, 1)));
        assert_eq!(hunk_start("@@@ -1,2 -1,2 +1,3 @@@"), None);
    }

    #[test]
    fn cuts_paths_from_the_left() {
        assert_eq!(tail("src/main.rs", 20), "src/main.rs");
        assert_eq!(tail("src/main.rs", 8), "…main.rs");
        assert_eq!(tail("src/main.rs", 0), "");
    }
}
