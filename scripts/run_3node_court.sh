#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_3node_court.sh — the Phase 5 multi-node referral court: broker + referrer
# A + referred B, with the referral transport captured by a man-in-the-middle
# wire relay.
#
# Three runs, all through wire relays:
#   1. baseline:  official broker ⇄ official A ⇄ official B
#   2. interop-b: official broker ⇄ official A ⇄ native Rust B (3node-acceptor)
#   3. interop-a: official broker ⇄ native Rust A (3node-referrer) ⇄ official B
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
# Equivalence: the broker's and A's event streams must be IDENTICAL between
# the baseline and interop-b; the broker's and B's event streams must be
# IDENTICAL between the baseline and interop-a; the referral transport wire is
# compared structurally (decoded message sequences, node names normalized).
# The native node must additionally verify its exchange and exit 0.
#
# Evidence produced under evidence/3node/<stamp>/:
#   baseline/{broker,a,b}.events            interop-b/{broker,a,b}.events
#   interop-a/{broker,a,b}.events
#   <tag>/wire/{broker-to-a,a-to-broker,broker-to-b,b-to-broker}.bin
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

mojo_rs_log "building the wire relay, wire-dump, and the native 3node nodes"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin wire-dump
cargo build --quiet -p mojo-rs-interop --bin 3node-acceptor
cargo build --quiet -p mojo-rs-interop --bin 3node-referrer
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
DUMP="$MOJO_RS_REPO_ROOT/target/debug/wire-dump"
B3="$MOJO_RS_REPO_ROOT/target/debug/3node-acceptor"
A3="$MOJO_RS_REPO_ROOT/target/debug/3node-referrer"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$DUMP" ] || mojo_rs_fail "wire-dump missing"
[ -x "$B3" ] || mojo_rs_fail "3node acceptor missing"
[ -x "$A3" ] || mojo_rs_fail "3node referrer missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/3node/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop-b/wire" "$OUT/interop-a/wire" \
  "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$DUMP" "$B3" "$A3" "$OUT" "$STAMP" \
  "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, re, socket, subprocess, sys

driver, relay, dump, b3, a3, out, stamp, manifests = sys.argv[1:]

def run_three(a_cmd, b_cmd, tag):
    """Run broker (oracle) against the given A and B commands."""
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
        a_cmd + [str(a_broker_sock.fileno()), str(broker_ref_sock.fileno()),
                 os.path.join(out, tag, "a.events")],
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

def events_identical(base_path, int_path):
    return (open(base_path, 'rb').read() == open(int_path, 'rb').read())

# Baseline: all three nodes official.
base_br, base_a_rc, base_b_rc, base_r1, base_r2 = run_three(
    [driver, "invite-node-a-3node"], [driver, "invite-node-b-3node"], "baseline")

# Interop-b: B is the native Rust 3node-acceptor.
intb_br, intb_a_rc, intb_b_rc, intb_r1, intb_r2 = run_three(
    [driver, "invite-node-a-3node"], [b3], "interop-b")

# Interop-a: A is the native Rust 3node-referrer.
inta_br, inta_a_rc, inta_b_rc, inta_r1, inta_r2 = run_three(
    [a3], [driver, "invite-node-b-3node"], "interop-a")

def p(tag, name):
    return os.path.join(out, tag, name)

# Interop-b equivalence: broker + A event streams byte-identical.
broker_identical_b = (base_br == 0 and intb_br == 0 and
                      events_identical(p("baseline", "broker.events"),
                                       p("interop-b", "broker.events")))
a_identical_b = (base_a_rc == 0 and intb_a_rc == 0 and
                 events_identical(p("baseline", "a.events"),
                                  p("interop-b", "a.events")))

# Interop-a equivalence: broker + B event streams byte-identical.
broker_identical_a = (base_br == 0 and inta_br == 0 and
                      events_identical(p("baseline", "broker.events"),
                                       p("interop-a", "broker.events")))
b_identical_a = (base_b_rc == 0 and inta_b_rc == 0 and
                 events_identical(p("baseline", "b.events"),
                                  p("interop-a", "b.events")))

# Structural wire comparison of the referral transport (both directions) for
# each interop run: decoded message sequences must match modulo node names.
def wires_identical(tag):
    ok = True
    detail = {}
    for direction in ("broker-to-b", "b-to-broker"):
        base_norm = normalize_wire(os.path.join(out, "baseline", "wire", direction + ".bin"))
        int_norm = normalize_wire(os.path.join(out, tag, "wire", direction + ".bin"))
        same = base_norm == int_norm and base_norm != ""
        ok = ok and same
        detail[direction] = {"identical": same}
    return ok, detail

wire_ok_b, wire_detail_b = wires_identical("interop-b")
wire_ok_a, wire_detail_a = wires_identical("interop-a")

ok = (broker_identical_b and a_identical_b and broker_identical_a and
      b_identical_a and wire_ok_b and wire_ok_a and
      intb_b_rc == 0 and inta_a_rc == 0 and
      base_r1 == 0 and base_r2 == 0 and
      intb_r1 == 0 and intb_r2 == 0 and inta_r1 == 0 and inta_r2 == 0)

def art(rel):
    pth = os.path.join(out, rel)
    return {"sha256": h(pth), "bytes": os.path.getsize(pth)}

receipt = {
    "schema_version": 1,
    "court": "3node",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_br, "a_exit": base_a_rc, "b_exit": base_b_rc,
                 "relay1_exit": base_r1, "relay2_exit": base_r2},
    "interop-b": {"broker_exit": intb_br, "a_exit": intb_a_rc, "b_exit": intb_b_rc,
                  "relay1_exit": intb_r1, "relay2_exit": intb_r2},
    "interop-a": {"broker_exit": inta_br, "a_exit": inta_a_rc, "b_exit": inta_b_rc,
                  "relay1_exit": inta_r1, "relay2_exit": inta_r2},
    "broker_events_identical_interop_b": broker_identical_b,
    "a_events_identical_interop_b": a_identical_b,
    "broker_events_identical_interop_a": broker_identical_a,
    "b_events_identical_interop_a": b_identical_a,
    "referral_wire_identical_interop_b": wire_ok_b,
    "referral_wire_identical_interop_a": wire_ok_a,
    "artifacts": {
        "baseline/broker.events": art("baseline/broker.events"),
        "baseline/a.events": art("baseline/a.events"),
        "baseline/b.events": art("baseline/b.events"),
        "interop-b/broker.events": art("interop-b/broker.events"),
        "interop-b/a.events": art("interop-b/a.events"),
        "interop-b/b.events": art("interop-b/b.events"),
        "interop-a/broker.events": art("interop-a/broker.events"),
        "interop-a/a.events": art("interop-a/a.events"),
        "interop-a/b.events": art("interop-a/b.events"),
    },
    "inputs": {
        "oracle_driver_binary": h(driver),
        "wire_relay_binary": h(relay),
        "wire_dump_binary": h(dump),
        "native_3node_acceptor_binary": h(b3),
        "native_3node_referrer_binary": h(a3),
    },
    "wire_comparison_interop_b": wire_detail_b,
    "wire_comparison_interop_a": wire_detail_a,
}
mout = os.path.join(manifests, f"3node-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"3node court FAILED: "
        f"interop-b broker-identical={broker_identical_b} a-identical={a_identical_b} "
        f"wire={wire_ok_b} native-b={intb_b_rc} "
        f"interop-a broker-identical={broker_identical_a} b-identical={b_identical_a} "
        f"wire={wire_ok_a} native-a={inta_a_rc}")
print(f"3node court PASS: broker/A/B event streams byte-identical in both "
      f"mixed pairings; referral wire structurally identical; "
      f"interop-b native-b={intb_b_rc} interop-a native-a={inta_a_rc}")
PYEOF
