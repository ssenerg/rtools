# rtools

Small developer tools in one binary: UUIDs, time conversion, JWTs, hashes,
JSON, cron schedules, Go structs from JSON, QR codes, listening ports and more.

## Install

Linux and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/ssenerg/rtools/main/install.sh | sh
```

Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/ssenerg/rtools/main/install.ps1 | iex
```

The scripts ([install.sh](install.sh), [install.ps1](install.ps1)) download the
latest release for your platform, check it against the release's `SHA256SUMS`
and install it into a folder you own, so rtools can update itself without admin
rights: `~/.local/bin`, or `%LOCALAPPDATA%\Programs\rtools` on Windows, which
they add to your `PATH`. `RTOOLS_VERSION` picks a version and
`RTOOLS_INSTALL_DIR` the folder:

```sh
curl -fsSL https://raw.githubusercontent.com/ssenerg/rtools/main/install.sh | RTOOLS_VERSION=0.2.0 sh
```

To install by hand instead, download the archive for your platform from the
[latest release](https://github.com/ssenerg/rtools/releases/latest) (Linux,
macOS and Windows, on x86_64 and ARM64), check it against `SHA256SUMS`, and put
`rtools` (`rtools.exe` on Windows) in a folder on your `PATH`.

### From source

With [Rust](https://rustup.rs) installed:

```sh
cargo install --git https://github.com/ssenerg/rtools --locked
```

### Checking a download

Each release lists the SHA-256 of every archive in `SHA256SUMS`, and GitHub
attests where each archive was built. After downloading an archive and
`SHA256SUMS` from the release page:

```sh
sha256sum --check --ignore-missing SHA256SUMS   # on macOS: shasum -a 256 --check --ignore-missing SHA256SUMS
gh attestation verify rtools-<version>-<target>.tar.gz --repo ssenerg/rtools
```

## Updating

rtools checks for a new release at most once a day, and only when you use it in
a terminal. When it finds one, it asks before installing anything:

```
A new version of rtools is available: 0.1.1 → 0.2.0
Release notes: https://github.com/ssenerg/rtools/releases/tag/v0.2.0
Update now? [y]es, [N]o, [a]lways (auto-update), [s]kip this version:
```

You can also update yourself:

- `rtools update` installs the latest release.
- `rtools update --check` only tells you whether there is one.
- `rtools update --mode <ask|auto|notify|off>` sets what the daily check does:
  ask first (the default), install automatically, only print a notice, or not
  check at all.

rtools replaces its own binary, so keep it somewhere you can write to, like
`~/.local/bin` above. Otherwise updating needs `sudo rtools update`, or on
Windows an administrator terminal.

## Tab completion

`rtools completions` prints a script that makes Tab complete rtools' commands
and options. To load it in every new terminal, run the line for your shell once:

```sh
echo 'eval "$(rtools completions bash)"' >> ~/.bashrc                  # bash
echo 'eval "$(rtools completions zsh)"' >> ~/.zshrc                    # zsh
echo 'rtools completions fish | source' >> ~/.config/fish/config.fish  # fish
```

```powershell
if (!(Test-Path $PROFILE)) { New-Item -Force $PROFILE | Out-Null }
Add-Content $PROFILE 'rtools completions powershell | Out-String | Invoke-Expression'
```

Then open a new terminal. In zsh the line needs to come after `compinit`, which
oh-my-zsh and most setups already run. The script is made fresh each time a
terminal opens, so it keeps up as rtools updates.

## Commands

| Command       | What it does                                                                                |
| ------------- | ------------------------------------------------------------------------------------------- |
| `uuid`        | Generate UUIDs (versions 1 and 3–8)                                                         |
| `tconv`       | Convert times between timestamps, dates, time zones and the Jalali calendar, with date math |
| `gostruct`    | Generate a Go struct from JSON                                                              |
| `qrcode`      | Show text as a QR code in the terminal                                                      |
| `ports`       | List listening ports and kill the processes behind them                                     |
| `factor`      | Factor a number into primes                                                                 |
| `jwt`         | Decode, verify and create JSON Web Tokens                                                   |
| `hash`        | Hash text or files, or check them against a checksum file                                   |
| `json`        | Pretty-print, minify, validate and query JSON                                               |
| `conv`        | Convert between JSON, YAML and TOML, in any direction                                       |
| `logs`        | Make streaming logs readable: pretty-print JSON and filter with regexes                     |
| `watch`       | Rerun a command every few seconds and highlight what changed                                |
| `enc`         | Encode or decode base64, base64url, hex and URL encoding                                    |
| `cron`        | Explain a cron schedule in plain English and list when it runs next                         |
| `completions` | Print the script that makes Tab complete rtools commands                                    |
| `update`      | Update rtools to the latest release                                                         |

`rtools <command> --help` shows the details. Most commands take `-c` to copy
their output to the clipboard.

### JWT

```sh
# Show a token's header, payload and times. With a secret, also check its signature.
rtools jwt decode eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...
rtools jwt decode --secret "$JWT_SECRET" < token.txt

# Only the payload, for scripts. Exits with 1 if the signature doesn't match.
rtools jwt decode -p --secret-file secret.txt "$TOKEN" | jq .sub

# Create a token that expires in 15 minutes.
rtools jwt encode '{"sub":"42","role":"admin"}' --secret-file secret.txt --exp 15m --iat
```

Tokens are signed and checked with a shared secret (HS256, HS384 or HS512).
Tokens signed with a private key, like RS256 or ES256, can be decoded but not
checked.

### Logs

```sh
kubectl logs app -f | rtools logs                          # JSON pretty-printed like jq
kubectl logs app -f | rtools logs -e 'timeout|refused' -x healthz
kubectl logs app --since 1h | rtools logs --level warn --short
docker compose logs -f api | rtools logs -e panic -A 20    # a panic and the 20 lines after it
```

- JSON lines are pretty-printed and colored like `jq`, with the level colored by
  how serious it is. Unlike `jq`, lines that aren't JSON, like startup messages,
  panics and stack traces, pass through instead of stopping everything.
- JSON after a prefix is recognized too, like the ones `kubectl logs --timestamps`
  and `--prefix` add. logfmt lines (`level=info msg="..."`), from Go's slog text
  handler and logrus, are read as well.
- `--short` prints one line per entry: the time in your own time zone, the level,
  the message, then the other fields. Stack traces go below it.
- `-e` shows only lines matching a regex and highlights the matches, `-x` hides
  lines, `-i` ignores case, and `-A`, `-B` and `-C` add lines of context like
  grep. `--level warn` hides entries below that level.

### Converting JSON, YAML and TOML

```sh
rtools conv config.yaml --to toml        # print it as TOML
rtools conv config.yaml config.toml      # write a file; its extension picks the format
kubectl get deploy api -o json | rtools conv --to yaml
```

- The input format comes from the file's extension, `--from`, or is detected.
- YAML anchors and merge keys (`<<: *base`) are resolved, and key order is kept.
- The YAML it writes quotes anything an older YAML 1.1 reader would take for
  something else, like `yes`, `NO`, `1.20` or `2026-09-28`, so every tool reads
  back the same data.
- Several YAML documents (`---`) become a JSON array. TOML has no null, so nulls
  are left out with a warning, and TOML needs a table at the top level.

### Watching a command

```sh
rtools watch kubectl get pods                     # every 2 seconds
rtools watch -n 5 'kubectl get pods | grep api'   # one quoted argument runs through the shell
rtools watch --until Running kubectl get pod api-0
rtools watch -g curl -s https://example.com/status   # stop once the output changes
```

It fills the terminal and highlights words that changed since the last run. The
header shows the time and the exit code when the command fails. `q` or Ctrl-C
quits, space runs it again right away. When the output goes to a file or a pipe
instead, it prints the output each time it changes, which makes a simple change
log.

### Time

```sh
$ rtools tconv 'tomorrow + 9h' --tz Asia/Tehran --tz Europe/Berlin
Input
  tomorrow + 9h

UTC
  2026-09-26 05:30:00 UTC

Asia/Tehran
  2026-09-26 09:00:00 +03:30

Europe/Berlin
  2026-09-26 07:30:00 +02:00 CEST
...
```

- The input can be a Unix timestamp, an ISO 8601 date, `2026-03-20 09:00`, a
  Jalali date with `-j`, or `now`, `today`, `tomorrow` and `yesterday`.
- A Unix timestamp's unit (seconds, milliseconds, microseconds or nanoseconds) is
  picked by its size and shown next to the input, like
  `1700000000123 (Unix time in milliseconds)`. Decimals and negative timestamps
  work, and `--unit ms` sets the unit yourself.
- `--tz` shows the time in that zone; repeat it for several. Dates without an
  offset, and `today`, are read in the first zone instead of your own.
- Date math adds or subtracts durations: `now + 90m`, `2026-01-01 - 2d`,
  `-1w`, `now + 1y 6mo`. Days, months and years follow the calendar, so `+ 1d`
  keeps the time of day across a daylight-saving change.

### Cron

```sh
$ rtools cron '*/15 9-17 * * 1-5'
Schedule
  Every 15 minutes, from 09:00 through 17:45, Monday through Friday

Next runs
  Fri 2026-09-25 09:00  in 9 hours
  Fri 2026-09-25 09:15
  ...
```

- `--tz` names the time zone the schedule runs in, like `--tz UTC` for GitHub
  Actions and most Kubernetes clusters. The runs are then shown in your time
  too.
- `crontab -l | rtools cron` explains every line of your crontab, including
  `CRON_TZ` lines.
- It points out common mistakes. Setting both the day of month and the weekday
  runs on days that match *either* one. `*/7` minutes restarts every hour.
  There's no February 30th. And Linux cron and Kubernetes read some schedules
  differently.

Schedules use the standard five fields (minute, hour, day of month, month, day
of week) or `@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly` and `@reboot`,
and match the way Linux cron (cronie) runs them, including around daylight
saving changes.

### More examples

```sh
rtools hash rtools-0.3.0-x86_64-unknown-linux-musl.tar.gz   # sha256, like sha256sum
rtools hash --check SHA256SUMS         # check the downloads a checksum file lists
rtools hash -a md5 --text hello        # a string, without echo's newline

curl -s https://api.example.com/users | rtools json -q '.[0].email' -r
rtools json config.json --sort-keys    # invalid JSON points at the exact line and column

rtools enc base64 'hello world'        # aGVsbG8gd29ybGQ=
rtools enc url -d 'a%20b%26c'          # a b&c
rtools enc hex -i logo.png             # a file's exact bytes

rtools tconv now                       # includes the date in the Jalali calendar
rtools tconv --jalali 1403/07/02       # read a Jalali date
rtools tconv -j '1405/01/01 - 1d'      # the day before Nowruz
rtools tconv 1790000000 --tz America/New_York
rtools tconv '2026-03-20 09:00' --tz Europe/Berlin   # 09:00 in Berlin, in UTC and your time

curl -s https://api.github.com/users/octocat | rtools gostruct -n User   # gofmt-ready, with json tags

rtools cron @weekly -n 10              # the next 10 runs
rtools cron 'CRON_TZ=Asia/Tehran 0 9 * * *'
```
