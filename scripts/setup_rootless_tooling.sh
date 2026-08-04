#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# setup_rootless_tooling.sh — stage the pinned rootless daemon tooling into
# $MOJO_RS_DOCKER_ROOT/bin, sha256-verified.
#
# Pins (see atlas/pins.json):
#   rootlesskit 2.3.2            (rootlesskit, rootlessctl, rootlesskit-docker-proxy)
#   slirp4netns 1.3.2            (libslirp 4.9.0, commit 0f13345bcef588d2bb70d662d41e92ee8a816d85)
#
# Sources, in order (first verified match wins):
#   1. an existing verified stage in $MOJO_RS_DOCKER_ROOT/bin
#   2. $MOJO_RS_ROOTLESS_TOOLING_SOURCE (host-provided verified directory)
#   3. pinned release download URLs (best-effort; upstream assets are often gone)
#
# Every staged binary is byte-verified against the recorded sha256; a mismatch
# aborts. This is a host-side deployment step, never a repository default.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

BIN_DIR="$MOJO_RS_DOCKER_ROOT/bin"
STAGE="$MOJO_RS_DOCKER_ROOT/tmp/rootless-stage"
mkdir -p "$BIN_DIR" "$STAGE"

# name -> sha256 (recorded from the verified pinned binaries)
declare -A TOOLS=(
  [rootlesskit]="d93d865f2de005efcf5a64d4bbd76506bd17c3ec6eb1e13046ab01c4ea8fbe71"
  [rootlessctl]="f0da39a18b93d8da066e680d233c546a594de720d1be92e4b872e961b70a97f3"
  [rootlesskit-docker-proxy]="35775c01e21797e88f4ccb8cee889b51d379615b616e16e83264c51614a3d77f"
  [slirp4netns]="4d55a3658ae259e3e74bb75cf058eb05d6e39ad6bbe170ca8e94c2462bea0eb1"
)

verify_one() { # path name expected_sha
  local p="$1" name="$2" want="$3"
  [ -f "$p" ] || return 1
  [ -x "$p" ] || chmod +x "$p" 2>/dev/null || return 1
  local got
  got="$(mojo_rs_sha256 "$p")"
  [ "$got" = "$want" ] || return 1
  return 0
}

need=()
for name in "${!TOOLS[@]}"; do
  if ! verify_one "$BIN_DIR/$name" "$name" "${TOOLS[$name]}"; then
    need+=("$name")
  fi
done

if [ "${#need[@]}" -eq 0 ]; then
  mojo_rs_ok "rootless tooling already staged and verified in $BIN_DIR"
  exit 0
fi

mojo_rs_log "staging rootless tooling (missing: ${need[*]})"

# --- source 2: host-provided verified directory -------------------------------------------
if [ -n "$MOJO_RS_ROOTLESS_TOOLING_SOURCE" ] && [ -d "$MOJO_RS_ROOTLESS_TOOLING_SOURCE" ]; then
  for name in "${need[@]}"; do
    if verify_one "$MOJO_RS_ROOTLESS_TOOLING_SOURCE/$name" "$name" "${TOOLS[$name]}"; then
      cp -f "$MOJO_RS_ROOTLESS_TOOLING_SOURCE/$name" "$BIN_DIR/$name"
      mojo_rs_ok "$name staged from MOJO_RS_ROOTLESS_TOOLING_SOURCE"
    fi
  done
fi

# --- source 3: pinned release URLs (best-effort) ------------------------------------------
still=()
for name in "${need[@]}"; do
  if verify_one "$BIN_DIR/$name" "$name" "${TOOLS[$name]}"; then
    continue
  fi
  case "$name" in
    rootlesskit|rootlessctl|rootlesskit-docker-proxy)
      url="https://github.com/moby/rootlesskit/releases/download/v2.3.2/rootlesskit-x86_64.tar.gz"
      ;;
    slirp4netns)
      url="https://github.com/rootless-containers/slirp4netns/releases/download/v1.3.2/slirp4netns-x86_64"
      ;;
  esac
  if curl -fsSL --max-time 300 -o "$STAGE/$name" "$url" 2>/dev/null \
     && verify_one "$STAGE/$name" "$name" "${TOOLS[$name]}"; then
    cp -f "$STAGE/$name" "$BIN_DIR/$name"
    mojo_rs_ok "$name staged from pinned release URL"
  else
    still+=("$name")
  fi
done

# --- source 1 re-check ------------------------------------------------------------
for name in "${need[@]}"; do
  if verify_one "$BIN_DIR/$name" "$name" "${TOOLS[$name]}"; then
    mojo_rs_ok "$name verified in $BIN_DIR"
  else
    mojo_rs_fail "no verified source for $name; set MOJO_RS_ROOTLESS_TOOLING_SOURCE to a
  directory containing the pinned binaries (see atlas/pins.json for sha256 values)"
  fi
done

mojo_rs_ok "rootless tooling complete:"
ls -la "$BIN_DIR"
