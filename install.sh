#!/bin/sh
# Installs the latest rtools release on Linux or macOS:
#
#   curl -fsSL https://raw.githubusercontent.com/ssenerg/rtools/main/install.sh | sh
#
# RTOOLS_VERSION picks a version (default: the latest release) and
# RTOOLS_INSTALL_DIR where it goes (default: ~/.local/bin).
set -eu

repo=ssenerg/rtools
dir=${RTOOLS_INSTALL_DIR:-$HOME/.local/bin}

fail() {
    echo "rtools install: $*" >&2
    exit 1
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-musl ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    *) fail "there's no prebuilt rtools for $(uname -sm). Build it with: cargo install --git https://github.com/$repo --locked" ;;
esac
command -v curl >/dev/null 2>&1 || fail "curl is needed to download rtools"

version=${RTOOLS_VERSION:-}
if [ -z "$version" ]; then
    # The latest-release link redirects to the release's tag.
    latest=$(curl -fsSLo /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") ||
        fail "couldn't reach GitHub"
    version=${latest##*/tag/}
    [ "$version" != "$latest" ] || fail "no rtools release has been published yet"
fi
version=${version#v}

archive="rtools-$version-$target.tar.gz"
base="https://github.com/$repo/releases/download/v$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading rtools $version for $target..."
curl -fsSL "$base/$archive" -o "$tmp/$archive" || fail "couldn't download $base/$archive"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || fail "couldn't download $base/SHA256SUMS"

expected=$(awk -v file="$archive" '$2 == file || $2 == "*" file { print $1 }' "$tmp/SHA256SUMS")
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$archive" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$tmp/$archive" | cut -d ' ' -f 1)
fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    fail "$archive doesn't match its checksum, not installing it"
fi

tar xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$dir"
# Copy next to the destination, then rename: a running rtools is never overwritten in place.
cp "$tmp/rtools-$version-$target/rtools" "$dir/.rtools.new"
chmod 755 "$dir/.rtools.new"
mv -f "$dir/.rtools.new" "$dir/rtools"
echo "Installed rtools $version to $dir/rtools"

case ":$PATH:" in
    *":$dir:"*) ;;
    *)
        echo
        echo "$dir isn't on your PATH yet. Add this line to your shell's profile"
        echo "(~/.zshrc on macOS, ~/.bashrc on most Linux systems) and open a new terminal:"
        echo
        echo "    export PATH=\"$dir:\$PATH\""
        ;;
esac
