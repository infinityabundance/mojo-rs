#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_court.sh — run a forensic differential court: oracle baseline, candidate
# baseline, comparison, and evidence assembly.
#
# Usage:
#   scripts/run_court.sh system                  # run the 'system' court
#   scripts/run_court.sh system --dry-run        # print what would run
#
# Environment overrides:
#   MOJO_RS_ORACLE_DRIVER   path to the built oracle driver binary
#                           (default: $MOJO_RS_WORK_ROOT/oracle-build/out/Oracle/
#                            mojo_rs_oracle_driver)
#   MOJO_RS_EVIDENCE_ROOT   evidence output root (default: $MOJO_RS_REPO_ROOT/
#                           evidence)
#
# The court manifest (courts/<court>/manifest.json) lists the cases. For every
# case the script:
#   1. runs the oracle driver baseline  -> evidence/oracle/<court>/<case>.events
#   2. runs the candidate harness       -> evidence/candidate/<court>/<case>.events
#   3. compares (mojo-rs-casefile)      -> evidence/diffs/<court>/<case>.json
#   4. verifies byte identity of the raw streams
# and writes a manifest under evidence/manifests/ recording input hashes
# (casefiles, oracle driver binary, candidate binaries) so the receipt can be
# invalidated automatically when any input changes.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_require_cmd cargo
mojo_rs_require_cmd sha256sum

COURT="${1:-}"
DRY_RUN=0
VERIFY=""
if [ "${1:-}" = "verify" ]; then
  VERIFY="${2:-}"
  [ -n "$VERIFY" ] || mojo_rs_fail "usage: scripts/run_court.sh verify <manifest.json>"
fi
[ "${2:-}" = "--dry-run" ] && DRY_RUN=1
[ -z "$COURT" ] && mojo_rs_fail "usage: scripts/run_court.sh <court> [--dry-run]"

# ---------------------------------------------------------------------------
# Receipt verification: recompute every input hash and every event hash
# recorded in an existing manifest; fail loudly if anything changed.
# ---------------------------------------------------------------------------
if [ -n "$VERIFY" ]; then
  [ -f "$VERIFY" ] || mojo_rs_fail "manifest not found: $VERIFY"
  python3 - "$VERIFY" "$MOJO_RS_REPO_ROOT" "$MOJO_RS_WORK_ROOT/oracle-build/out/Oracle/mojo_rs_oracle_driver" "$MOJO_RS_WORK_ROOT" <<'PYEOF'
import hashlib, json, os, sys
p, repo_root, oracle_driver, _work_root = sys.argv[1:]
m = json.load(open(p))
court = m["court"]
evroot = os.path.dirname(os.path.dirname(os.path.abspath(p)))
failures = []
for key, want in m["inputs"].items():
    if key.startswith("casefile:"):
        path = os.path.join(repo_root, "courts", court, key.split(":", 1)[1] + ".casefile.json")
    elif key == "oracle_driver_binary":
        path = oracle_driver
    elif key == "candidate_harness_binary":
        path = os.path.join(repo_root, "target/debug/candidate-harness")
    elif key == "casefile_cli_binary":
        path = os.path.join(repo_root, "target/debug/mojo-rs-casefile")
    else:
        continue
    if not os.path.exists(path):
        failures.append(f"{key}: missing {path}")
        continue
    got = hashlib.sha256(open(path, 'rb').read()).hexdigest()
    if got != want:
        failures.append(f"{key}: hash changed ({got} != {want})")
for case, res in m["cases"].items():
    for kind in ("oracle", "candidate"):
        path = os.path.join(evroot, kind, court, case + ".events")
        if not os.path.exists(path):
            failures.append(f"{case} {kind} events missing")
            continue
        got = hashlib.sha256(open(path, 'rb').read()).hexdigest()
        if got != res[f"{kind}_events_sha256"]:
            failures.append(f"{case} {kind} events hash changed")
if failures:
    print("RECEIPT INVALIDATED:")
    for f in failures:
        print("  - " + f)
    sys.exit(1)
print("receipt valid: all input and event hashes match")
PYEOF
  exit $?
fi

MANIFEST="$MOJO_RS_REPO_ROOT/courts/$COURT/manifest.json"
[ -f "$MANIFEST" ] || mojo_rs_fail "court manifest missing: $MANIFEST"

# Resolve binaries.
ORACLE_DRIVER="${MOJO_RS_ORACLE_DRIVER:-$MOJO_RS_WORK_ROOT/oracle-build/out/Oracle/mojo_rs_oracle_driver}"
[ -x "$ORACLE_DRIVER" ] || mojo_rs_fail "oracle driver missing: $ORACLE_DRIVER (build it with scripts/compose_project.sh run --rm oracle)"

CARGO_BIN="$MOJO_RS_REPO_ROOT/target/debug"
mojo_rs_log "building candidate binaries"
cargo build --quiet -p mojo-rs-casefile --bin mojo-rs-casefile
cargo build --quiet -p mojo-rs-interop --bin candidate-harness
CANDIDATE_HARNESS="$CARGO_BIN/candidate-harness"
CASEFILE_CLI="$CARGO_BIN/mojo-rs-casefile"
[ -x "$CANDIDATE_HARNESS" ] || mojo_rs_fail "candidate harness missing"
[ -x "$CASEFILE_CLI" ] || mojo_rs_fail "mojo-rs-casefile CLI missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_ORACLE="$EVIDENCE_ROOT/oracle/$COURT"
OUT_CANDIDATE="$EVIDENCE_ROOT/candidate/$COURT"
OUT_DIFFS="$EVIDENCE_ROOT/diffs/$COURT"
OUT_MANIFESTS="$EVIDENCE_ROOT/manifests"

mkdir -p "$OUT_ORACLE" "$OUT_CANDIDATE" "$OUT_DIFFS" "$OUT_MANIFESTS"

# Read the case list from the manifest (python3 for JSON).
mojo_rs_require_cmd python3
CASES="$(python3 -c "
import json, sys
d = json.load(open('$MANIFEST'))
for c in d['cases']:
    print(c)
")"

[ -n "$CASES" ] || mojo_rs_fail "court manifest has no cases"

PASS=0
FAIL=0
FAILED_CASES=""

for CASE in $CASES; do
  CASEFILE="$MOJO_RS_REPO_ROOT/courts/$COURT/$CASE.casefile.json"
  [ -f "$CASEFILE" ] || { echo "MISSING casefile: $CASEFILE"; FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES $CASE"; continue; }

  ORACLE_EVENTS="$OUT_ORACLE/$CASE.events"
  CANDIDATE_EVENTS="$OUT_CANDIDATE/$CASE.events"
  COMPARISON="$OUT_DIFFS/$CASE.json"

  if [ "$DRY_RUN" = 1 ]; then
    echo "  would run: $CASE (oracle + candidate + compare)"
    continue
  fi

  if ! "$ORACLE_DRIVER" baseline "$CASEFILE" "$ORACLE_EVENTS" >/dev/null 2>"$OUT_DIFFS/$CASE.oracle.stderr"; then
    echo "FAIL (oracle) $CASE"; FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES $CASE"; continue
  fi
  if ! "$CANDIDATE_HARNESS" baseline "$CASEFILE" "$CANDIDATE_EVENTS" >/dev/null 2>"$OUT_DIFFS/$CASE.candidate.stderr"; then
    echo "FAIL (candidate) $CASE"; FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES $CASE"; continue
  fi
  if ! "$CASEFILE_CLI" compare "$CASEFILE" "$ORACLE_EVENTS" "$CANDIDATE_EVENTS" >"$COMPARISON" 2>"$OUT_DIFFS/$CASE.compare.stderr"; then
    echo "FAIL (compare) $CASE"; FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES $CASE"; continue
  fi

  # Byte identity: the raw streams must be identical (both emit sorted keys).
  if diff -q "$ORACLE_EVENTS" "$CANDIDATE_EVENTS" >/dev/null 2>&1; then
    BYTE="byte-identical"
  else
    BYTE="byte-diff"
  fi

  STATUS="$(python3 -c "import json; print(json.load(open('$COMPARISON'))['status'])")"
  if [ "$STATUS" = "pass" ] && [ "$BYTE" = "byte-identical" ]; then
    echo "PASS $CASE ($BYTE)"
    PASS=$((PASS + 1))
  else
    echo "FAIL $CASE status=$STATUS $BYTE"
    FAIL=$((FAIL + 1))
    FAILED_CASES="$FAILED_CASES $CASE"
  fi
done

if [ "$DRY_RUN" = 1 ]; then
  exit 0
fi

# ---------------------------------------------------------------------------
# Input hashes for receipt invalidation, case results, and the manifest are
# computed in python below (single source of truth).
# ---------------------------------------------------------------------------
python3 - "$MANIFEST" "$COURT" "$ORACLE_DRIVER" "$CANDIDATE_HARNESS" "$CASEFILE_CLI" "$EVIDENCE_ROOT" "$OUT_MANIFESTS" "$STAMP" "$PASS" "$FAIL" "$FAILED_CASES" <<'PYEOF'
import hashlib, json, os, sys

manifest_path, court, oracle, candidate, casefile_cli, evroot, outman, stamp, npass, nfail, failed = sys.argv[1:]

def h(p):
    with open(p, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()

cases = json.load(open(manifest_path))['cases']
inputs = {}
for c in cases:
    inputs[f"casefile:{c}"] = h(os.path.join(os.path.dirname(manifest_path), f"{c}.casefile.json"))
inputs["oracle_driver_binary"] = h(oracle)
inputs["candidate_harness_binary"] = h(candidate)
inputs["casefile_cli_binary"] = h(casefile_cli)

case_results = {}
for c in cases:
    cmp = os.path.join(evroot, "diffs", court, f"{c}.json")
    if not os.path.exists(cmp):
        case_results[c] = {"status": "missing"}
        continue
    d = json.load(open(cmp))
    oe = os.path.join(evroot, "oracle", court, f"{c}.events")
    ce = os.path.join(evroot, "candidate", court, f"{c}.events")
    with open(oe, 'rb') as f: oeh = hashlib.sha256(f.read()).hexdigest()
    with open(ce, 'rb') as f: ceh = hashlib.sha256(f.read()).hexdigest()
    case_results[c] = {
        "status": d["status"],
        "oracle_events": d["oracle_events"],
        "candidate_events": d["candidate_events"],
        "residuals": len(d["residuals"]),
        "oracle_events_sha256": oeh,
        "candidate_events_sha256": ceh,
        "byte_identical": oeh == ceh,
        "comparison_sha256": h(cmp),
    }

manifest = {
    "schema_version": 1,
    "court": court,
    "generated_at_utc": stamp,
    "status": "pass" if int(nfail) == 0 else "fail",
    "passed": int(npass),
    "failed": int(nfail),
    "failed_cases": failed.strip().split() if failed.strip() else [],
    "inputs": inputs,
    "cases": case_results,
}
out = os.path.join(outman, f"{court}-{stamp}.json")
with open(out, 'w') as f:
    json.dump(manifest, f, indent=2, sort_keys=True)
print(out)
PYEOF

MANIFEST_OUT="$(python3 -c "
import glob, os
files = sorted(glob.glob('$OUT_MANIFESTS/$COURT-*.json'))
print(files[-1])
")"

mojo_rs_log "court '$COURT': $PASS passed, $FAIL failed"
mojo_rs_log "evidence manifest: $MANIFEST_OUT"
[ "$FAIL" = 0 ] || mojo_rs_fail "court '$COURT' has failures:${FAILED_CASES}"
mojo_rs_ok "court '$COURT' sealed"
