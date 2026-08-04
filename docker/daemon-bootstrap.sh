#!/usr/bin/env bash
# daemon-bootstrap.sh — inside-userns bootstrap for the isolated rootless
# dockerd of mojo-rs. Exec'd as the rootlesskit child (uid 0 inside the
# rootless user namespace). Repairs /run/{docker,containerd} (copy-up'd
# read-only symlinks from the host) and execs dockerd with project-scoped
# roots, pidfile and socket — all under $MOJO_RS_DOCKER_ROOT.
set -eu

export DOCKER_IGNORE_BR_NETFILTER_ERROR=1

PROJECT_DOCKER_ROOT="${MOJO_RS_DOCKER_ROOT:?MOJO_RS_DOCKER_ROOT must be set}"

rm -f /run/docker /run/containerd
mkdir -p /run/docker/plugins /run/containerd/s

exec dockerd \
  --data-root="$PROJECT_DOCKER_ROOT/data-root" \
  --exec-root="$PROJECT_DOCKER_ROOT/exec-root" \
  --pidfile="$PROJECT_DOCKER_ROOT/run/docker.pid" \
  --host="unix://$PROJECT_DOCKER_ROOT/run/docker.sock" \
  --storage-driver=overlay2 \
  --userland-proxy=false \
  "$@"
