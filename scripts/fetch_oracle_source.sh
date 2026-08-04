#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# fetch_oracle_source.sh — fetch and verify the pinned Chromium source for the
# oracle epoch, into $MOJO_RS_ORACLE_SOURCE_ROOT (host-side persistent store).
#
#   1. depot_tools at the pinned commit   -> $MOJO_RS_BUILD_CACHE/depot_tools
#   2. chromium/src at the pinned tag     -> $MOJO_RS_ORACLE_SOURCE_ROOT/src
#   3. .gclient + gclient sync (deps)     (shallow; full fallback)
#   4. gclient runhooks                   (pinned clang, gn, ninja, ...)
#   5. oracle driver patch application    (from oracle/patches/, if present)
#   6. receipt: source receipt JSON
#
# Long-running: run in the background (nohup ... &) and poll the log.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

CHROMIUM_TAG="${MOJO_RS_CHROMIUM_TAG:-151.0.7922.105}"
CHROMIUM_COMMIT="${MOJO_RS_CHROMIUM_COMMIT:-bfa3579138998e2fbb981725570fa588c5b6f8cd}"
DEPOT_TOOLS_COMMIT="${MOJO_RS_DEPOT_TOOLS_COMMIT:-d22ef3bf62a8c3c76d9c7427015bdfec7665587a}"
SRC_REPO="https://chromium.googlesource.com/chromium/src"
DEPOT_REPO="https://chromium.googlesource.com/chromium/tools/depot_tools"
SHALLOW="${MOJO_RS_GCLIENT_SHALLOW:-1}"

DEPOT="$MOJO_RS_BUILD_CACHE/depot_tools"
SRC_ROOT="$MOJO_RS_ORACLE_SOURCE_ROOT"
SRC="$SRC_ROOT/src"
LOG="$MOJO_RS_LOGS/oracle-fetch.log"

mkdir -p "$MOJO_RS_BUILD_CACHE" "$SRC_ROOT" "$MOJO_RS_LOGS"

log() { printf '\n=== [fetch] %s ===\n' "$*" | tee -a "$LOG"; }
info() { printf '  %s\n' "$*" | tee -a "$LOG"; }
fail() { echo "FATAL: $*" | tee -a "$LOG" >&2; exit 1; }

mojo_rs_require_cmd git
mojo_rs_require_cmd curl
mojo_rs_require_cmd python3

# ---------------------------------------------------------------------------
# 1. depot_tools (pinned commit)
# ---------------------------------------------------------------------------
log "depot_tools (pinned $DEPOT_TOOLS_COMMIT)"
if [ ! -d "$DEPOT/.git" ]; then
  info "cloning depot_tools..."
  git clone "$DEPOT_REPO" "$DEPOT" >> "$LOG" 2>&1 || fail "depot_tools clone failed"
fi
ACTUAL_DEPOT="$(git -C "$DEPOT" rev-parse HEAD 2>/dev/null || echo missing)"
if [ "$ACTUAL_DEPOT" != "$DEPOT_TOOLS_COMMIT" ]; then
  info "checking out pinned commit (was $ACTUAL_DEPOT)"
  git -C "$DEPOT" fetch origin "$DEPOT_TOOLS_COMMIT" >> "$LOG" 2>&1 || fail "depot_tools fetch failed"
  git -C "$DEPOT" checkout "$DEPOT_TOOLS_COMMIT" >> "$LOG" 2>&1 || fail "depot_tools checkout failed"
fi
info "depot_tools at $(git -C "$DEPOT" rev-parse HEAD)"
export PATH="$DEPOT:$PATH"

# ---------------------------------------------------------------------------
# 2. chromium/src at the pinned tag
# ---------------------------------------------------------------------------
log "chromium/src (tag $CHROMIUM_TAG, commit $CHROMIUM_COMMIT)"
if [ ! -d "$SRC/.git" ]; then
  info "cloning chromium/src at $CHROMIUM_TAG (blob:none, depth 1)..."
  git clone --depth 1 --branch "$CHROMIUM_TAG" --filter=blob:none \
    "$SRC_REPO" "$SRC" >> "$LOG" 2>&1 || fail "chromium/src clone failed"
fi
ACTUAL_SRC="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo missing)"
if [ "$ACTUAL_SRC" != "$CHROMIUM_COMMIT" ]; then
  fail "chromium/src revision mismatch: got $ACTUAL_SRC expected $CHROMIUM_COMMIT"
fi
info "chromium/src at $ACTUAL_SRC"

# ---------------------------------------------------------------------------
# 3. .gclient + gclient sync
# ---------------------------------------------------------------------------
log "gclient sync"
cat > "$SRC_ROOT/.gclient" <<EOF
solutions = [
  {
    "name": "src",
    "url": "$SRC_REPO.git",
    "deps_file": "DEPS",
    "managed": False,
    "custom_deps": {},
  },
]
EOF

# Shallow syncs are used throughout: deepening already-shallow dep clones
# costs many hours for zero build benefit (the working tree at the pinned rev
# is identical). If the shallow attempt fails, retry shallow once.
SYNC_OK=0
if [ "$SHALLOW" = 1 ]; then
  for attempt in 1 2 3; do
    info "gclient sync --shallow (attempt $attempt)"
    if (cd "$SRC_ROOT" && gclient sync --shallow --no-history >> "$LOG" 2>&1); then
      SYNC_OK=1
      break
    fi
    info "shallow attempt $attempt failed; retrying"
  done
fi
if [ "$SYNC_OK" = 0 ]; then
  (cd "$SRC_ROOT" && gclient sync --shallow --no-history >> "$LOG" 2>&1) \
    || fail "gclient sync failed"
fi
info "gclient sync complete"

# ---------------------------------------------------------------------------
# 4. hooks (pinned clang, gn, ninja, ...)
# ---------------------------------------------------------------------------
log "gclient runhooks"
(cd "$SRC_ROOT" && gclient runhooks >> "$LOG" 2>&1) || fail "gclient runhooks failed"
info "hooks complete"

# ---------------------------------------------------------------------------
# 5. oracle driver patch (deterministic; generated from oracle/driver/)
# ---------------------------------------------------------------------------
PATCH="$MOJO_RS_REPO_ROOT/oracle/patches/mojo-rs-oracle-driver.patch"
if [ -f "$PATCH" ]; then
  log "applying oracle driver patch"
  git -C "$SRC" apply --check "$PATCH" >> "$LOG" 2>&1 \
    || fail "oracle driver patch does not apply cleanly (regenerate with scripts/generate_oracle_patch.sh)"
  git -C "$SRC" apply "$PATCH" >> "$LOG" 2>&1 || fail "oracle driver patch apply failed"
  info "patch applied: $(sha256sum "$PATCH" | cut -d' ' -f1)"
else
  info "no patch yet (scripts/generate_oracle_patch.sh will produce one)"
fi

# ---------------------------------------------------------------------------
# 6. receipt
# ---------------------------------------------------------------------------
RECEIPT="$MOJO_RS_STATE/oracle-source-receipt.json"
mkdir -p "$MOJO_RS_STATE"
DEPS_SHA="$(mojo_rs_sha256 "$SRC/DEPS")"
SRC_SIZE="$(du -sb "$SRC" 2>/dev/null | awk '{print $1}')"
cat > "$RECEIPT" <<EOF
{
  "schema_version": 1,
  "generated_at": "$(mojo_rs_now_utc)",
  "chromium_tag": "$CHROMIUM_TAG",
  "chromium_commit": "$CHROMIUM_COMMIT",
  "chromium_src_head": "$ACTUAL_SRC",
  "deps_sha256": "$DEPS_SHA",
  "depot_tools_commit": "$DEPOT_TOOLS_COMMIT",
  "depot_tools_head": "$ACTUAL_DEPOT",
  "gclient_shallow": $([ "$SHALLOW" = 1 ] && echo true || echo false),
  "source_root": "$SRC_ROOT",
  "source_size_bytes": ${SRC_SIZE:-0},
  "patch_sha256": "$([ -f "$PATCH" ] && mojo_rs_sha256 "$PATCH" || echo none)",
  "hooks_complete": true,
  "log": "$LOG"
}
EOF
info "receipt: $RECEIPT"
cat "$RECEIPT"
mojo_rs_ok "oracle source fetch complete"
