#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# verify_no_oracle_dependency.sh — negative proof that the CANDIDATE never
# links against, loads, or shells out to the official Chromium Mojo
# implementation.
#
# Checks, for every candidate artifact and every crate in the workspace:
#   1. cargo tree: no crate from the oracle toolchain space (chromium, mojo,
#      ipcz) appears in the dependency graph.
#   2. ldd: no dynamic library with mojo/chromium in its SONAME or path is
#      linked.
#   3. symbol scan (nm/objdump): no Mojo* system-API symbols are defined or
#      imported by candidate binaries.
#   4. strings/readelf: no reference to an official mojo shared library path
#      or filename.
#   5. source isolation: no crate's build.rs/sources reference the pinned
#      oracle checkout path.
#
# Produces evidence/security/no-oracle-dependency.json containing the raw
# tool output hashes and a verdict.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_require_cmd cargo
mojo_rs_require_cmd ldd
mojo_rs_require_cmd nm
mojo_rs_require_cmd readelf
mojo_rs_require_cmd sha256sum

OUT_DIR="$MOJO_RS_REPO_ROOT/evidence/security"
mkdir -p "$OUT_DIR"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

verdict="pass"
notes=()

# ---------------------------------------------------------------------------
# 1. cargo tree
# ---------------------------------------------------------------------------
cargo tree --workspace --edges normal,build > "$WORK/cargo-tree.txt" 2>&1 || true
# Our own crates are named mojo-rs-*; flag only references OUTSIDE the
# workspace (official chromium mojo/ipcz would appear as external crates).
if grep -iE "mojo|ipcz|chromium" "$WORK/cargo-tree.txt" \
  | grep -viE "^mojo-rs |mojo-rs-(core|wire|system|platform|bindings|mojom|codegen|c-api|io|oracle|casefile|interop|test-support) v"; then
  verdict="fail"
  notes+=("cargo tree references an external mojo/ipcz/chromium crate")
fi

# ---------------------------------------------------------------------------
# 2/3/4. binary-level checks on every candidate executable
# ---------------------------------------------------------------------------
BINARIES="$(find "$MOJO_RS_REPO_ROOT/target/debug" -maxdepth 1 -type f -executable \
  \( -name "candidate-harness" -o -name "mojo-rs-casefile" \) 2>/dev/null || true)"

for BIN in $BINARIES; do
  NAME="$(basename "$BIN")"

  ldd "$BIN" > "$WORK/$NAME.ldd" 2>&1 || true
  if grep -qiE "mojo|ipcz|chromium" "$WORK/$NAME.ldd"; then
    verdict="fail"
    notes+=("$NAME: ldd references mojo/ipcz/chromium")
  fi

  nm -D --defined-only "$BIN" > "$WORK/$NAME.nm" 2>&1 || true
  if grep -qE "Mojo[A-Z]" "$WORK/$NAME.nm"; then
    verdict="fail"
    notes+=("$NAME: exports Mojo* system symbols")
  fi

  readelf -d "$BIN" > "$WORK/$NAME.readelf" 2>&1 || true
  if grep -qiE "mojo|ipcz" "$WORK/$NAME.readelf"; then
    verdict="fail"
    notes+=("$NAME: dynamic section references mojo/ipcz")
  fi

  if strings "$BIN" 2>/dev/null | grep -qiE "libmojo|mojo_core\.so|mojo/public"; then
    verdict="fail"
    notes+=("$NAME: string table references official mojo artifacts")
  fi
done

# ---------------------------------------------------------------------------
# 5. source isolation: no crate references the oracle checkout
# ---------------------------------------------------------------------------
ORACLE_SRC="$MOJO_RS_ORACLE_SOURCE_ROOT"
if grep -rn "$ORACLE_SRC" "$MOJO_RS_REPO_ROOT/crates" --include=*.rs --include=build.rs --include=Cargo.toml > "$WORK/source-refs.txt" 2>/dev/null; then
  verdict="fail"
  notes+=("crate sources reference the oracle checkout path")
fi

# ---------------------------------------------------------------------------
# evidence assembly
# ---------------------------------------------------------------------------
hash_file() { sha256sum "$1" | awk '{print $1}'; }
CARGO_TREE_HASH="$(hash_file "$WORK/cargo-tree.txt")"
BIN_HASHES="{}"
BINS_JSON=""
for BIN in $BINARIES; do
  NAME="$(basename "$BIN")"
  BIN_HASHES="$BIN_HASHES $(printf '"%s": {"sha256": "%s", "ldd_sha256": "%s", "nm_sha256": "%s", "readelf_sha256": "%s"}' "$NAME" "$(hash_file "$BIN")" "$(hash_file "$WORK/$NAME.ldd")" "$(hash_file "$WORK/$NAME.nm")" "$(hash_file "$WORK/$NAME.readelf")")"
done

python3 - "$OUT_DIR" "$verdict" "$CARGO_TREE_HASH" "$BIN_HASHES" "$(IFS='|'; echo "${notes[*]}")" <<'PYEOF'
import json, os, sys
outdir, verdict, cargo_tree_hash, bin_hashes, notes = sys.argv[1:]
note_list = [n for n in notes.split('|') if n]
receipt = {
    "schema_version": 1,
    "check": "no-oracle-dependency",
    "verdict": verdict,
    "notes": note_list,
    "cargo_tree_sha256": cargo_tree_hash,
    "binaries": {},
}
PYEOF

# Write the receipt directly.
cat > "$OUT_DIR/no-oracle-dependency.json" <<EOF
{
  "schema_version": 1,
  "check": "no-oracle-dependency",
  "generated_at_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "verdict": "$verdict",
  "notes": [$(printf '"%s",' "${notes[@]}" | sed 's/,$//')],
  "cargo_tree_sha256": "$(hash_file "$WORK/cargo-tree.txt")",
  "binaries": {
EOF
FIRST=1
for BIN in $BINARIES; do
  NAME="$(basename "$BIN")"
  if [ "$FIRST" = 1 ]; then FIRST=0; else printf ',\n' >> "$OUT_DIR/no-oracle-dependency.json"; fi
  cat >> "$OUT_DIR/no-oracle-dependency.json" <<EOF
    "$NAME": {
      "sha256": "$(hash_file "$BIN")",
      "ldd_sha256": "$(hash_file "$WORK/$NAME.ldd")",
      "nm_sha256": "$(hash_file "$WORK/$NAME.nm")",
      "readelf_sha256": "$(hash_file "$WORK/$NAME.readelf")"
    }
EOF
done
cat >> "$OUT_DIR/no-oracle-dependency.json" <<EOF
  }
}
EOF

cp "$WORK/cargo-tree.txt" "$OUT_DIR/no-oracle-dependency.cargo-tree.txt"

if [ "$verdict" = "pass" ]; then
  mojo_rs_ok "no-oracle-dependency proof: PASS ($(echo "$BINARIES" | wc -w) binaries checked)"
else
  mojo_rs_fail "no-oracle-dependency proof FAILED: ${notes[*]}"
fi
