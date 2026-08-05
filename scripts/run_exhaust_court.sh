#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_exhaust_court.sh — the Phase 5 block-capacity exhaustion court: the
# broker's RouterLinkState pool exhausts, the broker shares a new 64-byte
# block buffer via AddBlockBuffer, and the native adopts it and resolves
# cross-buffer fragments — plus the exhausted-pool proxy transfer and the
# route migrations under exhaustion.
#
# Two runs, both through the wire-relay man-in-the-middle:
#   1. baseline:  official broker (invite-broker-exhaust) ⇄ official oracle
#                 acceptor (invite-acceptor-exhaust)
#   2. interop:   official broker ⇄ native Rust exhaust acceptor
#                 (exhaust-acceptor)
#
# The scenario (RunExhaustBroker / RunExhaustAcceptor): the broker transfers
# 1486 portals through the bootstrap pipe (transfer-b1 + transfer-2..1486);
# each pair is HELD on both ends, so the RouterLinkState blocks stay
# allocated. The primary buffer's 64-byte pool (1483 allocable blocks)
# exhausts mid-stream: the failing transfer falls back to the plain proxy
# path, the broker lobbies RequestBlockCapacity(64) (unconditional lobby),
# allocates a 64 KiB buffer locally, and shares it via AddBlockBuffer; the
# acceptor adopts it and the remaining transfers' link states come from the
# new buffer. The broker's IO thread flushes asynchronously, so the transfers
# arrive OUT OF ROUTE-SEQUENCE ORDER and migrate across sublinks (route
# bypasses); the receivers reorder via their sequenced queues.
#
# Equivalence relations (strongest first):
#   1. the broker's event stream must be BYTE-IDENTICAL in both runs;
#   2. the acceptor delivers all 1486 transfers with the correct payloads
#      (rseq-ordered) and observes peer closure — both acceptors exit 0;
#   3. the exhaustion expansion occurred in both runs (the broker sent at
#      least one AddBlockBuffer; the captures contain the transfer set).
#
# Documented residual: the exhaustion POINT differs between the runs (the
# baseline exhausted at transfer ~1330, the interop at ~750) and the interop
# can trigger a second AddBlockBuffer, because the native retains decayed
# RouterLinkStates (it does not free them when a decaying link finishes —
# the same free-timing boundary as the routing court's fragment-offset
# normalization). The broker's event stream — the primary equivalence — is
# unaffected.
#
# Evidence produced under evidence/exhaust/:
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

mojo_rs_log "building the wire relay, wire-dump, and the native exhaust acceptor"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin exhaust-acceptor
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
ACCEPTOR="$MOJO_RS_REPO_ROOT/target/debug/exhaust-acceptor"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$ACCEPTOR" ] || mojo_rs_fail "native exhaust acceptor missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/exhaust/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop/wire" "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$ACCEPTOR" "$OUT" "$STAMP" "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, re, socket, subprocess, sys

driver, relay, acceptor_bin, out, stamp, manifests = sys.argv[1:]

def run_pair(acceptor_cmd, tag):
    """Run the official exhaust broker against the given acceptor command."""
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
        [driver, "invite-broker-exhaust", str(b1.fileno()),
         os.path.join(out, tag, "broker.events")],
        pass_fds=(b1.fileno(),))

    b1.close(); b2.close(); a1.close(); a2.close()

    try:
        broker_rc = broker.wait(timeout=180)
        acceptor_rc = acceptor.wait(timeout=180)
        relay_rc = relay_proc.wait(timeout=120)
    except subprocess.TimeoutExpired as e:
        for p in (broker, acceptor, relay_proc):
            p.kill()
        raise SystemExit(f"{tag} pair timed out: {e}")
    return broker_rc, acceptor_rc, relay_rc

def h(p):
    with open(p, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()

def transfer_sequence(capture_path):
    """Decode the broker-to-acceptor wire and return the ordered route
    sequence (the set of per-route sequence numbers across the bootstrap
    route's sublink migrations; rseq 0 is transfer-b1, rseq k is
    transfer-{k+1}) plus the AddBlockBuffer count. The transfer payloads
    live in shared-memory fragments on the wire (the broker allocates them at
    put time), so the payload verification is the acceptors' job; the wire
    contributes the completeness of the route sequence."""
    proc = subprocess.run(
        ["cargo", "run", "-q", "-p", "mojo-rs-interop", "--bin", "wire-dump", "--", capture_path],
        capture_output=True, text=True, check=True)
    rseqs = []
    add_block_buffers = 0
    for line in proc.stdout.splitlines():
        if "id=14 (AddBlockBuffer)" in line:
            add_block_buffers += 1
        # The w1 (inline, on the transferred pipe's route) is not a transfer.
        if 'data="w1"' in line:
            continue
        r = re.search(r"rseq=(\d+)", line)
        if "id=20 (AcceptParcel)" in line and r:
            rseqs.append(int(r.group(1)))
    return sorted(rseqs), add_block_buffers

# Baseline: official broker ⇄ official oracle acceptor.
base_broker_rc, base_acc_rc, base_relay_rc = run_pair(
    [driver, "invite-acceptor-exhaust"], "baseline")

# Interop: official broker ⇄ native Rust exhaust acceptor.
int_broker_rc, int_acc_rc, int_relay_rc = run_pair(
    [acceptor_bin], "interop")

base_broker = os.path.join(out, "baseline", "broker.events")
int_broker = os.path.join(out, "interop", "broker.events")

# 1. The broker's event stream must be byte-identical in both runs.
broker_events_identical = (base_broker_rc == 0 and int_broker_rc == 0 and
                           open(base_broker, 'rb').read() ==
                           open(int_broker, 'rb').read())

# 2. Both acceptors deliver all 1486 transfers: the bootstrap route's
#    sequence (rseq 0..1485 across its sublink migrations) is complete in
#    both runs, and the acceptors verified the payloads (they exit 0 only if
#    every payload matched), and observe peer closure.
base_seq, base_addb = transfer_sequence(
    os.path.join(out, "baseline", "wire", "broker-to-acceptor.bin"))
int_seq, int_addb = transfer_sequence(
    os.path.join(out, "interop", "wire", "broker-to-acceptor.bin"))
expected_rseqs = list(range(1486))
delivery_ok = (base_seq == expected_rseqs and int_seq == expected_rseqs)

# 3. The exhaustion expansion occurred in both runs.
expansion_ok = (base_addb >= 1 and int_addb >= 1)

ok = (broker_events_identical and delivery_ok and expansion_ok and
      int_acc_rc == 0 and int_relay_rc == 0 and base_acc_rc == 0 and
      base_relay_rc == 0)

def art(rel):
    p = os.path.join(out, rel)
    return {"sha256": h(p), "bytes": os.path.getsize(p)}

receipt = {
    "schema_version": 1,
    "court": "exhaust",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_broker_rc, "acceptor_exit": base_acc_rc,
                 "relay_exit": base_relay_rc,
                 "add_block_buffers": base_addb},
    "interop": {"broker_exit": int_broker_rc, "acceptor_exit": int_acc_rc,
                "relay_exit": int_relay_rc,
                "add_block_buffers": int_addb},
    "broker_events_identical": broker_events_identical,
    "transfers_delivered_in_order": delivery_ok,
    "expansion_occurred": expansion_ok,
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
        "native_exhaust_acceptor_binary": h(acceptor_bin),
    },
}
mout = os.path.join(manifests, f"exhaust-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"exhaust court FAILED: broker-events-identical={broker_events_identical} "
        f"delivery={delivery_ok} expansion={expansion_ok} "
        f"baseline=({base_broker_rc},{base_acc_rc},{base_relay_rc},addb={base_addb}) "
        f"interop=({int_broker_rc},{int_acc_rc},{int_relay_rc},addb={int_addb})")
print(f"exhaust court PASS: broker event streams byte-identical; "
      f"all 1486 transfers delivered in order; expansion occurred "
      f"(baseline addb={base_addb}, interop addb={int_addb}); "
      f"broker={int_broker_rc} native-exhaust-acceptor={int_acc_rc}")
PYEOF
