#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# verify_environment.sh — record the execution environment and verify the
# court prerequisites. Emits an environment receipt JSON.
#
#   scripts/verify_environment.sh [--out <path>] [--require]
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

OUT="${MOJO_RS_ENV_RECEIPT:-}"
REQUIRE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --require) REQUIRE=1; shift ;;
    *) mojo_rs_fail "unknown argument: $1" ;;
  esac
done

FAIL=0
check() { # 1:cmd 2:display
  if command -v "$1" >/dev/null 2>&1; then
    printf '  OK: %s (%s)\n' "$2" "$(command -v "$1")" >&2
  else
    printf '  FAIL: %s not found\n' "$2" >&2
    FAIL=1
  fi
}

mojo_rs_log "environment verification"
printf '  host: %s\n' "$(uname -a)" >&2
printf '  date: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >&2

for c in git curl python3 sha256sum file jq; do check "$c" "$c"; done
for c in cargo rustc; do check "$c" "$c"; done
[ "$REQUIRE" = 1 ] && for c in docker rootlesskit qemu-img sfdisk debugfs; do check "$c" "$c"; done

# Tool versions (best effort; empty when absent).
V_RUSTC="$(rustc --version 2>/dev/null || true)"
V_CARGO="$(cargo --version 2>/dev/null || true)"
V_CLANG="$(clang --version 2>/dev/null | sed -n 1p || true)"
V_GCC="$(gcc --version 2>/dev/null | sed -n 1p || true)"
V_PY="$(python3 --version 2>/dev/null || true)"
V_GLIBC="$(ldd --version 2>/dev/null | sed -n 1p || true)"
V_KERNEL="$(uname -r 2>/dev/null || true)"
V_ARCH="$(uname -m 2>/dev/null || true)"
V_LC="$(locale 2>/dev/null | sed -n 1p || true)"
V_TZ="${TZ:-<unset>}"
V_GIT="$(git --version 2>/dev/null || true)"
V_DOCKER_CLI="$(docker --version 2>/dev/null || true)"
V_DOCKER_SRV="$(docker info --format '{{.ServerVersion}}' 2>/dev/null || true)"

RECEIPT="$OUT"
if [ -z "$RECEIPT" ]; then
  RECEIPT="$(mktemp --suffix=-env-receipt.json)"
fi
cat > "$RECEIPT" <<EOF
{
  "schema_version": 1,
  "generated_at": "$(mojo_rs_now_utc)",
  "host": "$(uname -a)",
  "kernel": "$V_KERNEL",
  "architecture": "$V_ARCH",
  "glibc": "$V_GLIBC",
  "locale": "$V_LC",
  "tz": "$V_TZ",
  "rustc": "$V_RUSTC",
  "cargo": "$V_CARGO",
  "clang": "$V_CLANG",
  "gcc": "$V_GCC",
  "python": "$V_PY",
  "git": "$V_GIT",
  "docker_cli": "$V_DOCKER_CLI",
  "docker_server": "$V_DOCKER_SRV",
  "docker_host": "${MOJO_RS_DOCKER_HOST:-<unset>}",
  "docker_root": "${MOJO_RS_DOCKER_ROOT:-<unset>}",
  "artifacts_ok": true
}
EOF
printf '\n  receipt: %s\n' "$RECEIPT" >&2

if [ "$FAIL" = 1 ]; then
  mojo_rs_fail "environment verification FAILED"
fi
mojo_rs_ok "environment verified"
