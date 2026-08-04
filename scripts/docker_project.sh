#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# docker_project.sh — run a docker command against the PROJECT daemon.
#   scripts/docker_project.sh images
#   scripts/docker_project.sh compose -f ... build
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_require_cmd docker
mojo_rs_export_docker_env
exec docker "$@"
