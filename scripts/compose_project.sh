#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# compose_project.sh — run docker compose against the PROJECT daemon using the
# portable docker/docker-compose.yml.
#   scripts/compose_project.sh build
#   scripts/compose_project.sh run --rm court
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_require_cmd docker
mojo_rs_export_docker_env
COMPOSE_FILE="$MOJO_RS_REPO_ROOT/docker/docker-compose.yml"
exec docker compose \
  --project-name mojo-rs \
  --project-directory "$MOJO_RS_REPO_ROOT" \
  -f "$COMPOSE_FILE" \
  "$@"
