#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_routing_court.sh — the Phase 5 routing seal gate: portal transfer in
# both directions plus proxy bypass completion against the official broker.
#
# Two runs, both through the wire-relay man-in-the-middle:
#   1. baseline:  official broker (invite-broker-routing) ⇄ official oracle
#                 acceptor (invite-acceptor-routing)
#   2. interop:   official broker ⇄ native Rust routing acceptor
#                 (routing-acceptor)
#
# The scenario (RunRoutingBroker / RunRoutingAcceptor):
#   1. the broker creates a message pipe (A, B) and sends B through the
#      bootstrap pipe (the WithLocalPeer serialization path);
#   2. the broker writes "w1" on A -> routed over the wire to the acceptor's
#      B';
#   3. the acceptor writes "r1" on B' -> routed over the wire back to A;
#   4. the acceptor sends B' back through the bootstrap pipe (the proxy
#      serialization path); the broker completes the bypass locally and
#      extracts B'';
#   5. the broker writes "w2" on A -> delivered locally to B'';
#   6. the broker closes A, B'', and the bootstrap pipe (RouteClosed
#      propagation).
#
# The broker's event stream must be IDENTICAL in both runs — the broker
# cannot distinguish the native routing acceptor from the official one. The
# Rust acceptor must additionally verify every payload and exit 0.
#
# Evidence produced under evidence/routing/:
#   baseline-broker.events        interop-broker.events
#   baseline-acceptor.events      interop-acceptor.events (oracle)
#   baseline-<dir>/wire/*.bin     interop-<dir>/wire/*.bin
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

mojo_rs_log "building the wire relay and the native routing acceptor"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin routing-acceptor
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
ACCEPTOR="$MOJO_RS_REPO_ROOT/target/debug/routing-acceptor"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$ACCEPTOR" ] || mojo_rs_fail "native routing acceptor missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/routing/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop/wire" "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$ACCEPTOR" "$OUT" "$STAMP" "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, socket, subprocess, sys

driver, relay, acceptor_bin, out, stamp, manifests = sys.argv[1:]

def run_pair(acceptor_cmd, tag):
    """Run the official routing broker against the given acceptor command."""
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
        [driver, "invite-broker-routing", str(b1.fileno()),
         os.path.join(out, tag, "broker.events")],
        pass_fds=(b1.fileno(),))

    b1.close(); b2.close(); a1.close(); a2.close()

    try:
        broker_rc = broker.wait(timeout=60)
        acceptor_rc = acceptor.wait(timeout=60)
        relay_rc = relay_proc.wait(timeout=30)
    except subprocess.TimeoutExpired as e:
        for p in (broker, acceptor, relay_proc):
            p.kill()
        raise SystemExit(f"{tag} pair timed out: {e}")
    return broker_rc, acceptor_rc, relay_rc

def h(p):
    with open(p, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()

# Baseline: official broker ⇄ official oracle acceptor.
base_broker_rc, base_acc_rc, base_relay_rc = run_pair(
    [driver, "invite-acceptor-routing"], "baseline")

# Interop: official broker ⇄ native Rust routing acceptor.
int_broker_rc, int_acc_rc, int_relay_rc = run_pair(
    [acceptor_bin], "interop")

base_broker = os.path.join(out, "baseline", "broker.events")
int_broker = os.path.join(out, "interop", "broker.events")

# The broker's event stream must be byte-identical in both runs.
broker_events_identical = (base_broker_rc == 0 and int_broker_rc == 0 and
                           open(base_broker, 'rb').read() ==
                           open(int_broker, 'rb').read())

ok = (broker_events_identical and int_acc_rc == 0 and int_relay_rc == 0 and
      base_acc_rc == 0 and base_relay_rc == 0)

def art(rel):
    p = os.path.join(out, rel)
    return {"sha256": h(p), "bytes": os.path.getsize(p)}

receipt = {
    "schema_version": 1,
    "court": "routing",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_broker_rc, "acceptor_exit": base_acc_rc,
                 "relay_exit": base_relay_rc},
    "interop": {"broker_exit": int_broker_rc, "acceptor_exit": int_acc_rc,
                "relay_exit": int_relay_rc},
    "broker_events_identical": broker_events_identical,
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
        "native_routing_acceptor_binary": h(acceptor_bin),
    },
}
mout = os.path.join(manifests, f"routing-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"routing court FAILED: broker-events-identical={broker_events_identical} "
        f"baseline=({base_broker_rc},{base_acc_rc},{base_relay_rc}) "
        f"interop=({int_broker_rc},{int_acc_rc},{int_relay_rc})")
print(f"routing court PASS: broker event streams byte-identical; "
      f"broker={int_broker_rc} native-routing-acceptor={int_acc_rc}")
PYEOF
