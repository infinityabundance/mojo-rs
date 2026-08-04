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
  gn gen "$OUT" --args='
    is_debug = false
    is_component_build = false
    use_custom_libcxx = false
    symbol_level = 0
    enable_nacl = false
    treat_warnings_as_errors = false
    use_sysroot = false
    use_allocator = "none"
    fieldtrial_testing_config = "[]"
    enable_remoting = false
    enable_print_preview = false
    blink_symbol_level = 0
  '
else
  log "gn gen (reuse args.gn)"
  gn gen "$OUT"
fi

log "ninja mojo_rs_oracle_driver"
ninja -C "$OUT" -j"$(nproc)" mojo_rs_oracle_driver

DRIVER="$OUT/mojo_rs_oracle_driver"
[ -x "$DRIVER" ] || fail "oracle driver not produced"
log "oracle driver: $DRIVER"
sha256sum "$DRIVER"
