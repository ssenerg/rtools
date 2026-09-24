alias pc := pre-commit
alias b  := build

[private]
default:
    @just -f {{ justfile() }} --list --unsorted

# Runs the pre-commit checks.
pre-commit:
    @cargo fmt --all
    @cargo clippy --all-targets --all-features --locked -- -D warnings
    @cargo check --all-targets --locked

build:
    @cargo build --release

# Sets the version in Cargo.toml and Cargo.lock, ready for a release PR.
bump version:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! "{{ version }}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
        echo "error: {{ version }} isn't a version like 1.2.3" >&2
        exit 1
    fi
    sed -i.bak '1,/^version = /s/^version = ".*"/version = "{{ version }}"/' Cargo.toml
    rm Cargo.toml.bak
    cargo check --quiet
    echo "Cargo.toml and Cargo.lock now say {{ version }}. Merge that, then run: just release"

# Tags the version on origin/main and pushes the tag, which publishes the release.
release:
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch --quiet origin main --tags
    version=$(git show origin/main:Cargo.toml | sed -n 's/^version = "\(.*\)"$/\1/p' | head -n 1)
    tag="v$version"
    if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null; then
        echo "error: $tag is already released. Bump the version first (just bump X.Y.Z) and merge that." >&2
        exit 1
    fi
    if git rev-parse --quiet --verify "refs/tags/$tag" >/dev/null; then
        echo "error: there's a local $tag tag that GitHub doesn't have. Remove it with: git tag -d $tag" >&2
        exit 1
    fi
    echo "Tagging origin/main as $tag: $(git log -1 --format='%h %s' origin/main)"
    git tag -a "$tag" origin/main -m "rtools $version"
    git push origin "$tag"
    echo "Building the release: https://github.com/ssenerg/rtools/actions/workflows/release.yml"
