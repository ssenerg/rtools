# rtools

Small developer tools in one binary: UUIDs, time conversion, Go structs from
JSON, QR codes, listening ports and more.

## Install

Every [release](https://github.com/ssenerg/rtools/releases/latest) has prebuilt
binaries for Linux, macOS and Windows, on both x86_64 and ARM64. The commands
below install the latest one.

### Linux and macOS

```sh
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)                TARGET=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) TARGET=aarch64-unknown-linux-musl ;;
  Darwin-arm64)                TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64)               TARGET=x86_64-apple-darwin ;;
  *) echo "No prebuilt rtools for $(uname -sm), see 'From source' below." ;;
esac
VERSION=$(curl -fsSLo /dev/null -w '%{url_effective}' https://github.com/ssenerg/rtools/releases/latest)
VERSION=${VERSION##*/v}
curl -fsSL "https://github.com/ssenerg/rtools/releases/download/v$VERSION/rtools-$VERSION-$TARGET.tar.gz" | tar xz
mkdir -p ~/.local/bin
mv "rtools-$VERSION-$TARGET/rtools" ~/.local/bin/
rmdir "rtools-$VERSION-$TARGET"
rtools --version
```

If the last line says `rtools: command not found`, add `~/.local/bin` to your
`PATH`. With zsh, the macOS default, that is:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
```

Then open a new terminal.

### Windows

In PowerShell:

```powershell
$Target  = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$Version = (Invoke-RestMethod https://api.github.com/repos/ssenerg/rtools/releases/latest).tag_name.TrimStart("v")
$Dir     = "$env:LOCALAPPDATA\Programs\rtools"
$Zip     = "$env:TEMP\rtools.zip"
Invoke-WebRequest "https://github.com/ssenerg/rtools/releases/download/v$Version/rtools-$Version-$Target.zip" -OutFile $Zip
Expand-Archive $Zip "$env:TEMP\rtools" -Force
New-Item -ItemType Directory -Force $Dir | Out-Null
Move-Item "$env:TEMP\rtools\rtools-$Version-$Target\rtools.exe" $Dir -Force
Remove-Item $Zip, "$env:TEMP\rtools" -Recurse -Force
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$Dir*") { [Environment]::SetEnvironmentVariable("Path", "$UserPath;$Dir", "User") }
```

Then open a new terminal and run `rtools --version`.

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

| Command    | What it does                                            |
| ---------- | ------------------------------------------------------- |
| `uuid`     | Generate UUIDs (versions 1 and 3–8)                     |
| `tconv`    | Convert between Unix timestamps, ISO 8601 and dates     |
| `gostruct` | Generate a Go struct from JSON                          |
| `qrcode`   | Show text as a QR code in the terminal                  |
| `ports`    | List listening ports and kill the processes behind them |
| `factor`   | Factor a number into primes                             |
| `jwt`      | Decode, verify and create JSON Web Tokens               |
| `update`   | Update rtools to the latest release                     |

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
