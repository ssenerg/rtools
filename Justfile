alias pc := pre-commit

[private]
default:
    @just -f {{ justfile() }} --list --unsorted

# Runs the pre-commit checks.
pre-commit:
    @cargo fmt --all
    @cargo clippy --all-targets --all-features --locked -- -D warnings
    @cargo check --all-targets --locked
