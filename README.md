# rtools

Small developer tools in one binary: UUIDs, time conversion, Go structs from
JSON, QR codes, listening ports and more.

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

## Commands

| Command    | What it does                                                             |
| ---------- | ------------------------------------------------------------------------ |
| `uuid`     | Generate UUIDs (versions 1 and 3–8)                                      |
| `tconv`    | Convert between Unix timestamps, ISO 8601, dates and the Jalali calendar |
| `gostruct` | Generate a Go struct from JSON                                           |
| `qrcode`   | Show text as a QR code in the terminal                                   |
| `ports`    | List listening ports and kill the processes behind them                  |
| `factor`   | Factor a number into primes                                              |
| `jwt`      | Decode, verify and create JSON Web Tokens                                |
| `hash`     | Hash text or files, or check them against a checksum file                |
| `json`     | Pretty-print, minify, validate and query JSON                            |
| `enc`      | Encode or decode base64, base64url, hex and URL encoding                 |
| `update`   | Update rtools to the latest release                                      |

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
```
