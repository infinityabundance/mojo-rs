#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_3node_court.sh — the Phase 5 multi-node referral court: broker + referrer
# A + referred B, with the referral transport captured by a man-in-the-middle
# wire relay.
#
# Two runs, both through wire relays:
#   1. baseline:  official broker ⇄ official A ⇄ official B
#   2. interop:   official broker ⇄ official A ⇄ native Rust B (3node-acceptor)
#
# Topology and transports:
#   broker ── relay1 ── A      (the broker↔A invitation-1 link)
#   broker ── relay2 ── B      (the referral transport: A's socket-b fd travels
#                               inside `ReferNonBroker` through relay1 to the
#                               broker, which connects to B through relay2)
#   broker ──(A↔B link)── B    (the referrer link, created by the broker via
#                               CreatePair; its endpoints travel inside
#                               `ConnectToReferredNonBroker` / `NonBrokerReferralAccepted`).
#
# The broker's and A's event streams must be IDENTICAL in both runs (the
# broker cannot distinguish the native B from the official one). The referral
# transport wire is compared structurally (decoded message sequences, node
# names normalized). The Rust B must additionally verify the round trip and
# exit 0.
#
# Evidence produced under evidence/3node/<stamp>/:
#   baseline/{broker,a,b}.events            interop/{broker,a,b}.events
#   baseline/wire/{broker-to-a,a-to-broker,broker-to-b,b-to-broker}.bin
#   interop/wire/...                        (same four captures)
#   evidence/manifests/3node-<stamp>.json
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

mojo_rs_log "building the wire relay, wire-dump, and the native 3node acceptor"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin wire-dump
cargo build --quiet -p mojo-rs-interop --bin 3node-acceptor
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
DUMP="$MOJO_RS_REPO_ROOT/target/debug/wire-dump"
B3="$MOJO_RS_REPO_ROOT/target/debug/3node-acceptor"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$DUMP" ] || mojo_rs_fail "wire-dump missing"
[ -x "$B3" ] || mojo_rs_fail "3node acceptor missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/3node/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop/wire" "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$DUMP" "$B3" "$OUT" "$STAMP" "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, socket, subprocess, sys, re

driver, relay, dump, b3, out, stamp, manifests = sys.argv[1:]

def run_three(b_cmd, tag):
    """Run broker + A (oracle) against the given B command."""
    # broker<->A transport: (broker_sock, relay1_a) and (relay1_b, a_broker_sock).
    broker_sock, relay1_a = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    relay1_b, a_broker_sock = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    # Referral transport (relay2): (broker_ref_sock, relay2_a) and
    # (relay2_b, b_sock). A's socket-b fd is broker_ref_sock; it travels
    # inside `ReferNonBroker` through relay1 to the broker, which connects to
    # B through relay2.
    broker_ref_sock, relay2_a = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    relay2_b, b_sock = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    for f in (broker_sock, relay1_a, relay1_b, a_broker_sock,
              broker_ref_sock, relay2_a, relay2_b, b_sock):
        os.set_inheritable(f.fileno(), True)

    w = os.path.join(out, tag, "wire")
    relay1 = subprocess.Popen(
        [relay, str(relay1_a.fileno()), str(relay1_b.fileno()),
         os.path.join(w, "broker-to-a.bin"), os.path.join(w, "a-to-broker.bin")],
        pass_fds=(relay1_a.fileno(), relay1_b.fileno()))
    relay2 = subprocess.Popen(
        [relay, str(relay2_a.fileno()), str(relay2_b.fileno()),
         os.path.join(w, "broker-to-b.bin"), os.path.join(w, "b-to-broker.bin")],
        pass_fds=(relay2_a.fileno(), relay2_b.fileno()))

    a = subprocess.Popen(
        [driver, "invite-node-a-3node", str(a_broker_sock.fileno()),
         str(broker_ref_sock.fileno()), os.path.join(out, tag, "a.events")],
        pass_fds=(a_broker_sock.fileno(), broker_ref_sock.fileno()))
    b = subprocess.Popen(
        b_cmd + [str(b_sock.fileno()), os.path.join(out, tag, "b.events")],
        pass_fds=(b_sock.fileno(),))
    broker = subprocess.Popen(
        [driver, "invite-broker-3node", str(broker_sock.fileno()),
         os.path.join(out, tag, "broker.events")],
        pass_fds=(broker_sock.fileno(),))

    broker_sock.close(); relay1_a.close(); relay1_b.close(); a_broker_sock.close()
    broker_ref_sock.close(); relay2_a.close(); relay2_b.close(); b_sock.close()

    try:
        broker_rc = broker.wait(timeout=60)
        a_rc = a.wait(timeout=60)
        b_rc = b.wait(timeout=60)
        r1_rc = relay1.wait(timeout=30)
        r2_rc = relay2.wait(timeout=30)
    except subprocess.TimeoutExpired as e:
        for p in (broker, a, b, relay1, relay2):
            p.kill()
        raise SystemExit(f"{tag} run timed out: {e}")
    return broker_rc, a_rc, b_rc, r1_rc, r2_rc

def h(p):
    with open(p, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()

def normalize_wire(path):
    """Decode a wire capture and normalize node-name GUIDs to <name>."""
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        return ""
    try:
        raw = subprocess.run([dump, path], capture_output=True, text=True,
                             timeout=30, check=True).stdout
    except subprocess.CalledProcessError as e:
        return f"<decode error: {e.stderr.strip()}>"
    # Node names are 32-hex-char GUIDs in `name=`, `broker=`, `referrer=`,
    # `current_peer_node=` (and `proxy_node=` inside descriptors) fields.
    return re.sub(r'(?<=[= ])[0-9a-f]{32}', '<name>', raw)

# Baseline: all three nodes official.
base_br, base_a_rc, base_b_rc, base_r1, base_r2 = run_three(
    [driver, "invite-node-b-3node"], "baseline")

# Interop: B is the native Rust 3node-acceptor.
int_br, int_a_rc, int_b_rc, int_r1, int_r2 = run_three(
    [b3], "interop")

base_broker = os.path.join(out, "baseline", "broker.events")
int_broker = os.path.join(out, "interop", "broker.events")
base_a = os.path.join(out, "baseline", "a.events")
int_a = os.path.join(out, "interop", "a.events")

broker_identical = (base_br == 0 and int_br == 0 and
                    open(base_broker, 'rb').read() == open(int_broker, 'rb').read())
a_identical = (base_a_rc == 0 and int_a_rc == 0 and
               open(base_a, 'rb').read() == open(int_a, 'rb').read())

# Structural wire comparison of the referral transport (both directions):
# decoded message sequences must match modulo node names.
w = os.path.join(out, "interop", "wire")
wb = os.path.join(out, "baseline", "wire")
wire_ok = True
wire_detail = {}
for direction in ("broker-to-b", "b-to-broker"):
    base_norm = normalize_wire(os.path.join(wb, direction + ".bin"))
    int_norm = normalize_wire(os.path.join(w, direction + ".bin"))
    same = base_norm == int_norm and base_norm != ""
    wire_ok = wire_ok and same
    wire_detail[direction] = {
        "identical": same,
        "baseline_decoded": base_norm,
        "interop_decoded": int_norm,
    }

ok = (broker_identical and a_identical and wire_ok and
      base_b_rc == 0 and int_b_rc == 0 and base_r1 == 0 and int_r1 == 0 and
      base_r2 == 0 and int_r2 == 0)

receipt = {
    "schema_version": 1,
    "court": "3node",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_br, "a_exit": base_a_rc, "b_exit": base_b_rc,
                 "relay1_exit": base_r1, "relay2_exit": base_r2},
    "interop": {"broker_exit": int_br, "a_exit": int_a_rc, "b_exit": int_b_rc,
                "relay1_exit": int_r1, "relay2_exit": int_r2},
    "broker_events_identical": broker_identical,
    "a_events_identical": a_identical,
    "referral_wire_identical": wire_ok,
    "artifacts": {
        "baseline/broker.events": {"sha256": h(base_broker), "bytes": os.path.getsize(base_broker)},
        "baseline/a.events": {"sha256": h(base_a), "bytes": os.path.getsize(base_a)},
        "baseline/b.events": {"sha256": h(os.path.join(out, "baseline", "b.events")), "bytes": os.path.getsize(os.path.join(out, "baseline", "b.events"))},
        "interop/broker.events": {"sha256": h(int_broker), "bytes": os.path.getsize(int_broker)},
        "interop/a.events": {"sha256": h(int_a), "bytes": os.path.getsize(int_a)},
        "interop/b.events": {"sha256": h(os.path.join(out, "interop", "b.events")), "bytes": os.path.getsize(os.path.join(out, "interop", "b.events"))},
    },
    "inputs": {
        "oracle_driver_binary": h(driver),
        "wire_relay_binary": h(relay),
        "wire_dump_binary": h(dump),
        "native_3node_acceptor_binary": h(b3),
    },
    "wire_comparison": wire_detail,
}
mout = os.path.join(manifests, f"3node-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"3node court FAILED: broker-identical={broker_identical} "
        f"a-identical={a_identical} wire={wire_ok} "
        f"baseline=({base_br},{base_a_rc},{base_b_rc}) interop=({int_br},{int_a_rc},{int_b_rc})")
print(f"3node court PASS: broker and A event streams byte-identical; "
      f"referral wire structurally identical; "
      f"broker={int_br} a={int_a_rc} native-b={int_b_rc}")
PYEOF
