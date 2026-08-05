#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run_interop_court.sh — the Phase 3 interop seal gate: bidirectional official
# C++ ⇄ native Rust transfer.
#
# Two runs, both through the wire-relay man-in-the-middle:
#   1. baseline:  official broker ⇄ official oracle acceptor
#   2. interop:   official broker ⇄ native Rust acceptor (ipcz-acceptor)
#
# The broker's event stream must be IDENTICAL in both runs (same op sequence,
# same results, same payload and fd hex) — the broker cannot distinguish the
# native acceptor from the official one. The Rust acceptor must additionally
# verify the broker's payload and descriptor content and exit 0.
#
# Evidence produced under evidence/interop/:
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

mojo_rs_log "building the wire relay and the native acceptor"
cargo build --quiet -p mojo-rs-interop --bin wire-relay
cargo build --quiet -p mojo-rs-interop --bin ipcz-acceptor
RELAY="$MOJO_RS_REPO_ROOT/target/debug/wire-relay"
ACCEPTOR="$MOJO_RS_REPO_ROOT/target/debug/ipcz-acceptor"
[ -x "$RELAY" ] || mojo_rs_fail "wire relay missing"
[ -x "$ACCEPTOR" ] || mojo_rs_fail "native acceptor missing"

EVIDENCE_ROOT="${MOJO_RS_EVIDENCE_ROOT:-$MOJO_RS_REPO_ROOT/evidence}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$EVIDENCE_ROOT/interop/$STAMP"
mkdir -p "$OUT/baseline/wire" "$OUT/interop/wire" "$EVIDENCE_ROOT/manifests"

python3 - "$ORACLE_DRIVER" "$RELAY" "$ACCEPTOR" "$OUT" "$STAMP" "$EVIDENCE_ROOT/manifests" <<'PYEOF'
import hashlib, json, os, socket, subprocess, sys

driver, relay, acceptor_bin, out, stamp, manifests = sys.argv[1:]

def run_pair(acceptor_cmd, tag):
    """Run the official broker against the given acceptor command."""
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
        [driver, "invite-broker", str(b1.fileno()),
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
    [driver, "invite-acceptor"], "baseline")

# Interop: official broker ⇄ native Rust acceptor.
int_broker_rc, int_acc_rc, int_relay_rc = run_pair(
    [acceptor_bin], "interop")

base_broker = os.path.join(out, "baseline", "broker.events")
int_broker = os.path.join(out, "interop", "broker.events")
base_acc = os.path.join(out, "baseline", "acceptor.events")
int_acc = os.path.join(out, "interop", "acceptor.events")

# The broker's event stream must be byte-identical in both runs.
broker_events_identical = (base_broker_rc == 0 and int_broker_rc == 0 and
                           open(base_broker, 'rb').read() ==
                           open(int_broker, 'rb').read())

ok = (broker_events_identical and int_acc_rc == 0 and int_relay_rc == 0 and
      base_acc_rc == 0 and base_relay_rc == 0)

receipt = {
    "schema_version": 1,
    "court": "interop",
    "generated_at_utc": stamp,
    "status": "pass" if ok else "fail",
    "baseline": {"broker_exit": base_broker_rc, "acceptor_exit": base_acc_rc,
                 "relay_exit": base_relay_rc},
    "interop": {"broker_exit": int_broker_rc, "acceptor_exit": int_acc_rc,
                "relay_exit": int_relay_rc},
    "broker_events_identical": broker_events_identical,
    "artifacts": {
        "baseline/broker.events": {"sha256": h(base_broker), "bytes": os.path.getsize(base_broker)},
        "baseline/acceptor.events": {"sha256": h(base_acc), "bytes": os.path.getsize(base_acc)},
        "interop/broker.events": {"sha256": h(int_broker), "bytes": os.path.getsize(int_broker)},
        "interop/acceptor.events": {"sha256": h(int_acc), "bytes": os.path.getsize(int_acc)},
        "baseline/wire/broker-to-acceptor.bin": {"sha256": h(os.path.join(out, "baseline", "wire", "broker-to-acceptor.bin")), "bytes": os.path.getsize(os.path.join(out, "baseline", "wire", "broker-to-acceptor.bin"))},
        "baseline/wire/acceptor-to-broker.bin": {"sha256": h(os.path.join(out, "baseline", "wire", "acceptor-to-broker.bin")), "bytes": os.path.getsize(os.path.join(out, "baseline", "wire", "acceptor-to-broker.bin"))},
        "interop/wire/broker-to-acceptor.bin": {"sha256": h(os.path.join(out, "interop", "wire", "broker-to-acceptor.bin")), "bytes": os.path.getsize(os.path.join(out, "interop", "wire", "broker-to-acceptor.bin"))},
        "interop/wire/acceptor-to-broker.bin": {"sha256": h(os.path.join(out, "interop", "wire", "acceptor-to-broker.bin")), "bytes": os.path.getsize(os.path.join(out, "interop", "wire", "acceptor-to-broker.bin"))},
    },
    "inputs": {
        "oracle_driver_binary": h(driver),
        "wire_relay_binary": h(relay),
        "native_acceptor_binary": h(acceptor_bin),
    },
}
mout = os.path.join(manifests, f"interop-{stamp}.json")
with open(mout, 'w') as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
print(mout)
if not ok:
    raise SystemExit(
        f"interop court FAILED: broker-events-identical={broker_events_identical} "
        f"baseline=({base_broker_rc},{base_acc_rc}) interop=({int_broker_rc},{int_acc_rc})")
print(f"interop court PASS: broker event streams byte-identical; "
      f"broker={int_broker_rc} native-acceptor={int_acc_rc}")
PYEOF
