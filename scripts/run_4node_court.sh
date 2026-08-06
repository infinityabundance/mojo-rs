#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_4node_court.sh — the Phase 5 introduction court: broker + referrer A +
# referred B + introduced C, with the broker links captured by three
# man-in-the-middle wire relays.
#
# Topology and transports:
#   broker ── relay1 ── A      (the broker↔A invitation-1 link)
#   broker ── relay2 ── B      (the referral transport: A's socket-b fd travels
#                               inside `ReferNonBroker` through relay1 to the
#                               broker, which connects to B through relay2)
#   broker ── relay3 ── C      (the referral transport: B's socket-c fd travels
#                               inside B's `ReferNonBroker` through relay2 to
#                               the broker, which connects to C through relay3)
#
# Scenario: A creates (X, Y) locally and transfers Y through the a2b pipe to B
# (the WithLocalPeer path); B re-transfers Y' through the b2c pipe to C with
# `proxy_peer_node_name` = A; C's new router calls BypassPeer(A) but has NO
# link to A, so it requests an introduction from the broker
# (`RequestIntroduction`); the broker sends `AcceptIntroduction` to both C and
# A; C establishes the C<->A link and completes the bypass with
# `AcceptBypassLink` (the `EstablishLink` -> `BypassPeerWithNewRemoteLink`
# path); the X<->Y'' "hello"/"world" round trip then crosses the new link.
#
# Three runs, all through wire relays:
#   1. baseline:  official broker ⇄ official A ⇄ official B ⇄ official C
#   2. interop-c: official broker ⇄ official A ⇄ official B ⇄ native Rust C
#                 (4node-acceptor)
#   3. interop-a: official broker ⇄ native Rust A (4node-referrer) ⇄ official B
#                 ⇄ official C
#
# Equivalence: in interop-c, the broker's, A's, and B's event streams must be
# IDENTICAL to the baseline; in interop-a, the broker's, B's, and C's event
# streams must be IDENTICAL to the baseline. The relayed broker-link wires are
# compared structurally (decoded message sequences, node names normalized).
# The native node must additionally verify its exchange and exit 0.
#
# Evidence produced under evidence/4node/<stamp>/:
#   baseline/{broker,a,b,c}.events       interop-c/{broker,a,b,c}.events
#   interop-a/{broker,a,b,c}.events
#   <tag>/wire/{broker-to-a,a-to-broker,broker-to-b,b-to-broker,
#               broker-to-c,c-to-broker}.bin
#   evidence/manifests/4node-<stamp>.json
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

mojo_rs_log "building the wire relay, wire-dump, and the native 4node nodes"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin wire-dump
cargo build --quiet -p mojo-rs-interop --bin 4node-acceptor
cargo build --quiet -p mojo-rs-interop --bin 4node-referrer
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
DUMP="$MOJO_RS_REPO_ROOT/target/debug/wire-dump"
C4="$MOJO_RS_REPO_ROOT/target/debug/4node-acceptor"
A4="$MOJO_RS_REPO_ROOT/target/debug/4node-referrer"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$DUMP" ] || mojo_rs_fail "wire-dump missing"
[ -x "$C4" ] || mojo_rs_fail "4node acceptor missing"
[ -x "$A4" ] || mojo_rs_fail "4node referrer missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/4node/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop-c/wire" "$OUT/interop-a/wire" \
  "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$DUMP" "$C4" "$A4" "$OUT" "$STAMP" \
  "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, re, socket, subprocess, sys

driver, relay, dump, c4, a4, out, stamp, manifests = sys.argv[1:]

def run_four(a_cmd, b_cmd, c_cmd, tag):
    """Run broker (oracle) against the given A/B/C commands."""
    # broker<->A transport: (broker_sock, relay1_a) and (relay1_b, a_broker_sock).
    broker_sock, relay1_a = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    relay1_b, a_broker_sock = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    # B's referral transport (relay2): (broker_ref_b_sock, relay2_a) and
    # (relay2_b, b_sock). A's socket-b fd is broker_ref_b_sock; it travels
    # inside A's `ReferNonBroker` through relay1 to the broker, which connects
    # to B through relay2.
    broker_ref_b_sock, relay2_a = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    relay2_b, b_sock = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    # C's referral transport (relay3): (broker_ref_c_sock, relay3_a) and
    # (relay3_b, c_sock). B's socket-c fd is broker_ref_c_sock; it travels
    # inside B's `ReferNonBroker` through relay2 to the broker, which connects
    # to C through relay3.
    broker_ref_c_sock, relay3_a = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    relay3_b, c_sock = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    for f in (broker_sock, relay1_a, relay1_b, a_broker_sock,
              broker_ref_b_sock, relay2_a, relay2_b, b_sock,
              broker_ref_c_sock, relay3_a, relay3_b, c_sock):
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
    relay3 = subprocess.Popen(
        [relay, str(relay3_a.fileno()), str(relay3_b.fileno()),
         os.path.join(w, "broker-to-c.bin"), os.path.join(w, "c-to-broker.bin")],
        pass_fds=(relay3_a.fileno(), relay3_b.fileno()))

    a = subprocess.Popen(
        a_cmd + [str(a_broker_sock.fileno()), str(broker_ref_b_sock.fileno()),
                 os.path.join(out, tag, "a.events")],
        pass_fds=(a_broker_sock.fileno(), broker_ref_b_sock.fileno()))
    b = subprocess.Popen(
        b_cmd + [str(b_sock.fileno()), str(broker_ref_c_sock.fileno()),
                 os.path.join(out, tag, "b.events")],
        pass_fds=(b_sock.fileno(), broker_ref_c_sock.fileno()))
    c = subprocess.Popen(
        c_cmd + [str(c_sock.fileno()), os.path.join(out, tag, "c.events")],
        pass_fds=(c_sock.fileno(),))
    broker = subprocess.Popen(
        [driver, "invite-broker-4node", str(broker_sock.fileno()),
         os.path.join(out, tag, "broker.events")],
        pass_fds=(broker_sock.fileno(),))

    broker_sock.close(); relay1_a.close(); relay1_b.close(); a_broker_sock.close()
    broker_ref_b_sock.close(); relay2_a.close(); relay2_b.close(); b_sock.close()
    broker_ref_c_sock.close(); relay3_a.close(); relay3_b.close(); c_sock.close()

    try:
        broker_rc = broker.wait(timeout=90)
        a_rc = a.wait(timeout=90)
        b_rc = b.wait(timeout=90)
        c_rc = c.wait(timeout=90)
        r1_rc = relay1.wait(timeout=30)
        r2_rc = relay2.wait(timeout=30)
        r3_rc = relay3.wait(timeout=30)
    except subprocess.TimeoutExpired as e:
        for p in (broker, a, b, c, relay1, relay2, relay3):
            p.kill()
        raise SystemExit(f"{tag} run timed out: {e}")
    return broker_rc, a_rc, b_rc, c_rc, r1_rc, r2_rc, r3_rc

def h(p):
    with open(p, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()

def normalize_wire(path, drop_bypass_exchange=False):
    """Decode a wire capture and normalize node-name GUIDs to <name>.

    When `drop_bypass_exchange` is set (the broker<->A directions only), the
    pipe_a bridge-bypass exchange messages are dropped: the all-official
    baseline races on WHO initiates each bypass stage (the shared
    RouterLinkState lock CAS between the broker and A), producing either
    variant with byte-identical event streams. The exchange messages
    (`BypassPeerWithLink`, `FlushRouter`, `StopProxyingToLocalPeer`, and the
    broker's portal-0 flush) differ between the variants, but their FIXED
    POINT does not: both variants converge on sublink 13, and the `done` /
    closure traffic on sublink 13 is still compared, so a genuinely
    divergent wire (e.g. a missing bypass) remains detectable. No other
    relayed direction carries these message types, so the rule is narrow.
    """
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        return ""
    try:
        raw = subprocess.run([dump, path], capture_output=True, text=True,
                             timeout=30, check=True).stdout
    except subprocess.CalledProcessError as e:
        return f"<decode error: {e.stderr.strip()}>"
    lines = raw.splitlines()
    if drop_bypass_exchange:
        keep = []
        for line in lines:
            if any(t in line for t in ("id=34 (BypassPeerWithLink)",
                                       "id=36 (FlushRouter)",
                                       "id=35 (StopProxyingToLocalPeer)")):
                continue
            # The exchange consumes a variant-dependent number of messages,
            # so the remaining messages' ordinal prefixes and per-link
            # sequence numbers differ between the variants too; strip them
            # (the payload fields still compare exactly).
            line = re.sub(r'^\[[0-9]+\] ', '', line)
            line = re.sub(r'\bseq=[0-9]+ ', '', line)
            keep.append(line)
        raw = "\n".join(keep) + "\n"
    # Node names are 32-hex-char GUIDs in `name=`, `broker=`, `referrer=`,
    # `current_peer_node=`, `proxy_node=`, `target_node=`, and `source=`
    # fields.
    return re.sub(r'(?<=[= ])[0-9a-f]{32}', '<name>', raw)

def events_identical(base_path, int_path):
    return (open(base_path, 'rb').read() == open(int_path, 'rb').read())

# Baseline: all four nodes official.
base_br, base_a_rc, base_b_rc, base_c_rc, base_r1, base_r2, base_r3 = run_four(
    [driver, "invite-node-a-4node"], [driver, "invite-node-b-4node"],
    [driver, "invite-node-c-4node"], "baseline")

# Interop-c: C is the native Rust 4node-acceptor.
intc_br, intc_a_rc, intc_b_rc, intc_c_rc, intc_r1, intc_r2, intc_r3 = run_four(
    [driver, "invite-node-a-4node"], [driver, "invite-node-b-4node"],
    [c4], "interop-c")

# Interop-a: A is the native Rust 4node-referrer.
inta_br, inta_a_rc, inta_b_rc, inta_c_rc, inta_r1, inta_r2, inta_r3 = run_four(
    [a4], [driver, "invite-node-b-4node"], [driver, "invite-node-c-4node"],
    "interop-a")

def p(tag, name):
    return os.path.join(out, tag, name)

# Interop-c equivalence: broker + A + B event streams byte-identical.
broker_identical_c = (base_br == 0 and intc_br == 0 and
                     events_identical(p("baseline", "broker.events"),
                                      p("interop-c", "broker.events")))
a_identical_c = (base_a_rc == 0 and intc_a_rc == 0 and
                events_identical(p("baseline", "a.events"),
                                 p("interop-c", "a.events")))
b_identical_c = (base_b_rc == 0 and intc_b_rc == 0 and
                events_identical(p("baseline", "b.events"),
                                 p("interop-c", "b.events")))

# Interop-a equivalence: broker + B + C event streams byte-identical.
broker_identical_a = (base_br == 0 and inta_br == 0 and
                     events_identical(p("baseline", "broker.events"),
                                      p("interop-a", "broker.events")))
b_identical_a = (base_b_rc == 0 and inta_b_rc == 0 and
                events_identical(p("baseline", "b.events"),
                                 p("interop-a", "b.events")))
c_identical_a = (base_c_rc == 0 and inta_c_rc == 0 and
                events_identical(p("baseline", "c.events"),
                                 p("interop-a", "c.events")))

# Structural wire comparison of every relayed broker-link direction for each
# interop run: decoded message sequences must match modulo node names. The
# broker<->A directions drop the pipe_a bridge-bypass exchange (the official
# baseline races on its initiator; see `normalize_wire`); the other four
# directions compare exactly.
WIRE_DIRECTIONS = ("broker-to-a", "a-to-broker", "broker-to-b", "b-to-broker",
                   "broker-to-c", "c-to-broker")
BYPASS_RACE_DIRECTIONS = ("broker-to-a", "a-to-broker")
def wires_identical(tag):
    ok = True
    detail = {}
    for direction in WIRE_DIRECTIONS:
        base_norm = normalize_wire(os.path.join(out, "baseline", "wire", direction + ".bin"),
                                   drop_bypass_exchange=direction in BYPASS_RACE_DIRECTIONS)
        int_norm = normalize_wire(os.path.join(out, tag, "wire", direction + ".bin"),
                                  drop_bypass_exchange=direction in BYPASS_RACE_DIRECTIONS)
        same = base_norm == int_norm and base_norm != ""
        ok = ok and same
        detail[direction] = {"identical": same}
    return ok, detail

wire_ok_c, wire_detail_c = wires_identical("interop-c")
wire_ok_a, wire_detail_a = wires_identical("interop-a")

ok = (broker_identical_c and a_identical_c and b_identical_c and
      broker_identical_a and b_identical_a and c_identical_a and
      wire_ok_c and wire_ok_a and
      intc_c_rc == 0 and inta_a_rc == 0 and
      base_r1 == 0 and base_r2 == 0 and base_r3 == 0 and
      intc_r1 == 0 and intc_r2 == 0 and intc_r3 == 0 and
      inta_r1 == 0 and inta_r2 == 0 and inta_r3 == 0)

def art(rel):
    pth = os.path.join(out, rel)
    return {"sha256": h(pth), "bytes": os.path.getsize(pth)}

receipt = {
    "schema_version": 1,
    "court": "4node",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_br, "a_exit": base_a_rc, "b_exit": base_b_rc,
                 "c_exit": base_c_rc, "relay1_exit": base_r1, "relay2_exit": base_r2,
                 "relay3_exit": base_r3},
    "interop-c": {"broker_exit": intc_br, "a_exit": intc_a_rc, "b_exit": intc_b_rc,
                  "c_exit": intc_c_rc, "relay1_exit": intc_r1, "relay2_exit": intc_r2,
                  "relay3_exit": intc_r3},
    "interop-a": {"broker_exit": inta_br, "a_exit": inta_a_rc, "b_exit": inta_b_rc,
                  "c_exit": inta_c_rc, "relay1_exit": inta_r1, "relay2_exit": inta_r2,
                  "relay3_exit": inta_r3},
    "broker_events_identical_interop_c": broker_identical_c,
    "a_events_identical_interop_c": a_identical_c,
    "b_events_identical_interop_c": b_identical_c,
    "broker_events_identical_interop_a": broker_identical_a,
    "b_events_identical_interop_a": b_identical_a,
    "c_events_identical_interop_a": c_identical_a,
    "wire_identical_interop_c": wire_ok_c,
    "wire_identical_interop_a": wire_ok_a,
    "wire_normalization": {
        "node_name_guids": "32-hex GUIDs -> <name>",
        "pipe_a_bridge_bypass_exchange": (
            "dropped on broker<->A directions: the all-official baseline "
            "races on the bypass initiator (shared RouterLinkState lock CAS "
            "between broker and A); both variants produce byte-identical "
            "event streams and converge on the same final sublink (13), "
            "whose done/closure traffic is still compared"
        ),
    },
    "artifacts": {
        "baseline/broker.events": art("baseline/broker.events"),
        "baseline/a.events": art("baseline/a.events"),
        "baseline/b.events": art("baseline/b.events"),
        "baseline/c.events": art("baseline/c.events"),
        "interop-c/broker.events": art("interop-c/broker.events"),
        "interop-c/a.events": art("interop-c/a.events"),
        "interop-c/b.events": art("interop-c/b.events"),
        "interop-c/c.events": art("interop-c/c.events"),
        "interop-a/broker.events": art("interop-a/broker.events"),
        "interop-a/a.events": art("interop-a/a.events"),
        "interop-a/b.events": art("interop-a/b.events"),
        "interop-a/c.events": art("interop-a/c.events"),
    },
    "inputs": {
        "oracle_driver_binary": h(driver),
        "wire_relay_binary": h(relay),
        "wire_dump_binary": h(dump),
        "native_4node_acceptor_binary": h(c4),
        "native_4node_referrer_binary": h(a4),
    },
    "wire_comparison_interop_c": wire_detail_c,
    "wire_comparison_interop_a": wire_detail_a,
}
mout = os.path.join(manifests, f"4node-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"4node court FAILED: "
        f"interop-c broker-identical={broker_identical_c} a-identical={a_identical_c} "
        f"b-identical={b_identical_c} wire={wire_ok_c} native-c={intc_c_rc} "
        f"interop-a broker-identical={broker_identical_a} b-identical={b_identical_a} "
        f"c-identical={c_identical_a} wire={wire_ok_a} native-a={inta_a_rc}")
print(f"4node court PASS: broker/A/B/C event streams byte-identical in both "
      f"mixed pairings; relayed broker-link wires structurally identical; "
      f"interop-c native-c={intc_c_rc} interop-a native-a={inta_a_rc}")
PYEOF
