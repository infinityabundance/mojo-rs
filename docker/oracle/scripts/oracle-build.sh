#!/usr/bin/env bash
# oracle-build.sh — container entrypoint for the oracle service.
# Builds the pinned official Mojo oracle (libmojo_core_embedder + driver) into
# the persistent /work/oracle-build, incrementally.
set -euo pipefail

export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC0
export PATH="/opt/depot_tools:$PATH"
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1785863982}"

SRC=/work/oracle-source/src
OUT=/work/oracle-build/out/Oracle
COMMIT="${CHROMIUM_COMMIT:-bfa3579138998e2fbb981725570fa588c5b6f8cd}"
# The pinned checkout's own prebuilt gn (downloaded by gclient hooks) is the
# deterministic gn; the depot_tools gn wrapper needs an interactive bootstrap.
GN_BIN="$SRC/buildtools/linux64/gn"

log() { printf '\n=== [oracle-build] %s ===\n' "$*"; }
fail() { echo "FATAL: $*" >&2; exit 1; }

log "verifying pinned source"
[ -d "$SRC" ] || fail "pinned source missing at $SRC (run scripts/fetch_oracle_source.sh)"
ACTUAL="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo missing)"
[ "$ACTUAL" = "$COMMIT" ] || fail "source revision mismatch: got $ACTUAL expected $COMMIT"
[ -f "$SRC/oracle_driver/BUILD.gn" ] || fail "oracle driver patch not applied to the checkout"
log "pinned source verified: $ACTUAL"

mkdir -p "$OUT"
cd "$SRC"

if [ ! -f "$OUT/args.gn" ]; then
  log "gn gen (initial)"
  "$GN_BIN" gen "$OUT" --args='
    is_debug = false
    is_component_build = false
    v8_enable_sandbox = false
    symbol_level = 0
    treat_warnings_as_errors = false
    use_sysroot = false
    root_extra_deps = ["//oracle_driver:mojo_rs_oracle_driver"]
  '
else
  log "gn gen (reuse args.gn)"
  "$GN_BIN" gen "$OUT"
fi

log "ninja oracle_driver:mojo_rs_oracle_driver"
ninja -C "$OUT" -j"$(nproc)" oracle_driver:mojo_rs_oracle_driver

DRIVER="$OUT/mojo_rs_oracle_driver"
[ -x "$DRIVER" ] || fail "oracle driver not produced"
log "oracle driver: $DRIVER"
sha256sum "$DRIVER"
