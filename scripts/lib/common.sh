#!/usr/bin/env bash
# lib/common.sh — shared helpers for mojo-rs scripts.
# Source AFTER configure_local_environment.sh.
set -euo pipefail

if [ -n "${MOJO_RS_COMMON_LOADED:-}" ]; then
  return 0 2>/dev/null || exit 0
fi
MOJO_RS_COMMON_LOADED=1

# Resolve the repository root (parent of scripts/).
MOJO_RS_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

mojo_rs_fail() {
  printf 'FATAL: %s\n' "$*" >&2
  exit 1
}

mojo_rs_log() { printf '=== [mojo-rs] %s ===\n' "$*" >&2; }
mojo_rs_info() { printf '  %s\n' "$*" >&2; }
mojo_rs_warn() { printf '  WARN: %s\n' "$*" >&2; }
mojo_rs_ok() { printf '  OK: %s\n' "$*" >&2; }

mojo_rs_require_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || mojo_rs_fail "required command not found: $cmd"
}

# Realpath that fails loudly on broken symlinks / missing paths.
mojo_rs_realpath() {
  local p="$1"
  [ -e "$p" ] || mojo_rs_fail "path does not exist: $p"
  local r
  r="$(realpath -m "$p" 2>/dev/null)" || mojo_rs_fail "cannot resolve path: $p"
  printf '%s' "$r"
}

# Normalize an absolute directory path (no trailing slash).
mojo_rs_abspath() {
  local p="$1"
  case "$p" in
    /*) ;;
    *) mojo_rs_fail "not an absolute path: $p" ;;
  esac
  printf '%s' "${p%/}"
}

# Byte free space on the filesystem containing path $1.
mojo_rs_free_bytes() {
  df -Pk -- "$1" | awk 'NR==2 {print $4*1024}'
}

# sha256 of a file (streaming; tolerant of large inputs).
mojo_rs_sha256() {
  sha256sum "$1" | awk '{print $1}'
}

mojo_rs_now_utc() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# Export the docker-related environment for the PROJECT daemon. Every command
# that talks to Docker must go through this so the isolated socket is always used
# and the host default daemon is never touched accidentally.
mojo_rs_export_docker_env() {
  export DOCKER_HOST="${MOJO_RS_DOCKER_HOST:?environment not configured}"
  export BUILDX_CONFIG="$MOJO_RS_BUILDKIT_ROOT/buildx"
  export TMPDIR="$MOJO_RS_DOCKER_ROOT/tmp"
  export TEMP="$MOJO_RS_DOCKER_ROOT/tmp"
  export TMP="$MOJO_RS_DOCKER_ROOT/tmp"
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  export PATH="$MOJO_RS_DOCKER_ROOT/bin:$PATH"
}

# Normalize the daemon's reported DockerRootDir (which is expressed through the
# rootless copy-up bind view) to the canonical host path.
mojo_rs_normalize_docker_root() {
  local reported="$1"
  local norm
  norm="$(printf '%s' "$reported" | sed 's#^/run/\.ro[0-9]*/#/run/#')"
  local canon
  canon="$(readlink -f "$norm" 2>/dev/null || printf '%s' "$norm")"
  printf '%s' "$canon"
}
