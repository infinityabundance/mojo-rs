#!/usr/bin/env bash
# candidate-build.sh — container entrypoint for the candidate service.
# Builds the mojo-rs workspace (pinned Rust toolchain) into /work/target.
set -euo pipefail

export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC0
export RUSTUP_HOME=/work/toolchain/rustup CARGO_HOME=/work/toolchain/cargo
export CARGO_TARGET_DIR=/work/target
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1785863982}"

RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-1.96.0}"

log() { printf '\n=== [candidate-build] %s ===\n' "$*"; }
fail() { echo "FATAL: $*" >&2; exit 1; }

log "rust toolchain (rustup, pinned $RUST_TOOLCHAIN)"
if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
  mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal \
    --default-toolchain "$RUST_TOOLCHAIN" --no-modify-path
fi
"$CARGO_HOME/bin/rustc" --version
"$CARGO_HOME/bin/cargo" --version

log "building the mojo-rs workspace"
cd /repo
cargo build --release --workspace
log "workspace build complete"
