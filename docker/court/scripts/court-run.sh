#!/usr/bin/env bash
# court-run.sh — the mojo-rs forensic parity pipeline, executed INSIDE the
# court container.
#
# Bind mounts:
#   <repo>                                   -> /repo (ro)
#   <docker root>/oracle-source              -> /work/oracle-source (ro: pinned, patched)
#   <docker root>/work/oracle-build          -> /work/oracle-build (persistent oracle build)
#   <docker root>/work/toolchain             -> /work/toolchain (rustup+cargo homes)
#   <docker root>/work/candidate-target      -> /work/target (cargo target dir)
#   mojo-rs-court-evidence (named volume)    -> /work/evidence (evidence out)
#
# One invocation = ONE full pass: verify -> oracle build -> baseline -> candidate
# build -> candidate phase (oracle isolated) -> classify -> no-delegation proof.
set -euo pipefail

export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC0 SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1785863982}"
export PATH="/opt/depot_tools:$PATH"
export RUSTUP_HOME=/work/toolchain/rustup CARGO_HOME=/work/toolchain/cargo
export CARGO_TARGET_DIR=/work/target

REPO="${MOJO_RS_REPO_ROOT:-/repo}"
SRC=/work/oracle-source/src
ORACLE_OUT=/work/oracle-build/out/Oracle
EVIDENCE=/work/evidence
RUN_ID="${MOJO_RS_RUN_ID:-court}"
CHROMIUM_COMMIT="${CHROMIUM_COMMIT:-bfa3579138998e2fbb981725570fa588c5b6f8cd}"
CHROMIUM_TAG="${CHROMIUM_TAG:-151.0.7922.105}"
RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-1.96.0}"

EVDIR="$EVIDENCE/$RUN_ID"
mkdir -p "$EVDIR"/{oracle,candidate,abi,no-delegation,failures}

log() { printf '\n=== [court] %s ===\n' "$*"; }
fail() { echo "FATAL: $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1. pinned source verification
# ---------------------------------------------------------------------------
log "pinned source verification"
[ -d "$SRC" ] || fail "pinned source missing at $SRC"
ACTUAL="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo missing)"
[ "$ACTUAL" = "$CHROMIUM_COMMIT" ] || fail "source revision mismatch: $ACTUAL != $CHROMIUM_COMMIT"
[ -f "$SRC/oracle_driver/BUILD.gn" ] || fail "oracle driver patch not applied to the checkout"
[ -f "$SRC/oracle_driver/driver.cc" ] || fail "oracle driver sources missing in the checkout"
log "pinned source verified: $ACTUAL (tag $CHROMIUM_TAG)"

# ---------------------------------------------------------------------------
# 2. rust toolchain (rustup, pinned)
# ---------------------------------------------------------------------------
log "rust toolchain"
if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
  mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal \
    --default-toolchain "$RUST_TOOLCHAIN" --no-modify-path
fi
"$CARGO_HOME/bin/rustc" --version
"$CARGO_HOME/bin/cargo" --version

# ---------------------------------------------------------------------------
# 3. oracle build (incremental, persistent out dir)
# ---------------------------------------------------------------------------
log "oracle build"
mkdir -p "$ORACLE_OUT"
cd "$SRC"
if [ ! -f "$ORACLE_OUT/args.gn" ]; then
  gn gen "$ORACLE_OUT" --args='
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
  gn gen "$ORACLE_OUT"
fi
ninja -C "$ORACLE_OUT" -j"$(nproc)" mojo_rs_oracle_driver
DRIVER="$ORACLE_OUT/mojo_rs_oracle_driver"
[ -x "$DRIVER" ] || fail "oracle driver missing after build"
log "oracle driver built: $(sha256sum "$DRIVER" | cut -d' ' -f1)"

# ---------------------------------------------------------------------------
# 4. candidate build
# ---------------------------------------------------------------------------
log "candidate build"
cd "$REPO"
cargo build --release --workspace
CANDIDATE_HARNESS="$CARGO_TARGET_DIR/release/mojo-rs-casefile"
[ -x "$CANDIDATE_HARNESS" ] || fail "candidate harness (mojo-rs-casefile) missing after build"

# ---------------------------------------------------------------------------
# 5. oracle baseline phase
# ---------------------------------------------------------------------------
log "oracle baseline phase"
CASES=()
while IFS= read -r -d '' f; do CASES+=("$f"); done \
  < <(find "$REPO/courts" -name '*.casefile.json' -print0 2>/dev/null | sort -z)
[ "${#CASES[@]}" -gt 0 ] || fail "no casefiles found under $REPO/courts"

BASELINE_PASS=0
BASELINE_FAIL=0
for cf in "${CASES[@]}"; do
  id="$(basename "$cf" .casefile.json)"
  if "$DRIVER" baseline "$cf" "$EVDIR/oracle/$id.events.jsonl" \
     2> "$EVDIR/failures/oracle-$id.log"; then
    BASELINE_PASS=$((BASELINE_PASS+1))
  else
    BASELINE_FAIL=$((BASELINE_FAIL+1))
    log "oracle baseline FAILED: $id"
  fi
done
[ "$BASELINE_FAIL" = 0 ] || fail "oracle baseline: $BASELINE_FAIL failures"

# ---------------------------------------------------------------------------
# 6. candidate phase — the oracle is physically removed
# ---------------------------------------------------------------------------
log "candidate phase (oracle isolated)"
mv "$ORACLE_OUT" "$ORACLE_OUT.disabled"
ISOLATION_FAIL=0
if command -v mojo_rs_oracle_driver >/dev/null 2>&1; then
  echo "oracle driver still on PATH" >&2; ISOLATION_FAIL=1
fi
if [ -e "$ORACLE_OUT/mojo_rs_oracle_driver" ]; then
  echo "oracle driver still present" >&2; ISOLATION_FAIL=1
fi
if find / -maxdepth 4 -name 'libmojo*' 2>/dev/null | grep -q .; then
  echo "libmojo artifacts visible during the candidate phase" >&2; ISOLATION_FAIL=1
fi
[ "$ISOLATION_FAIL" = 0 ] || fail "candidate phase isolation check failed"

CAND_PASS=0
CAND_FAIL=0
for cf in "${CASES[@]}"; do
  id="$(basename "$cf" .casefile.json)"
  if "$CANDIDATE_HARNESS" baseline "$cf" "$EVDIR/candidate/$id.events.jsonl" \
     2> "$EVDIR/failures/candidate-$id.log"; then
    CAND_PASS=$((CAND_PASS+1))
  else
    CAND_FAIL=$((CAND_FAIL+1))
    log "candidate FAILED: $id"
  fi
done
mv "$ORACLE_OUT.disabled" "$ORACLE_OUT"
[ "$CAND_FAIL" = 0 ] || fail "candidate phase: $CAND_FAIL failures"

# ---------------------------------------------------------------------------
# 7. classification (differential comparison with normalizers)
# ---------------------------------------------------------------------------
log "classification"
for cf in "${CASES[@]}"; do
  id="$(basename "$cf" .casefile.json)"
  "$CANDIDATE_HARNESS" compare \
    --casefile "$cf" \
    --oracle "$EVDIR/oracle/$id.events.jsonl" \
    --candidate "$EVDIR/candidate/$id.events.jsonl" \
    --out "$EVDIR/$id.comparison.json" \
    || { log "comparison FAILED for $id"; cp "$EVDIR/$id.comparison.json" "$EVDIR/failures/" 2>/dev/null || true; }
done
COMPARE_FAILS="$(find "$EVDIR" -name '*.comparison.json' -exec grep -L '"status": "pass"' {} + 2>/dev/null | wc -l)"
COMPARE_TOTAL="$(find "$EVDIR" -name '*.comparison.json' | wc -l)"
[ "$COMPARE_FAILS" = 0 ] || fail "classification: $COMPARE_FAILS/$COMPARE_TOTAL comparisons not pass"
log "classification: $COMPARE_TOTAL comparisons, all pass"

# ---------------------------------------------------------------------------
# 8. no-delegation proof (mechanical, recorded)
# ---------------------------------------------------------------------------
log "no-delegation proof"
NO_DELEGATION="$EVDIR/no-delegation"
mkdir -p "$NO_DELEGATION"
cd "$REPO"
# a) dynamic deps of every candidate artifact contain no mojo/chromium libraries
for lib in $(find "$CARGO_TARGET_DIR/release" -maxdepth 1 -type f \( -name '*.so' -o -name 'mojo-rs*' \) 2>/dev/null); do
  base="$(basename "$lib")"
  LDD_HITS="$(ldd "$lib" 2>/dev/null | grep -ciE 'mojo|chromium|libc\+\+|ipcz' || true)"
  READELF_HITS="$(readelf -d "$lib" 2>/dev/null | grep -ciE 'mojo|chromium|ipcz' || true)"
  echo "{\"artifact\":\"$base\",\"ldd_hits\":$LDD_HITS,\"readelf_hits\":$READELF_HITS,\"sha256\":\"$(sha256sum "$lib" | cut -d' ' -f1)\"}" \
    >> "$NO_DELEGATION/scan.jsonl"
done
# b) symbol scan of the exported C ABI library: Mojo* symbols must be OUR exports,
#    not references to mojo_core (checked by host-side script against the ABI manifest)
# c) runtime trace: LD_DEBUG=libs on the harness must not load any official mojo lib
LD_DEBUG=libs "$CANDIDATE_HARNESS" --self-check > "$NO_DELEGATION/ld-debug.log" 2>&1 || true
grep -icE 'mojo_core|libmojo' "$NO_DELEGATION/ld-debug.log" || true

# ---------------------------------------------------------------------------
# 9. receipts + environment facts
# ---------------------------------------------------------------------------
log "receipts"
cat > "$EVDIR/run.json" <<EOF
{
  "schema_version": 1,
  "run_id": "$RUN_ID",
  "chromium_tag": "$CHROMIUM_TAG",
  "chromium_commit": "$CHROMIUM_COMMIT",
  "rust_toolchain": "$RUST_TOOLCHAIN",
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "case_count": ${#CASES[@]},
  "baseline_pass": $BASELINE_PASS,
  "baseline_fail": $BASELINE_FAIL,
  "candidate_pass": $CAND_PASS,
  "candidate_fail": $CAND_FAIL,
  "comparison_total": $COMPARE_TOTAL,
  "comparison_fail": $COMPARE_FAILS,
  "oracle_driver_sha256": "$(sha256sum "$DRIVER" | cut -d' ' -f1)"
}
EOF
uname -a
sed -n '1,2p' /etc/os-release
ldd --version | sed -n '1p'

log "court pass complete — evidence in $EVDIR"
find "$EVDIR" -type f | sort
