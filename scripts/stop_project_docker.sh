#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# stop_project_docker.sh — stop ONLY the project daemon.
#
# Scoped strictly to $MOJO_RS_DOCKER_ROOT: the pidfile, the rootlesskit state
# dir, and the socket. No other daemon (system or sibling-project) is touched.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

stopped=0

# 1. dockerd via pidfile
if [ -f "$MOJO_RS_DOCKER_PIDFILE" ]; then
  PID="$(cat "$MOJO_RS_DOCKER_PIDFILE")"
  if kill -0 "$PID" 2>/dev/null; then
    mojo_rs_info "stopping dockerd pid $PID"
    kill "$PID" 2>/dev/null || true
    for _ in $(seq 1 30); do
      kill -0 "$PID" 2>/dev/null || break
      sleep 1
    done
    kill -0 "$PID" 2>/dev/null && kill -9 "$PID" 2>/dev/null || true
    stopped=1
  else
    mojo_rs_warn "pidfile pid $PID is not alive"
  fi
  rm -f "$MOJO_RS_DOCKER_PIDFILE"
fi

# 2. rootlesskit (exact state-dir match)
RK_PIDS="$(pgrep -f "rootlesskit --state-dir=$MOJO_RS_DOCKER_ROOT/rootlesskit" 2>/dev/null || true)"
if [ -n "$RK_PIDS" ]; then
  for p in $RK_PIDS; do
    mojo_rs_info "stopping rootlesskit pid $p"
    kill "$p" 2>/dev/null || true
  done
  stopped=1
fi

# 3. socket
if [ -e "$MOJO_RS_DOCKER_SOCK" ]; then
  sleep 2
  if [ -e "$MOJO_RS_DOCKER_SOCK" ]; then
    rm -f "$MOJO_RS_DOCKER_SOCK"
    mojo_rs_info "removed project socket"
  fi
fi

if [ "$stopped" = 1 ]; then
  mojo_rs_ok "project daemon stopped"
else
  mojo_rs_info "project daemon was not running"
fi

# Confirm we did not leave anything behind.
LEFT="$(pgrep -f "MOJO_RS_DOCKER_ROOT=$MOJO_RS_DOCKER_ROOT" 2>/dev/null || true)"
LEFT2="$(pgrep -f "data-root=$MOJO_RS_DAEMON_DATA" 2>/dev/null || true)"
if [ -n "$LEFT$LEFT2" ]; then
  mojo_rs_warn "processes still matching the project daemon remain: $LEFT $LEFT2"
fi
