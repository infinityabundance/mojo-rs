#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# start_project_docker.sh — start (or connect to) the isolated project Docker
# daemon for mojo-rs. All daemon state lives under $MOJO_RS_DOCKER_ROOT
# (data-root, exec-root, buildkit, run/, logs/, tmp/).
#
# The host default daemon is NEVER used. This script fails loudly if the
# resolved daemon is not the project daemon.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_log "project Docker daemon at $MOJO_RS_DOCKER_HOST"
mojo_rs_require_cmd docker
mojo_rs_require_cmd rootlesskit

# Mandatory sub-layout under the project docker root.
for sub in data-root exec-root buildkit run logs tmp bin state work; do
  mkdir -p "$MOJO_RS_DOCKER_ROOT/$sub"
done

export PATH="$MOJO_RS_DOCKER_ROOT/bin:$PATH"
mojo_rs_export_docker_env

# ---------------------------------------------------------------------------
# Is the project daemon already reachable?
# ---------------------------------------------------------------------------
if docker info >/dev/null 2>&1; then
  DROOT="$(docker info --format '{{.DockerRootDir}}' 2>/dev/null || echo "")"
  CANON="$(mojo_rs_normalize_docker_root "$DROOT")"
  case "$CANON" in
    "$MOJO_RS_DAEMON_DATA"*)
      mojo_rs_ok "project daemon already running (root: $CANON)"
      exit 0
      ;;
    *)
      mojo_rs_fail "a daemon is reachable at $MOJO_RS_DOCKER_HOST but its root
  ($CANON) is NOT under $MOJO_RS_DAEMON_DATA — refusing to use it"
      ;;
  esac
fi

# ---------------------------------------------------------------------------
# Start the isolated rootless daemon.
# ---------------------------------------------------------------------------
mojo_rs_log "starting isolated rootless dockerd"

# Stale socket/pidfile from a dead daemon must not block startup.
if [ -f "$MOJO_RS_DOCKER_PIDFILE" ]; then
  OLDPID="$(cat "$MOJO_RS_DOCKER_PIDFILE")"
  if ! kill -0 "$OLDPID" 2>/dev/null; then
    rm -f "$MOJO_RS_DOCKER_PIDFILE" "$MOJO_RS_DOCKER_SOCK"
    mojo_rs_warn "removed stale pidfile/socket from dead daemon pid $OLDPID"
  fi
fi
rm -f "$MOJO_RS_DOCKER_SOCK"

# Rootless dockerd cannot see bridge-nf (standard rootless workaround).
export DOCKER_IGNORE_BR_NETFILTER_ERROR=1
export DOCKERD_ROOTLESS_ROOTLESSKIT=1
export DOCKERD_ROOTLESS_ROOTLESSKIT_NET=slirp4netns

nohup rootlesskit \
  --state-dir="$MOJO_RS_DOCKER_ROOT/rootlesskit" \
  --net=slirp4netns \
  --slirp4netns-sandbox=true \
  --disable-host-loopback \
  --copy-up=/etc \
  --copy-up=/run \
  -- "$MOJO_RS_REPO_ROOT/docker/daemon-bootstrap.sh" \
  > "$MOJO_RS_LOGS/dockerd.log" 2>&1 &
RK_PID=$!
mojo_rs_info "rootlesskit pid $RK_PID (log: $MOJO_RS_LOGS/dockerd.log)"

# Wait for readiness.
for _ in $(seq 1 90); do
  docker info >/dev/null 2>&1 && break
  sleep 2
done
docker info >/dev/null 2>&1 || {
  tail -40 "$MOJO_RS_LOGS/dockerd.log" >&2 || true
  mojo_rs_fail "isolated daemon did not start (see $MOJO_RS_LOGS/dockerd.log)"
}

# ---------------------------------------------------------------------------
# Post-start verification: must be OUR daemon on OUR storage.
# ---------------------------------------------------------------------------
DRIVER="$(docker info --format '{{.Driver}}' 2>/dev/null || echo "")"
DROOT="$(docker info --format '{{.DockerRootDir}}' 2>/dev/null || echo "")"
CANON="$(mojo_rs_normalize_docker_root "$DROOT")"
case "$CANON" in
  "$MOJO_RS_DAEMON_DATA"*)
    mojo_rs_ok "project daemon up: driver=$DRIVER root=$CANON"
    ;;
  *)
    mojo_rs_fail "daemon root $CANON is not under $MOJO_RS_DAEMON_DATA — aborting"
    ;;
esac

PID="$(cat "$MOJO_RS_DOCKER_PIDFILE" 2>/dev/null || echo "unknown")"
mojo_rs_ok "socket: $MOJO_RS_DOCKER_HOST  pid: $PID"
mojo_rs_info "use scripts/docker_project.sh or scripts/compose_project.sh for all docker commands"
