#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_invite_court.sh — run the cross-process invitation court: the official
# oracle broker and acceptor exchange an invitation, a bootstrap pipe, and a
# message + descriptor in each direction, with the raw ipcz node-link wire
# traffic captured by the wire-relay man-in-the-middle.
#
# Evidence produced:
#   evidence/invitations/oracle-broker.events
#   evidence/invitations/oracle-acceptor.events
#   evidence/invitations/wire/broker-to-acceptor.bin   (raw wire bytes)
#   evidence/invitations/wire/acceptor-to-broker.bin
#   evidence/manifests/invite-<stamp>.json
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

mojo_rs_log "building the wire relay"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/invitations"
mkdir -p "$OUT/wire" "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$OUT" "$STAMP" "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, socket, subprocess, sys

driver, relay, out, stamp, manifests = sys.argv[1:]

# Two socketpairs: broker<->relay and relay<->acceptor.
b1, b2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
a1, a2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
for f in (b1, b2, a1, a2):
    os.set_inheritable(f.fileno(), True)

relay_proc = subprocess.Popen(
    [relay, str(b2.fileno()), str(a1.fileno()),
     os.path.join(out, "wire", "broker-to-acceptor.bin"),
     os.path.join(out, "wire", "acceptor-to-broker.bin")],
    pass_fds=(b2.fileno(), a1.fileno()))

acceptor = subprocess.Popen(
    [driver, "invite-acceptor", str(a2.fileno()),
     os.path.join(out, "oracle-acceptor.events")],
    pass_fds=(a2.fileno(),))
broker = subprocess.Popen(
    [driver, "invite-broker", str(b1.fileno()),
     os.path.join(out, "oracle-broker.events")],
    pass_fds=(b1.fileno(),))

# The children hold their own copies of the endpoints (via pass_fds); close
# the parent's copies so peer-close (EOF) propagates to the relay once the
# broker and acceptor exit.
b1.close()
b2.close()
a1.close()
a2.close()

try:
    broker_rc = broker.wait(timeout=60)
    acceptor_rc = acceptor.wait(timeout=60)
    relay_rc = relay_proc.wait(timeout=30)
except subprocess.TimeoutExpired as e:
    for p in (broker, acceptor, relay_proc):
        p.kill()
    raise SystemExit(f"invite court timed out: {e}")

def h(p):
    with open(p, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()

events_ok = (broker_rc == 0 and acceptor_rc == 0)
wire_a = os.path.join(out, "wire", "broker-to-acceptor.bin")
wire_b = os.path.join(out, "wire", "acceptor-to-broker.bin")

receipt = {
    "schema_version": 1,
    "court": "invitations",
    "generated_at_utc": stamp,
    "status": "pass" if events_ok else "fail",
    "broker_exit": broker_rc,
    "acceptor_exit": acceptor_rc,
    "relay_exit": relay_rc,
    "artifacts": {
        "oracle-broker.events": {"sha256": h(os.path.join(out, "oracle-broker.events")), "bytes": os.path.getsize(os.path.join(out, "oracle-broker.events"))},
        "oracle-acceptor.events": {"sha256": h(os.path.join(out, "oracle-acceptor.events")), "bytes": os.path.getsize(os.path.join(out, "oracle-acceptor.events"))},
        "wire/broker-to-acceptor.bin": {"sha256": h(wire_a), "bytes": os.path.getsize(wire_a)},
        "wire/acceptor-to-broker.bin": {"sha256": h(wire_b), "bytes": os.path.getsize(wire_b)},
    },
    "inputs": {
        "oracle_driver_binary": h(driver),
        "wire_relay_binary": h(relay),
    },
}
mout = os.path.join(manifests, f"invite-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not events_ok:
    raise SystemExit(f"invite court FAILED: broker={broker_rc} acceptor={acceptor_rc}")
print(f"invite court PASS: broker={broker_rc} acceptor={acceptor_rc} "
      f"wire={os.path.getsize(wire_a)}+{os.path.getsize(wire_b)} bytes")
PYEOF
