#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_bypass_court.sh — the Phase 5 acceptor-initiated block-capacity
# exhaustion court: the ACCEPTOR's `SerializeNewRouterWithLocalPeer` transfers
# exhaust its own shared 64-byte RouterLinkState pool; the unconditional
# `TryAllocateRouterLinkState` lobby sends `RequestMemory` to the broker; the
# broker's `ProvideMemory` is adopted by the acceptor, which shares the new
# buffer back via `AddBlockBuffer` (the SEND side of the expansion round
# trip) and resolves the remaining transfers' link states from it.
#
# Two runs, both through the wire-relay man-in-the-middle:
#   1. baseline:  official broker (invite-broker-bypass) ⇄ official oracle
#                 acceptor (invite-acceptor-bypass)
#   2. interop:   official broker ⇄ native Rust bypass acceptor
#                 (bypass-acceptor)
#
# The scenario (RunBypassBroker / RunBypassAcceptor): after the routing-court
# prelude (the broker transfers b1 and writes w1), the ACCEPTOR creates 1520
# fresh local pairs and transfers one end of each through the bootstrap pipe;
# each pair is HELD on both ends, so the RouterLinkState blocks stay
# allocated. The shared 64-byte pool exhausts mid-loop: the failing transfer
# falls back to the plain proxy path, the acceptor lobbies
# RequestBlockCapacity(64) (the unconditional TryAllocateRouterLinkState
# lobby), sends `RequestMemory` to the broker (this node is the allocation
# delegate); the broker allocates a 64 KiB buffer and replies `ProvideMemory`;
# the acceptor shares it via `AddBlockBuffer` and registers it; the remaining
# transfers' link states come from the new buffer. The acceptor then sends a
# `sync` marker; the broker verifies every transfer payload and closes the
# bootstrap pipe; the acceptor observes peer closure.
#
# Equivalence relations (strongest first):
#   1. the broker's event stream must be BYTE-IDENTICAL in both runs;
#   2. the broker receives all 1520 transfers with the correct payloads plus
#      the sync marker, and both acceptors exit 0 (the native additionally
#      verifies an extra block buffer was registered);
#   3. the `RequestMemory` -> `ProvideMemory` -> `AddBlockBuffer` round trip
#      is visible on the acceptor→broker wire in BOTH runs (the SEND side
#      being sealed by the interop run).
#
# Documented residual: the exhaustion POINT in the transfer loop differs
# between the runs (the oracle's concurrent IO thread frees the transfer
# payload fragments while the native's single-threaded loop does not wait for
# the peer's frees), so the wire position of the `RequestMemory` and the
# exact fragment offsets differ. The broker's event stream — the primary
# equivalence — is unaffected.
#
# Evidence produced under evidence/bypass/:
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

mojo_rs_log "building the wire relay, wire-dump, and the native bypass acceptor"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin bypass-acceptor
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
ACCEPTOR="$MOJO_RS_REPO_ROOT/target/debug/bypass-acceptor"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$ACCEPTOR" ] || mojo_rs_fail "native bypass acceptor missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%S%3NZ)"
OUT="$EVIDENCE_ROOT/bypass/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop/wire" "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$ACCEPTOR" "$OUT" "$STAMP" "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, re, socket, subprocess, sys

driver, relay, acceptor_bin, out, stamp, manifests = sys.argv[1:]

def run_pair(acceptor_cmd, tag):
    """Run the official bypass broker against the given acceptor command."""
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
        [driver, "invite-broker-bypass", str(b1.fileno()),
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

def wire_send_side(capture_path):
    """Decode the acceptor->broker wire: count the portal-transfer parcels,
    and detect the RequestMemory and AddBlockBuffer messages (the SEND side
    of the expansion round trip)."""
    proc = subprocess.run(
        ["cargo", "run", "-q", "-p", "mojo-rs-interop", "--bin", "wire-dump", "--", capture_path],
        capture_output=True, text=True, check=True)
    transfers = 0
    request_memory = 0
    provide_memory = 0
    add_block_buffers = 0
    for line in proc.stdout.splitlines():
        if "id=64 (RequestMemory)" in line:
            request_memory += 1
        if "id=65 (ProvideMemory)" in line:
            provide_memory += 1
        if "id=14 (AddBlockBuffer)" in line:
            add_block_buffers += 1
        # A transfer parcel carries a serialized router descriptor.
        if "id=20 (AcceptParcel)" in line and "new_routers=[{new=" in line:
            transfers += 1
    return transfers, request_memory, provide_memory, add_block_buffers

# Baseline: official broker ⇄ official oracle acceptor.
base_broker_rc, base_acc_rc, base_relay_rc = run_pair(
    [driver, "invite-acceptor-bypass"], "baseline")

# Interop: official broker ⇄ native Rust bypass acceptor.
int_broker_rc, int_acc_rc, int_relay_rc = run_pair(
    [acceptor_bin], "interop")

base_broker = os.path.join(out, "baseline", "broker.events")
int_broker = os.path.join(out, "interop", "broker.events")

# 1. The broker's event stream must be byte-identical in both runs.
broker_events_identical = (base_broker_rc == 0 and int_broker_rc == 0 and
                           open(base_broker, 'rb').read() ==
                           open(int_broker, 'rb').read())

# 2. The wire carries the full transfer set in both runs, and the send-side
#    expansion round trip (RequestMemory / AddBlockBuffer) is present in
#    both. The broker itself verifies every transfer payload and the sync
#    marker (it exits 0 only if all matched).
base_xfers, base_req, base_prov, base_addb = wire_send_side(
    os.path.join(out, "baseline", "wire", "acceptor-to-broker.bin"))
int_xfers, int_req, int_prov, int_addb = wire_send_side(
    os.path.join(out, "interop", "wire", "acceptor-to-broker.bin"))
delivery_ok = (base_xfers == 1520 and int_xfers == 1520)
expansion_ok = (base_req >= 1 and base_addb >= 1 and
                int_req >= 1 and int_addb >= 1)

ok = (broker_events_identical and delivery_ok and expansion_ok and
      int_acc_rc == 0 and int_relay_rc == 0 and base_acc_rc == 0 and
      base_relay_rc == 0)

def art(rel):
    p = os.path.join(out, rel)
    return {"sha256": h(p), "bytes": os.path.getsize(p)}

receipt = {
    "schema_version": 1,
    "court": "bypass",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_broker_rc, "acceptor_exit": base_acc_rc,
                 "relay_exit": base_relay_rc,
                 "transfers": base_xfers, "request_memory": base_req,
                 "provide_memory": base_prov, "add_block_buffers": base_addb},
    "interop": {"broker_exit": int_broker_rc, "acceptor_exit": int_acc_rc,
                "relay_exit": int_relay_rc,
                "transfers": int_xfers, "request_memory": int_req,
                "provide_memory": int_prov, "add_block_buffers": int_addb},
    "broker_events_identical": broker_events_identical,
    "transfers_on_wire": delivery_ok,
    "expansion_send_side_seen": expansion_ok,
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
        "native_bypass_acceptor_binary": h(acceptor_bin),
    },
}
mout = os.path.join(manifests, f"bypass-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"bypass court FAILED: broker-events-identical={broker_events_identical} "
        f"delivery={delivery_ok} expansion={expansion_ok} "
        f"baseline=({base_broker_rc},{base_acc_rc},{base_relay_rc},"
        f"req={base_req},addb={base_addb},xfers={base_xfers}) "
        f"interop=({int_broker_rc},{int_acc_rc},{int_relay_rc},"
        f"req={int_req},addb={int_addb},xfers={int_xfers})")
print(f"bypass court PASS: broker event streams byte-identical; "
      f"1520 transfers on the wire in both runs; RequestMemory->ProvideMemory->"
      f"AddBlockBuffer send side sealed (baseline req={base_req},addb={base_addb}; "
      f"interop req={int_req},addb={int_addb}); "
      f"broker={int_broker_rc} native-bypass-acceptor={int_acc_rc}")
PYEOF
