#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_memory_court.sh — the Phase 5 memory court: parcel-fragment allocation
# and free-list reuse against the official broker, plus the sealed
# RequestMemory/ProvideMemory wire machinery readiness.
#
# Two runs, both through the wire-relay man-in-the-middle:
#   1. baseline:  official broker (invite-broker-memory) ⇄ official oracle
#                 acceptor (invite-acceptor-memory)
#   2. interop:   official broker ⇄ native Rust memory acceptor
#                 (memory-acceptor)
#
# The scenario (RunMemoryBroker / RunMemoryAcceptor):
#   1. the broker transfers B through the bootstrap pipe and writes w1 on A;
#   2. the acceptor sends m0..m8 (9 x 200-byte parcels) on B' — the primary
#      buffer's 256-byte block pool holds exactly 8 allocable blocks, so m8's
#      fragment allocation fails and m8 travels inline (the pinned mojo
#      embedder sets IPCZ_MEMORY_FIXED_PARCEL_CAPACITY, disabling parcel-data
#      expansion);
#   3. the acceptor sends a "sync" marker; the broker reads m0..m8 from A only
#      after receiving it, freeing the 256-byte blocks (LIFO free-list);
#   4. the acceptor sends the transfer-back (B' + handle) through the
#      bootstrap; the broker does a w2 round trip on A/B'' and sends w3;
#   5. the acceptor sends m9 and m10 on the bootstrap — deterministically
#      fragment-backed from the FREED primary blocks (m9 reuses block 8, m10
#      reuses block 7), sealing the free-list reuse semantics;
#   6. the broker reads m9/m10, then closes A, B'', and the bootstrap pipe.
#
# Equivalence relations (strongest first):
#   1. the broker's event stream must be BYTE-IDENTICAL in both runs;
#   2. the acceptor→broker wire must be IDENTICAL modulo node names and
#      per-link sequence numbers (decoded by wire-dump, normalized);
#   3. all four processes exit 0.
#
# Evidence produced under evidence/memory/:
#   baseline/{broker,acceptor}.events, baseline/wire/*.bin
#   interop/{broker,acceptor}.events,  interop/wire/*.bin
#   manifest-<stamp>.json
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_require_cmd cargo
mojo_rs_require_cmd python3

ORACLE_DRIVER="${MOJO_RS_ORACLE_DRIVER:-$MOJO_RS_WORK_ROOT/oracle-build/out/Oracle/mojo_rs_oracle_driver}"
[ -x "$ORACLE_DRIVER" ] || mojo_rs_fail "oracle driver missing: $ORACLE_DRIVER"

mojo_rs_log "building the wire relay, wire-dump, and the native memory acceptor"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin memory-acceptor
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
ACCEPTOR="$MOJO_RS_REPO_ROOT/target/debug/memory-acceptor"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$ACCEPTOR" ] || mojo_rs_fail "native memory acceptor missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/memory/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop/wire" "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$ACCEPTOR" "$OUT" "$STAMP" "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, re, socket, subprocess, sys

driver, relay, acceptor_bin, out, stamp, manifests = sys.argv[1:]

def run_pair(acceptor_cmd, tag):
    """Run the official memory broker against the given acceptor command."""
    b1, b2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    a1, a2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    for f in (b1, b2, a1, a2):
        os.set_inheritable(f.fileno(), True)

    relay_proc = subprocess.Popen(
        [relay, str(b2.fileno()), str(a1.fileno()),
         os.path.join(out, tag, "wire", "broker-to-acceptor.bin"),
         os.path.join(out, tag, "wire", "acceptor-to-broker.bin")],
        pass_fds=(b2.fileno(), a1.fileno()))

    acceptor = subprocess.Popen(
        acceptor_cmd + [str(a2.fileno()), os.path.join(out, tag, "acceptor.events")],
        pass_fds=(a2.fileno(),))
    broker = subprocess.Popen(
        [driver, "invite-broker-memory", str(b1.fileno()),
         os.path.join(out, tag, "broker.events")],
        pass_fds=(b1.fileno(),))

    b1.close(); b2.close(); a1.close(); a2.close()

    try:
        broker_rc = broker.wait(timeout=120)
        acceptor_rc = acceptor.wait(timeout=120)
        relay_rc = relay_proc.wait(timeout=60)
    except subprocess.TimeoutExpired as e:
        for p in (broker, acceptor, relay_proc):
            p.kill()
        raise SystemExit(f"{tag} pair timed out: {e}")
    return broker_rc, acceptor_rc, relay_rc

def h(p):
    with open(p, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()

def wire_inventory(capture_path):
    """Decode a wire capture with wire-dump and normalize the per-run fields:
    node names (32 hex chars) and per-link sequence numbers."""
    proc = subprocess.run(
        ["cargo", "run", "-q", "-p", "mojo-rs-interop", "--bin", "wire-dump", "--", capture_path],
        capture_output=True, text=True, check=True)
    lines = []
    for line in proc.stdout.splitlines():
        line = re.sub(r"[0-9a-f]{32}", "NODENAME", line)
        line = re.sub(r"seq=\d+", "seq=N", line)
        lines.append(line)
    return lines

# Baseline: official broker ⇄ official oracle acceptor.
base_broker_rc, base_acc_rc, base_relay_rc = run_pair(
    [driver, "invite-acceptor-memory"], "baseline")

# Interop: official broker ⇄ native Rust memory acceptor.
int_broker_rc, int_acc_rc, int_relay_rc = run_pair(
    [acceptor_bin], "interop")

base_broker = os.path.join(out, "baseline", "broker.events")
int_broker = os.path.join(out, "interop", "broker.events")

# 1. The broker's event stream must be byte-identical in both runs.
broker_events_identical = (base_broker_rc == 0 and int_broker_rc == 0 and
                           open(base_broker, 'rb').read() ==
                           open(int_broker, 'rb').read())

# 2. The acceptor→broker wire must match modulo node names and per-link
# sequence numbers (the native's fragment allocation and free-list reuse are
# byte-observable here).
try:
    base_a2b = wire_inventory(os.path.join(out, "baseline", "wire", "acceptor-to-broker.bin"))
    int_a2b = wire_inventory(os.path.join(out, "interop", "wire", "acceptor-to-broker.bin"))
    wire_identical = base_a2b == int_a2b
    if not wire_identical:
        for i, (x, y) in enumerate(zip(base_a2b, int_a2b)):
            if x != y:
                print(f"wire divergence at line {i}:\n  baseline: {x}\n  interop:  {y}")
                break
except subprocess.CalledProcessError:
    wire_identical = False

ok = (broker_events_identical and wire_identical and int_acc_rc == 0 and
      int_relay_rc == 0 and base_acc_rc == 0 and base_relay_rc == 0)

def art(rel):
    p = os.path.join(out, rel)
    return {"sha256": h(p), "bytes": os.path.getsize(p)}

receipt = {
    "schema_version": 1,
    "court": "memory",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_broker_rc, "acceptor_exit": base_acc_rc,
                 "relay_exit": base_relay_rc},
    "interop": {"broker_exit": int_broker_rc, "acceptor_exit": int_acc_rc,
                "relay_exit": int_relay_rc},
    "broker_events_identical": broker_events_identical,
    "acceptor_to_broker_wire_identical": wire_identical,
    "artifacts": {
        "baseline/broker.events": art("baseline/broker.events"),
        "baseline/acceptor.events": art("baseline/acceptor.events"),
        "interop/broker.events": art("interop/broker.events"),
        "interop/acceptor.events": art("interop/acceptor.events"),
        "baseline/wire/broker-to-acceptor.bin": art("baseline/wire/broker-to-acceptor.bin"),
        "baseline/wire/acceptor-to-broker.bin": art("baseline/wire/acceptor-to-broker.bin"),
        "interop/wire/broker-to-acceptor.bin": art("interop/wire/broker-to-acceptor.bin"),
        "interop/wire/acceptor-to-broker.bin": art("interop/wire/acceptor-to-broker.bin"),
    },
    "inputs": {
        "oracle_driver_binary": h(driver),
        "wire_relay_binary": h(relay),
        "native_memory_acceptor_binary": h(acceptor_bin),
    },
}
mout = os.path.join(manifests, f"memory-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"memory court FAILED: broker-events-identical={broker_events_identical} "
        f"wire-identical={wire_identical} "
        f"baseline=({base_broker_rc},{base_acc_rc},{base_relay_rc}) "
        f"interop=({int_broker_rc},{int_acc_rc},{int_relay_rc})")
print(f"memory court PASS: broker event streams byte-identical; "
      f"acceptor-to-broker wire identical (modulo node names); "
      f"broker={int_broker_rc} native-memory-acceptor={int_acc_rc}")
PYEOF
