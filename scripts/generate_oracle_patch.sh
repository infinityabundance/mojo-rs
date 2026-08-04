#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# generate_oracle_patch.sh — generate and apply the deterministic patch that
# adds the oracle driver to the pinned Chromium checkout.
#
# The patch adds two files at $SRC/oracle_driver/:
#   BUILD.gn           (from oracle/gn/BUILD.gn)
#   oracle_driver.cc   (from oracle/driver/oracle_driver.cc)
#
# The patch is generated from the committed sources with `git diff` against
# the pinned checkout, so it is deterministic and reviewable. The committed
# patch at oracle/patches/mojo-rs-oracle-driver.patch is regenerated whenever
# the driver changes; the fetch script applies it.
#
# Usage:
#   bash scripts/generate_oracle_patch.sh [--apply]
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

SRC="$MOJO_RS_ORACLE_SOURCE_ROOT/src"
PATCH_OUT="$MOJO_RS_REPO_ROOT/oracle/patches/mojo-rs-oracle-driver.patch"
APPLY=0
if [ "${1:-}" = "--apply" ]; then
  APPLY=1
fi

[ -d "$SRC/.git" ] || mojo_rs_fail "pinned checkout missing at $SRC (run scripts/fetch_oracle_source.sh)"

DRIVER_DIR="$SRC/oracle_driver"
mkdir -p "$DRIVER_DIR"

# Copy the committed driver sources into the checkout (stage only).
cp "$MOJO_RS_REPO_ROOT/oracle/driver/oracle_driver.cc" "$DRIVER_DIR/oracle_driver.cc"
cp "$MOJO_RS_REPO_ROOT/oracle/gn/BUILD.gn" "$DRIVER_DIR/BUILD.gn"

# If the files are unchanged, the patch is already current.
if [ -f "$PATCH_OUT" ]; then
  # Re-create from scratch for determinism.
  rm -f "$PATCH_OUT"
fi

git -C "$SRC" add oracle_driver/
git -C "$SRC" diff --cached --binary > "$PATCH_OUT"
git -C "$SRC" reset -q
git -C "$SRC" checkout -q -- oracle_driver/ 2>/dev/null || rm -rf "$DRIVER_DIR"

mojo_rs_log "patch written: $PATCH_OUT ($(wc -l < "$PATCH_OUT") lines, sha256 $(mojo_rs_sha256 "$PATCH_OUT"))"

if [ "$APPLY" = 1 ]; then
  mojo_rs_log "applying patch to the pinned checkout"
  git -C "$SRC" apply --check "$PATCH_OUT" || mojo_rs_fail "patch does not apply cleanly"
  git -C "$SRC" apply "$PATCH_OUT"
  mojo_rs_ok "patch applied: $SRC/oracle_driver/"
fi
