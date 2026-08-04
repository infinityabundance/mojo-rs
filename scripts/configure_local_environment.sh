#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# configure_local_environment.sh — resolve the mojo-rs execution environment.
#
# Precedence (highest first):
#   1. explicit command-line arguments      (--MOJO_RS_DOCKER_ROOT=/path)
#   2. already-exported environment variables
#   3. ignored local config file            (config/local.env, then .env.local,
#                                           then .local/mojo-rs.env)
#   4. portable computed defaults
#
# An explicitly exported value is NEVER silently overwritten.
#
# Usage:
#   eval "$(scripts/configure_local_environment.sh)"   # print KEY=VALUE lines
#   . scripts/configure_local_environment.sh            # set vars in this shell
#   scripts/configure_local_environment.sh --print      # human-readable block
# ---------------------------------------------------------------------------
set -euo pipefail

# Detect sourcing vs execution.
MOJO_RS_SOURCED=0
if [ -n "${BASH_SOURCE[0]}" ] && [ "${BASH_SOURCE[0]}" != "$0" ]; then
  MOJO_RS_SOURCED=1
fi

MOJO_RS_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MOJO_RS_PRINT_HUMAN=0

# ---------------------------------------------------------------------------
# 1. command-line arguments (highest precedence) — only when executed, not when
#    sourced (bash shares $@ with the sourcing script).
# ---------------------------------------------------------------------------
if [ "$MOJO_RS_SOURCED" = 0 ]; then
  while [ $# -gt 0 ]; do
    case "$1" in
      --print) MOJO_RS_PRINT_HUMAN=1; shift ;;
      --*=*)
        key="${1#--}"; key="${key%%=*}"; val="${1#*=}"
        # Accept only MOJO_RS_* variables to avoid arbitrary env injection.
        case "$key" in
          MOJO_RS_*) export "$key=$val" ;;
          *) echo "ignoring unknown argument: $1" >&2 ;;
        esac
        shift
        ;;
      *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
  done
fi

# ---------------------------------------------------------------------------
# 2 + 3. exported env wins; otherwise read the ignored local config file.
# ---------------------------------------------------------------------------
mojo_rs_local_config=""
for cand in \
  "$MOJO_RS_REPO_ROOT/config/local.env" \
  "$MOJO_RS_REPO_ROOT/.env.local" \
  "$MOJO_RS_REPO_ROOT/.local/mojo-rs.env"; do
  if [ -f "$cand" ]; then
    mojo_rs_local_config="$cand"
    break
  fi
done

if [ -n "$mojo_rs_local_config" ]; then
  while IFS='=' read -r mojo_rs_key mojo_rs_val || [ -n "$mojo_rs_key" ]; do
    # Skip blank lines and comments.
    case "$mojo_rs_key" in
      ''|'#'*) continue ;;
    esac
    mojo_rs_key="$(printf '%s' "$mojo_rs_key" | tr -d '[:space:]')"
    case "$mojo_rs_key" in
      MOJO_RS_*)
        # Never overwrite an exported value from a higher-precedence source.
        if [ -z "${!mojo_rs_key:-}" ]; then
          export "$mojo_rs_key=$mojo_rs_val"
        fi
        ;;
    esac
  done < "$mojo_rs_local_config"
fi

# ---------------------------------------------------------------------------
# 4. portable computed defaults (only for still-unset variables)
# ---------------------------------------------------------------------------
: "${XDG_DATA_HOME:=$HOME/.local/share}"
: "${XDG_CACHE_HOME:=$HOME/.cache}"

MOJO_RS_DOCKER_ROOT="${MOJO_RS_DOCKER_ROOT:-$XDG_DATA_HOME/mojo-rs/docker}"
MOJO_RS_DOCKER_HOST="${MOJO_RS_DOCKER_HOST:-unix://$MOJO_RS_DOCKER_ROOT/run/docker.sock}"
MOJO_RS_UBUNTU_IMAGE_DIR="${MOJO_RS_UBUNTU_IMAGE_DIR:-$XDG_DATA_HOME/mojo-rs/images}"
MOJO_RS_BUILD_CACHE="${MOJO_RS_BUILD_CACHE:-$XDG_CACHE_HOME/mojo-rs}"
MOJO_RS_ARTIFACT_ROOT="${MOJO_RS_ARTIFACT_ROOT:-$MOJO_RS_DOCKER_ROOT/artifacts}"
MOJO_RS_MIN_FREE_BYTES="${MOJO_RS_MIN_FREE_BYTES:-53687091200}"
MOJO_RS_MIN_FREE_BUILD_BYTES="${MOJO_RS_MIN_FREE_BUILD_BYTES:-32212254720}"
MOJO_RS_ROOTLESS_TOOLING_SOURCE="${MOJO_RS_ROOTLESS_TOOLING_SOURCE:-}"

# ---------------------------------------------------------------------------
# validation / normalization
# ---------------------------------------------------------------------------
mojo_rs_validate() {
  case "$MOJO_RS_DOCKER_ROOT" in
    /*) ;;
    *) echo "FATAL: MOJO_RS_DOCKER_ROOT must be an absolute path: $MOJO_RS_DOCKER_ROOT" >&2; exit 1 ;;
  esac
  case "$MOJO_RS_UBUNTU_IMAGE_DIR" in
    /*) ;;
    *) echo "FATAL: MOJO_RS_UBUNTU_IMAGE_DIR must be an absolute path: $MOJO_RS_UBUNTU_IMAGE_DIR" >&2; exit 1 ;;
  esac
  MOJO_RS_DOCKER_ROOT="${MOJO_RS_DOCKER_ROOT%/}"
  MOJO_RS_DOCKER_SOCK="$MOJO_RS_DOCKER_ROOT/run/docker.sock"
  MOJO_RS_DOCKER_PIDFILE="$MOJO_RS_DOCKER_ROOT/run/docker.pid"
  MOJO_RS_DAEMON_DATA="$MOJO_RS_DOCKER_ROOT/data-root"
  MOJO_RS_DAEMON_EXEC="$MOJO_RS_DOCKER_ROOT/exec-root"
  MOJO_RS_BUILDKIT_ROOT="$MOJO_RS_DOCKER_ROOT/buildkit"
  MOJO_RS_ORACLE_SOURCE_ROOT="$MOJO_RS_DOCKER_ROOT/oracle-source"
  MOJO_RS_WORK_ROOT="$MOJO_RS_DOCKER_ROOT/work"
  MOJO_RS_LOGS="$MOJO_RS_DOCKER_ROOT/logs"
  MOJO_RS_STATE="$MOJO_RS_DOCKER_ROOT/state"
}
mojo_rs_validate

# ---------------------------------------------------------------------------
# output
# ---------------------------------------------------------------------------
MOJO_RS_EXPORT_VARS="MOJO_RS_REPO_ROOT MOJO_RS_DOCKER_ROOT MOJO_RS_DOCKER_HOST \
MOJO_RS_DOCKER_SOCK MOJO_RS_DOCKER_PIDFILE MOJO_RS_DAEMON_DATA MOJO_RS_DAEMON_EXEC \
MOJO_RS_BUILDKIT_ROOT MOJO_RS_ORACLE_SOURCE_ROOT MOJO_RS_WORK_ROOT MOJO_RS_LOGS \
MOJO_RS_STATE MOJO_RS_UBUNTU_IMAGE_DIR MOJO_RS_UBUNTU_IMAGE MOJO_RS_BUILD_CACHE \
MOJO_RS_ARTIFACT_ROOT MOJO_RS_MIN_FREE_BYTES MOJO_RS_MIN_FREE_BUILD_BYTES \
MOJO_RS_ROOTLESS_TOOLING_SOURCE"

if [ "$MOJO_RS_PRINT_HUMAN" = 1 ]; then
  cat >&2 <<EOF
mojo-rs resolved configuration:
  Repository root:      $MOJO_RS_REPO_ROOT
  Docker storage root:  $MOJO_RS_DOCKER_ROOT
  Docker daemon socket: $MOJO_RS_DOCKER_HOST
  Daemon data root:     $MOJO_RS_DAEMON_DATA
  Daemon exec root:     $MOJO_RS_DAEMON_EXEC
  BuildKit root:        $MOJO_RS_BUILDKIT_ROOT
  Oracle source root:   $MOJO_RS_ORACLE_SOURCE_ROOT
  Work root:            $MOJO_RS_WORK_ROOT
  Logs:                 $MOJO_RS_LOGS
  Ubuntu image dir:     $MOJO_RS_UBUNTU_IMAGE_DIR
  Ubuntu image:         ${MOJO_RS_UBUNTU_IMAGE:-<unset; discovery>}
  Build cache root:     $MOJO_RS_BUILD_CACHE
  Artifact root:        $MOJO_RS_ARTIFACT_ROOT
  Local config file:    ${mojo_rs_local_config:-<none>}
  Min free bytes:       $MOJO_RS_MIN_FREE_BYTES
  Min free build bytes: $MOJO_RS_MIN_FREE_BUILD_BYTES
EOF
fi

if [ "$MOJO_RS_SOURCED" = 0 ]; then
  for v in $MOJO_RS_EXPORT_VARS; do
    printf '%s=%s\n' "$v" "${!v:-}"
  done
fi
