use clap::{Parser, ValueEnum};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Display;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, exit};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::ResponseExt;
use ureq::tls::{RootCerts, TlsConfig};

/// The GitHub repository releases are published to.
const REPO: &str = "ssenerg/rtools";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
/// The executable inside every release archive.
const BIN_FILE: &str = if cfg!(windows) {
    concat!(env!("CARGO_PKG_NAME"), ".exe")
} else {
    env!("CARGO_PKG_NAME")
};

/// GitHub is asked about new releases, and the user told about one, at most this often (seconds).
const CHECK_INTERVAL: u64 = 24 * 60 * 60;
/// How long a background check may hold up the command it runs alongside.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);
/// Upper bound for talking to GitHub while actually updating.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Updates are staged in `.rtools-update-<random>` next to the binary.
const STAGING_PREFIX: &str = ".rtools-update-";
/// Staging older than this was left behind; newer may belong to an update running now.
const STALE_STAGING: Duration = Duration::from_secs(60 * 60);

#[derive(Parser, Debug)]
#[command(after_help = "\
rtools also looks for new releases by itself: at most once a day, and only while it
runs in a terminal. What it does when it finds one is up to --mode (default: ask).
Downloads are checked against the release's SHA256SUMS before they replace rtools.")]
pub struct Args {
    /// Only check whether a newer release is available
    #[arg(long)]
    check: bool,

    /// Install without asking for confirmation
    #[arg(short, long, conflicts_with = "check")]
    yes: bool,

    /// Set what rtools does when it notices a new release by itself
    ///
    /// Without a value, shows the current mode.
    #[arg(long, value_enum, value_name = "MODE", conflicts_with_all = ["check", "yes"])]
    mode: Option<Option<Mode>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// Ask before installing a new release (default)
    #[default]
    Ask,
    /// Install new releases without asking
    Auto,
    /// Only print a notice about new releases
    Notify,
    /// Never look for new releases in the background
    Off,
}

pub fn run(args: &Args) {
    let result = match args.mode {
        Some(mode) => set_mode(mode),
        None => update(args.check, args.yes),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        exit(1);
    }
}

fn update(check_only: bool, yes: bool) -> Result<(), String> {
    let agent = agent(UPDATE_TIMEOUT);
    let release = fetch_latest(&agent)?;
    let latest = release.version()?;
    if latest <= current_version() {
        println!("rtools {CURRENT} is up to date");
        return Ok(());
    }

    println!("A new version of rtools is available: {CURRENT} → {latest}");
    println!("Release notes: {}", release.url());
    if check_only {
        println!("Run `rtools update` to install it.");
        return Ok(());
    }
    if !yes {
        if !io::stdin().is_terminal() {
            return Err("no terminal to confirm the update in, re-run with --yes".to_string());
        }
        eprint!("Install it now? [Y/n]: ");
        if !matches!(read_answer().as_str(), "" | "y" | "yes") {
            println!("Update cancelled.");
            return Ok(());
        }
    }

    let installed = install(&agent, &release)?;
    println!("Updated rtools {CURRENT} → {installed}");
    Ok(())
}

fn set_mode(mode: Option<Mode>) -> Result<(), String> {
    let mode = match mode {
        Some(mode) => {
            update_state(|s| s.mode = mode)
                .map_err(|e| format!("failed to save the update mode: {e}"))?;
            mode
        }
        None => state_path()
            .map(|path| State::load(&path).mode)
            .unwrap_or_default(),
    };

    let value = mode.to_possible_value().expect("every mode is a CLI value");
    let help = value
        .get_help()
        .map(ToString::to_string)
        .unwrap_or_default();
    println!("Update mode: {} — {help}", value.get_name());
    Ok(())
}

/// A trimmed, lowercased line from stdin; empty on EOF.
fn read_answer() -> String {
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    line.trim().to_ascii_lowercase()
}

// ── Background check ─────────────────────────────────────────────────────────

/// Looks for a new release while a command runs. Once the command is done it
/// tells the user, asks to install it, or installs it, depending on the mode.
pub struct BackgroundCheck {
    path: PathBuf,
    pending: Option<Receiver<Result<Release, String>>>,
    deadline: Instant,
}

impl BackgroundCheck {
    /// Only starts when someone is at the terminal to see the outcome, so
    /// pipes, scripts and CI are never slowed down or interrupted.
    pub fn start() -> Option<BackgroundCheck> {
        if !io::stdout().is_terminal() || !io::stderr().is_terminal() {
            return None;
        }
        let path = state_path()?;
        let mut state = State::load(&path);
        if state.mode == Mode::Off {
            return None;
        }

        let now = now();
        let pending = if is_due(state.last_check, now) {
            // Record the attempt up front: a slow or offline network then costs one
            // delay per interval rather than one per run. No state, no check.
            state.last_check = now;
            state.save(&path).ok()?;
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(fetch_latest(&agent(CHECK_TIMEOUT)));
            });
            Some(rx)
        } else {
            None
        };

        Some(BackgroundCheck {
            path,
            pending,
            deadline: Instant::now() + CHECK_TIMEOUT,
        })
    }

    pub fn finish(self) {
        let deadline = self.deadline;
        let fresh = self.pending.and_then(|pending| {
            let wait = deadline.saturating_duration_since(Instant::now());
            pending.recv_timeout(wait).ok()?.ok()
        });

        // Reload: the mode may have been changed while the command ran.
        let mut state = State::load(&self.path);
        if let Some(version) = fresh.as_ref().and_then(|release| release.version().ok()) {
            state.latest = Some(version.to_string());
        }
        let now = now();
        let announcement = state.announcement(&current_version(), now);
        if announcement.is_some() {
            state.last_notice = now;
        }
        if fresh.is_some() || announcement.is_some() {
            let _ = state.save(&self.path);
        }
        let Some(latest) = announcement else {
            return;
        };

        eprintln!();
        eprintln!("A new version of rtools is available: {CURRENT} → {latest}");
        let tag = format!("v{latest}");
        eprintln!("Release notes: {}", Release { tag }.url());
        match state.mode {
            Mode::Auto => {
                eprintln!("Installing it (auto-update is on; `rtools update --mode ask` to stop)");
                report(install_latest(fresh));
            }
            Mode::Ask if io::stdin().is_terminal() => ask(&latest, fresh),
            _ => eprintln!("Run `rtools update` to install it."),
        }
    }
}

/// The consent prompt: nothing is installed unless the user says so.
fn ask(latest: &Version, release: Option<Release>) {
    eprint!("Update now? [y]es, [N]o, [a]lways (auto-update), [s]kip this version: ");
    match Answer::parse(&read_answer()) {
        Answer::Yes => report(install_latest(release)),
        Answer::Always => {
            match update_state(|s| s.mode = Mode::Auto) {
                Ok(()) => eprintln!("Auto-update is on, `rtools update --mode ask` turns it off."),
                Err(e) => eprintln!("Couldn't turn auto-update on: {e}"),
            }
            report(install_latest(release));
        }
        Answer::Skip => match update_state(|s| s.skipped = Some(latest.to_string())) {
            Ok(()) => eprintln!("Skipped {latest}, you'll hear about the next release."),
            Err(e) => eprintln!("Couldn't save that: {e}"),
        },
        Answer::No => eprintln!("Run `rtools update` when you're ready."),
    }
}

#[derive(Debug, PartialEq)]
enum Answer {
    Yes,
    No,
    Always,
    Skip,
}

impl Answer {
    /// Anything unrecognised — including just pressing enter — is a no.
    fn parse(answer: &str) -> Answer {
        match answer {
            "y" | "yes" => Answer::Yes,
            "a" | "always" => Answer::Always,
            "s" | "skip" => Answer::Skip,
            _ => Answer::No,
        }
    }
}

/// Install `release`, or whatever is latest if it wasn't fetched during this run.
fn install_latest(release: Option<Release>) -> Result<Version, String> {
    let agent = agent(UPDATE_TIMEOUT);
    let release = match release {
        Some(release) => release,
        None => fetch_latest(&agent)?,
    };
    install(&agent, &release)
}

fn report(result: Result<Version, String>) {
    match result {
        Ok(version) => eprintln!("Updated rtools {CURRENT} → {version}"),
        Err(e) => eprintln!("Update failed: {e}"),
    }
}

// ── Saved state ──────────────────────────────────────────────────────────────

/// Kept in `update.json` in the rtools config directory.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    mode: Mode,
    /// When GitHub was last asked for the latest release (unix seconds).
    last_check: u64,
    /// The latest release found by that check.
    latest: Option<String>,
    /// When the user was last told about a new release (unix seconds).
    last_notice: u64,
    /// A release the user chose to skip.
    skipped: Option<String>,
}

impl State {
    fn load(path: &Path) -> State {
        fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("state file has no parent directory"))?;
        fs::create_dir_all(dir)?;
        // Write-then-rename, so a concurrent run never reads a half-written file.
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.persist(path)?;
        Ok(())
    }

    /// The release to tell the user about now, if any.
    fn announcement(&self, current: &Version, now: u64) -> Option<Version> {
        if self.mode == Mode::Off || !is_due(self.last_notice, now) {
            return None;
        }
        let latest = Version::parse(self.latest.as_deref()?).ok()?;
        let skipped = self.skipped.as_deref() == Some(latest.to_string().as_str());
        (latest > *current && !skipped).then_some(latest)
    }
}

/// Load, change and save the state in one go, so changes made by other rtools
/// processes in the meantime aren't lost.
fn update_state(change: impl FnOnce(&mut State)) -> io::Result<()> {
    let path =
        state_path().ok_or_else(|| io::Error::other("no config directory found to save it in"))?;
    let mut state = State::load(&path);
    change(&mut state);
    state.save(&path)
}

/// `%APPDATA%\rtools\update.json` on Windows, `~/.config/rtools/update.json` elsewhere.
fn state_path() -> Option<PathBuf> {
    let var = |key: &str| {
        env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let config = if cfg!(windows) {
        var("APPDATA")?
    } else {
        var("XDG_CONFIG_HOME")
            .filter(|dir| dir.is_absolute())
            .or_else(|| Some(var("HOME")?.join(".config")))?
    };
    Some(config.join(env!("CARGO_PKG_NAME")).join("update.json"))
}

fn is_due(last: u64, now: u64) -> bool {
    // A clock that went backwards mustn't postpone checks until it catches up.
    now < last || now - last >= CHECK_INTERVAL
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn current_version() -> Version {
    Version::parse(CURRENT).expect("Cargo.toml version is valid semver")
}

// ── GitHub releases ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct Release {
    tag: String,
}

impl Release {
    /// The tag without its `v`: the release workflow requires it to match Cargo.toml.
    fn tag_version(&self) -> &str {
        self.tag.strip_prefix('v').unwrap_or(&self.tag)
    }

    fn version(&self) -> Result<Version, String> {
        Version::parse(self.tag_version())
            .map_err(|_| format!("release tag {} is not a version", self.tag))
    }

    fn url(&self) -> String {
        format!("https://github.com/{REPO}/releases/tag/{}", self.tag)
    }

    fn asset_url(&self, name: &str) -> String {
        format!(
            "https://github.com/{REPO}/releases/download/{}/{name}",
            self.tag
        )
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(USER_AGENT)
        // The OS certificate store, so proxies and antivirus the system trusts keep working.
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

/// The newest non-prerelease release, found through github.com's documented
/// `releases/latest` link, which redirects to the release's tag page. Unlike the
/// REST API, it has no 60-requests-an-hour limit per IP to share with everyone
/// else behind the same office or carrier NAT.
fn fetch_latest(agent: &ureq::Agent) -> Result<Release, String> {
    let response = agent
        .head(format!("https://github.com/{REPO}/releases/latest"))
        .call()
        .map_err(|e| format!("failed to reach GitHub: {e}"))?;
    // Without any release, the link lands on the release list instead.
    let tag = tag_from_url(&response.get_uri().to_string())
        .ok_or("no rtools release has been published yet")?;
    Ok(Release { tag })
}

/// `https://github.com/<owner>/<repo>/releases/tag/<tag>` → `<tag>`
fn tag_from_url(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("/releases/tag/")?;
    let tag = rest.split(['?', '#', '/']).next()?;
    (!tag.is_empty()).then(|| tag.to_string())
}

/// GitHub answers asset links with a redirect to its CDN, which ureq follows.
fn fetch_asset(
    agent: &ureq::Agent,
    release: &Release,
    name: &str,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    agent
        .get(release.asset_url(name))
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => format!("release {} has no {name}", release.tag),
            e => format!("failed to download {name}: {e}"),
        })
}

// ── Installing ───────────────────────────────────────────────────────────────

/// Download, verify and swap in `release`, returning the version installed.
fn install(agent: &ureq::Agent, release: &Release) -> Result<Version, String> {
    let version = release.version()?;
    if version <= current_version() {
        return Err(format!(
            "the latest release ({version}) is not newer than rtools {CURRENT}"
        ));
    }
    let target = release_target().ok_or_else(|| {
        format!(
            "there are no prebuilt rtools binaries for {}-{}",
            env::consts::ARCH,
            env::consts::OS
        )
    })?;
    let archive = archive_name(release.tag_version(), target);
    let sums = fetch_asset(agent, release, "SHA256SUMS")?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("failed to download SHA256SUMS: {e}"))?;
    let expected = expected_checksum(&sums, &archive)
        .ok_or_else(|| format!("SHA256SUMS lists no checksum for {archive}"))?;

    let exe = installed_exe()
        .map_err(|e| format!("cannot locate the running rtools (moved or replaced?): {e}"))?;
    let dir = exe
        .parent()
        .ok_or("cannot locate the directory rtools is installed in")?;
    // Stage next to the binary: that fails early without write access, keeps the
    // final rename on one filesystem, and unlike a temp dir it is never mounted
    // noexec, so the self-test can run.
    remove_stale_staging(dir, STALE_STAGING);
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(dir)
        .map_err(|e| write_error(dir, e))?;

    let archive_path = staging.path().join(&archive);
    let actual = download(agent, release, &archive, &archive_path)?;
    if actual != expected {
        return Err(format!(
            "checksum mismatch for {archive}; the download is damaged or was tampered with, nothing was changed"
        ));
    }

    let binary = staging.path().join(BIN_FILE);
    let file = File::open(&archive_path).map_err(unreadable_archive)?;
    extract_binary(file, &binary)?;
    check_runs(&binary)?;
    replace_exe(&exe, &binary).map_err(|e| write_error(dir, e))?;
    Ok(version)
}

/// The binary to replace. Symlinks are resolved so the file they point to is
/// updated rather than the link.
fn installed_exe() -> io::Result<PathBuf> {
    let exe = env::current_exe()?;
    if cfg!(windows) {
        Ok(exe)
    } else {
        exe.canonicalize()
    }
}

#[cfg(not(windows))]
fn replace_exe(exe: &Path, new: &Path) -> io::Result<()> {
    // rename() swaps atomically: the path always holds a complete binary, and
    // the running process keeps its own copy.
    fs::set_permissions(new, fs::metadata(exe)?.permissions())?;
    fs::rename(new, exe)
}

#[cfg(windows)]
fn replace_exe(exe: &Path, new: &Path) -> io::Result<()> {
    // A running .exe can't be overwritten or deleted, only renamed: move it
    // aside, and back again if the new one can't take its place, so rtools
    // never goes missing.
    let old = exe.with_file_name(format!(".rtools-old-{}.exe", std::process::id()));
    fs::rename(exe, &old)?;
    if let Err(e) = fs::rename(new, exe) {
        let _ = fs::rename(&old, exe);
        return Err(e);
    }
    // A helper process deletes the old binary once this one has exited.
    let _ = self_replace::self_delete_at(&old);
    Ok(())
}

/// Clear staging left behind by an interrupted update, or on Windows by a virus
/// scanner still holding the new binary.
fn remove_stale_staging(dir: &Path, older_than: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age >= older_than));
        if stale
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The release workflow's build targets (.github/workflows/release.yml). Linux
/// always gets the static musl build, so source builds can update too.
fn release_target() -> Option<&'static str> {
    Some(match (env::consts::ARCH, env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-musl",
        ("aarch64", "linux") => "aarch64-unknown-linux-musl",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        _ => return None,
    })
}

/// Mirrors the naming in the release workflow's Package step.
fn archive_name(version: &str, target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("{}-{version}-{target}.{ext}", env!("CARGO_PKG_NAME"))
}

/// `file`'s digest in `sha256sum` output: `<hex>  <name>`, or `<hex> *<name>` in binary mode.
fn expected_checksum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim_start();
        (name.strip_prefix('*').unwrap_or(name) == file).then(|| digest.to_ascii_lowercase())
    })
}

/// Stream the release asset `name` into `dest`, returning its SHA-256 as lowercase hex.
fn download(
    agent: &ureq::Agent,
    release: &Release,
    name: &str,
    dest: &Path,
) -> Result<String, String> {
    let fail = |e: &dyn Display| format!("failed to download {name}: {e}");
    let response = fetch_asset(agent, release, name)?;
    let mut progress = Progress::start(name, response.body().content_length());
    let mut body = response.into_body().into_reader();
    let mut file = File::create(dest).map_err(|e| fail(&e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = body.read(&mut buf).map_err(|e| fail(&e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| fail(&e))?;
        progress.advance(n);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `Downloading <file> (1.9 MB) 42%` on one stderr line, ended when dropped.
struct Progress {
    label: String,
    total: u64,
    received: u64,
    percent: Option<u64>,
    live: bool,
}

impl Progress {
    fn start(name: &str, size: Option<u64>) -> Progress {
        let label = match size {
            Some(size) => format!("Downloading {name} ({:.1} MB)", size as f64 / 1e6),
            None => format!("Downloading {name}"),
        };
        eprint!("{label}");
        let total = size.unwrap_or(0);
        Progress {
            label,
            total,
            received: 0,
            percent: None,
            live: io::stderr().is_terminal() && total > 0,
        }
    }

    fn advance(&mut self, bytes: usize) {
        self.received += bytes as u64;
        if !self.live {
            return;
        }
        let percent = (self.received * 100 / self.total).min(100);
        if self.percent != Some(percent) {
            self.percent = Some(percent);
            eprint!("\r{} {percent:>3}%", self.label);
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        eprintln!();
    }
}

/// Archives hold `rtools-<version>-<target>/rtools[.exe]`; Windows'
/// Compress-Archive may separate the path with `\`.
fn is_binary_entry(path: &str) -> bool {
    path.rsplit(['/', '\\']).next() == Some(BIN_FILE)
}

#[cfg(not(windows))]
fn extract_binary(archive: impl Read, dest: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in archive.entries().map_err(unreadable_archive)? {
        let mut entry = entry.map_err(unreadable_archive)?;
        let path = entry
            .path()
            .map_err(unreadable_archive)?
            .to_string_lossy()
            .into_owned();
        if entry.header().entry_type().is_file() && is_binary_entry(&path) {
            write_binary(&mut entry, dest)?;
            return fs::set_permissions(dest, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("failed to unpack {BIN_FILE}: {e}"));
        }
    }
    Err(format!("the downloaded archive has no {BIN_FILE}"))
}

#[cfg(windows)]
fn extract_binary(archive: impl Read + io::Seek, dest: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(archive).map_err(unreadable_archive)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(unreadable_archive)?;
        if entry.is_file() && is_binary_entry(entry.name()) {
            return write_binary(&mut entry, dest);
        }
    }
    Err(format!("the downloaded archive has no {BIN_FILE}"))
}

fn write_binary(mut from: impl Read, dest: &Path) -> Result<(), String> {
    File::create(dest)
        .and_then(|mut file| io::copy(&mut from, &mut file))
        .map(drop)
        .map_err(|e| format!("failed to unpack {BIN_FILE}: {e}"))
}

fn unreadable_archive(e: impl Display) -> String {
    format!("failed to read the downloaded archive: {e}")
}

/// Run the new binary once, so one that can't start here never replaces a working rtools.
fn check_runs(binary: &Path) -> Result<(), String> {
    let status = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("the downloaded rtools does not run on this system: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the downloaded rtools failed to start ({status})"))
    }
}

fn write_error(dir: &Path, e: io::Error) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        let how = if cfg!(windows) {
            "from an administrator terminal"
        } else {
            "with sudo"
        };
        format!(
            "no permission to write to {}, run `rtools update` {how}",
            dir.display()
        )
    } else {
        format!("failed to replace rtools in {}: {e}", dir.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(version: &str) -> Version {
        Version::parse(version).unwrap()
    }

    fn release(tag: &str) -> Release {
        Release {
            tag: tag.to_string(),
        }
    }

    #[test]
    fn names_archives_like_the_release_workflow() {
        assert_eq!(
            archive_name("1.2.3", "x86_64-unknown-linux-musl"),
            "rtools-1.2.3-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            archive_name("1.2.3-rc.1", "aarch64-pc-windows-msvc"),
            "rtools-1.2.3-rc.1-aarch64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn reads_versions_from_tags() {
        assert_eq!(release("v1.2.3").version(), Ok(v("1.2.3")));
        assert_eq!(release("1.2.3").version(), Ok(v("1.2.3")));
        assert_eq!(release("v1.0.0-rc.1").tag_version(), "1.0.0-rc.1");
        assert!(release("v1.0.0-rc.1").version().unwrap() < v("1.0.0"));
        assert!(release("nightly").version().is_err());
    }

    #[test]
    fn finds_the_tag_the_latest_release_link_lands_on() {
        let releases = "https://github.com/ssenerg/rtools/releases";
        assert_eq!(
            tag_from_url(&format!("{releases}/tag/v0.2.0")).as_deref(),
            Some("v0.2.0")
        );
        assert_eq!(
            tag_from_url(&format!("{releases}/tag/v1.0.0-rc.1?from=latest")).as_deref(),
            Some("v1.0.0-rc.1")
        );
        // No release yet: the link lands on the (empty) release list.
        assert_eq!(tag_from_url(releases), None);
        assert_eq!(tag_from_url(&format!("{releases}/tag/")), None);
    }

    #[test]
    fn links_to_release_pages_and_assets() {
        let release = release("v0.2.0");
        assert_eq!(
            release.url(),
            "https://github.com/ssenerg/rtools/releases/tag/v0.2.0"
        );
        assert_eq!(
            release.asset_url("SHA256SUMS"),
            "https://github.com/ssenerg/rtools/releases/download/v0.2.0/SHA256SUMS"
        );
    }

    #[test]
    fn finds_checksums_in_sha256sum_output() {
        let sums = "AB12  rtools-1.0.0-a.tar.gz\ncd34 *rtools-1.0.0-b.zip\n";
        assert_eq!(
            expected_checksum(sums, "rtools-1.0.0-a.tar.gz").as_deref(),
            Some("ab12")
        );
        assert_eq!(
            expected_checksum(sums, "rtools-1.0.0-b.zip").as_deref(),
            Some("cd34")
        );
        assert_eq!(expected_checksum(sums, "rtools-1.0.0-c.zip"), None);
        assert_eq!(expected_checksum(sums, "1.0.0-a.tar.gz"), None);
    }

    #[test]
    fn hashes_as_lowercase_hex() {
        assert_eq!(
            hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn spots_the_binary_inside_archives() {
        assert!(is_binary_entry(BIN_FILE));
        assert!(is_binary_entry(&format!("rtools-1.0.0-x/{BIN_FILE}")));
        assert!(is_binary_entry(&format!("rtools-1.0.0-x\\{BIN_FILE}")));
        assert!(!is_binary_entry("rtools-1.0.0-x/README.md"));
        assert!(!is_binary_entry(&format!("rtools-1.0.0-x/{BIN_FILE}.sig")));
        assert!(!is_binary_entry("rtools-1.0.0-x/"));
    }

    #[cfg(not(windows))]
    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(windows)]
    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (path, data) in files {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_only_the_binary() {
        let binary = format!("rtools-1.0.0-x/{BIN_FILE}");
        let with_binary = archive(&[
            ("rtools-1.0.0-x/README.md", b"readme"),
            (binary.as_str(), b"new rtools"),
        ]);
        let without_binary = archive(&[("rtools-1.0.0-x/README.md", b"readme")]);

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(BIN_FILE);
        extract_binary(io::Cursor::new(with_binary), &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new rtools");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }

        let other = dir.path().join("other");
        assert!(extract_binary(io::Cursor::new(without_binary), &other).is_err());
        assert!(!other.exists());
    }

    #[test]
    fn clears_only_stale_staging() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(format!("{STAGING_PREFIX}abc123"));
        let unrelated = dir.path().join("keep-me");
        fs::create_dir(&staging).unwrap();
        fs::create_dir(&unrelated).unwrap();

        remove_stale_staging(dir.path(), STALE_STAGING);
        assert!(
            staging.exists(),
            "may belong to an update running right now"
        );

        remove_stale_staging(dir.path(), Duration::ZERO);
        assert!(!staging.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn state_round_trips_and_tolerates_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rtools").join("update.json");
        assert_eq!(State::load(&path), State::default());

        let state = State {
            mode: Mode::Notify,
            last_check: 7,
            latest: Some("1.2.3".to_string()),
            last_notice: 5,
            skipped: None,
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);

        fs::write(&path, r#"{"mode": "auto"}"#).unwrap();
        assert_eq!(
            State::load(&path),
            State {
                mode: Mode::Auto,
                ..State::default()
            }
        );
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(State::load(&path), State::default());
    }

    #[test]
    fn announces_newer_releases_once_per_interval_unless_skipped() {
        let current = v("1.0.0");
        let now = 10 * CHECK_INTERVAL;
        let newer = State {
            latest: Some("1.1.0".to_string()),
            ..State::default()
        };
        assert_eq!(newer.announcement(&current, now), Some(v("1.1.0")));

        let told_recently = State {
            last_notice: now - 60,
            ..newer.clone()
        };
        let skipped = State {
            skipped: Some("1.1.0".to_string()),
            ..newer.clone()
        };
        let off = State {
            mode: Mode::Off,
            ..newer.clone()
        };
        let not_newer = State {
            latest: Some("1.0.0".to_string()),
            ..State::default()
        };
        for state in [told_recently, skipped, off, not_newer, State::default()] {
            assert_eq!(state.announcement(&current, now), None, "{state:?}");
        }

        let newer_than_skipped = State {
            latest: Some("1.2.0".to_string()),
            skipped: Some("1.1.0".to_string()),
            ..State::default()
        };
        assert_eq!(
            newer_than_skipped.announcement(&current, now),
            Some(v("1.2.0"))
        );
    }

    #[test]
    fn checks_are_due_once_per_interval() {
        assert!(is_due(0, CHECK_INTERVAL));
        assert!(!is_due(100, 100 + CHECK_INTERVAL - 1));
        assert!(is_due(100, 100 + CHECK_INTERVAL));
        assert!(is_due(500, 100), "clock went backwards");
    }

    #[test]
    fn only_explicit_answers_install_anything() {
        assert_eq!(Answer::parse("y"), Answer::Yes);
        assert_eq!(Answer::parse("yes"), Answer::Yes);
        assert_eq!(Answer::parse("a"), Answer::Always);
        assert_eq!(Answer::parse("always"), Answer::Always);
        assert_eq!(Answer::parse("s"), Answer::Skip);
        assert_eq!(Answer::parse(""), Answer::No);
        assert_eq!(Answer::parse("n"), Answer::No);
        assert_eq!(Answer::parse("ls -la"), Answer::No);
    }
}
