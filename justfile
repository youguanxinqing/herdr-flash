# Development commands for Herdr Flash. `just --list` shows this menu.
#
# The Herdr install path stays `scripts/build.sh` (plain sh + cargo) so consumers
# installing the plugin never need just; `just build` runs that same script.

# Run everything AGENTS.md requires before a task counts as done.
verify: fmt test clippy

# Build the release binary and stage it where the plugin manifest expects it.
build:
    ./scripts/build.sh

fmt:
    cargo fmt --all -- --check

test:
    cargo test --all-features

clippy:
    cargo clippy --all-targets --all

# Prove the test suite is immune to ambient color policy.
test-no-color:
    NO_COLOR=1 cargo test --all-features
