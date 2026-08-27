#!/usr/bin/env sh
# Build from source. Prebuilt release assets can replace this once CI publishes them;
# until then every install compiles with cargo.
set -eu
cargo build --release
mkdir -p bin
cp target/release/herdr-flash bin/herdr-flash
