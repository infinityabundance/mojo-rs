#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# verify_storage_layout.sh — prove that mojo-rs Docker storage is using the
# intended locations (addendum §8). Fails rather than warns when Docker storage
# resolves contrary to the configured host policy.
#
# Emits a storage receipt JSON on stdout and writes a copy to
# $MOJO_RS_STATE/storage-receipt.json.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_require_cmd docker

# The project daemon is the ONLY daemon this script may talk to.
mojo_rs_export_docker_env

FAIL=0
fail() { printf '  FAIL: %s\n' "$*" >&2; FAIL=1; }
ok() { printf '  OK: %s\n' "$*" >&2; }
note() { printf '  NOTE: %s\n' "$*" >&2; }

# Paths that count as "the system drive" — Docker mutable state must never be
# under them (beneath the configured project root).
SYSTEM_DRIVE_MARKERS="/ /home /var /tmp"

mkdir -p "$MOJO_RS_STATE"
mkdir -p "$MOJO_RS_DOCKER_ROOT/run" "$MOJO_RS_DOCKER_ROOT/tmp" "$MOJO_RS_DOCKER_ROOT/buildkit" "$MOJO_RS_DOCKER_ROOT/data-root" "$MOJO_RS_DOCKER_ROOT/exec-root" "$MOJO_RS_DOCKER_ROOT/logs"

mojo_rs_log "storage verification"

# ---- 0. configuration sanity -------------------------------------------------------------
[ -d "$MOJO_RS_DOCKER_ROOT" ] || fail "project docker root missing: $MOJO_RS_DOCKER_ROOT"
[ -w "$MOJO_RS_DOCKER_ROOT" ] || fail "project docker root not writable: $MOJO_RS_DOCKER_ROOT"

# Broken-symlink detection for every configured path.
for p in "$MOJO_RS_DOCKER_ROOT" "$MOJO_RS_UBUNTU_IMAGE_DIR" "$MOJO_RS_ARTIFACT_ROOT"; do
  if [ -e "$p" ] || [ -L "$p" ]; then
    mojo_rs_realpath "$p" >/dev/null || fail "unresolvable path (broken symlink?): $p"
  fi
done

# ---- 1. project daemon is the daemon in use ---------------------------------------------
DOCKER_OK=0
DOCKER_REPORTED_ROOT=""
if [ -n "${DOCKER_HOST:-}" ]; then
  case "$DOCKER_HOST" in
    "unix:///var/run/docker.sock"|"unix:///run/docker.sock")
      fail "DOCKER_HOST points at the production socket: $DOCKER_HOST" ;;
  esac
fi
if docker info >/dev/null 2>&1; then
  DOCKER_OK=1
  DOCKER_REPORTED_ROOT="$(docker info --format '{{.DockerRootDir}}' 2>/dev/null || echo "")"
  # The project daemon is required — the socket must be the configured one.
  case "${DOCKER_HOST:-}" in
    "$MOJO_RS_DOCKER_HOST")
      ok "project socket in use: $DOCKER_HOST" ;;
    *)
      fail "DOCKER_HOST (${DOCKER_HOST:-<unset>}) != configured project socket ($MOJO_RS_DOCKER_HOST)" ;;
  esac
else
  fail "project daemon not reachable (run scripts/start_project_docker.sh)"
fi

# ---- 2. docker reported root beneath the project data root --------------------------------
CANON=""
if [ "$DOCKER_OK" = 1 ]; then
  CANON="$(mojo_rs_normalize_docker_root "$DOCKER_REPORTED_ROOT")"
  case "$CANON" in
    "$MOJO_RS_DAEMON_DATA"*)
      ok "docker data root beneath project data root: $CANON" ;;
    *)
      fail "docker data root $CANON is NOT beneath $MOJO_RS_DAEMON_DATA" ;;
  esac
  # No Docker mutable state on the system drive.
  case "$CANON" in
    /run/media/*) ok "docker data root is on removable storage" ;;
    *)
      for m in $SYSTEM_DRIVE_MARKERS; do
        case "$CANON" in
          "$m"/*|"$m") fail "docker data root looks like system-drive storage: $CANON" ;;
        esac
      done
      ;;
  esac
fi

# ---- 3. BuildKit / buildx state -----------------------------------------------------------
if [ "$DOCKER_OK" = 1 ]; then
  if [ -d "$MOJO_RS_BUILDKIT_ROOT" ]; then
    ok "buildkit root present: $MOJO_RS_BUILDKIT_ROOT"
  else
    fail "buildkit root missing: $MOJO_RS_BUILDKIT_ROOT"
  fi
  if [ -n "${BUILDX_CONFIG:-}" ]; then
    case "$BUILDX_CONFIG" in
      "$MOJO_RS_BUILDKIT_ROOT"*) ok "BUILDX_CONFIG beneath project root" ;;
      *) fail "BUILDX_CONFIG off-project: $BUILDX_CONFIG" ;;
    esac
  fi
fi

# ---- 4. named volumes reside beneath the project daemon data root --------------------------
if [ "$DOCKER_OK" = 1 ]; then
  VOLS="$(docker volume ls -q 2>/dev/null | grep -E '^mojo-rs' || true)"
  for v in $VOLS; do
    MP="$(docker volume inspect -f '{{.Mountpoint}}' "$v" 2>/dev/null || echo "")"
    case "$MP" in
      "$MOJO_RS_DAEMON_DATA"*) ok "volume $v at $MP" ;;
      *) fail "volume $v mountpoint off-project: $MP" ;;
    esac
  done
fi

# ---- 5. images are registered with the isolated daemon ------------------------------------
if [ "$DOCKER_OK" = 1 ]; then
  IMGS="$(docker images --format '{{.Repository}}:{{.Tag}}' 2>/dev/null | grep -E '^mojo-rs/' || true)"
  [ -n "$IMGS" ] && ok "mojo-rs images present in the isolated daemon:" && for i in $IMGS; do note "  $i"; done
fi

# ---- 6. no mojo-rs containers on the HOST default daemon ----------------------------------
if [ -S /var/run/docker.sock ] && docker -H unix:///var/run/docker.sock info >/dev/null 2>&1; then
  HOST_MOJO="$(docker -H unix:///var/run/docker.sock ps -a --format '{{.Names}}' 2>/dev/null | grep -E '^mojo-rs' || true)"
  if [ -n "$HOST_MOJO" ]; then
    fail "mojo-rs containers found on the HOST default daemon: $HOST_MOJO"
  else
    ok "no mojo-rs containers on the host default daemon"
  fi
else
  note "host default daemon not inspectable (no docker group access); skipped"
fi

# ---- 7. Ubuntu source image unmodified ----------------------------------------------------
UBUNTU_STATE="$MOJO_RS_STATE/ubuntu-image.sha256"
RESOLVED=""
if [ -n "${MOJO_RS_UBUNTU_IMAGE:-}" ] && [ -f "$MOJO_RS_UBUNTU_IMAGE" ]; then
  RESOLVED="$MOJO_RS_UBUNTU_IMAGE"
elif [ -f "$MOJO_RS_STATE/ubuntu-image.path" ]; then
  RESOLVED="$(cat "$MOJO_RS_STATE/ubuntu-image.path")"
fi
UBUNTU_SHA=""
if [ -n "$RESOLVED" ] && [ -f "$RESOLVED" ]; then
  UBUNTU_SHA="$(mojo_rs_sha256 "$RESOLVED")"
  if [ -f "$UBUNTU_STATE" ]; then
    PREV="$(cat "$UBUNTU_STATE")"
    if [ "$PREV" = "$UBUNTU_SHA" ]; then
      ok "ubuntu source image unmodified (sha256 $UBUNTU_SHA)"
    else
      fail "ubuntu source image CHANGED since baseline: $PREV -> $UBUNTU_SHA"
    fi
  else
    printf '%s' "$UBUNTU_SHA" > "$UBUNTU_STATE"
    printf '%s' "$RESOLVED" > "$MOJO_RS_STATE/ubuntu-image.path"
    note "recorded ubuntu source image baseline sha256"
  fi
else
  note "no ubuntu image selected yet; baseline deferred"
fi

# ---- 8. writable VM overlays / mounts ------------------------------------------------------
for d in "$MOJO_RS_DOCKER_ROOT/tmp" "$MOJO_RS_ARTIFACT_ROOT"; do
  mkdir -p "$d" 2>/dev/null || true
  [ -w "$d" ] || fail "not writable: $d"
done
ok "project writable dirs verified"

# ---- 9. free space -------------------------------------------------------------------------
MIN_FREE="$MOJO_RS_MIN_FREE_BYTES"
MIN_FREE_BUILD="$MOJO_RS_MIN_FREE_BUILD_BYTES"
AVAIL_DOCKER="$(mojo_rs_free_bytes "$MOJO_RS_DOCKER_ROOT")"
AVAIL_ART="$(mojo_rs_free_bytes "$MOJO_RS_ARTIFACT_ROOT")"
[ "$AVAIL_DOCKER" -ge "$MIN_FREE" ] || fail "free space on docker root: $AVAIL_DOCKER < $MIN_FREE"
[ "$AVAIL_ART" -ge "$MIN_FREE_BUILD" ] || fail "free space on artifact root: $AVAIL_ART < $MIN_FREE_BUILD"
ok "free space: docker root $AVAIL_DOCKER bytes, artifact root $AVAIL_ART bytes"

# ---- receipt -------------------------------------------------------------------------------
STORAGE_PASSED=0
[ "$FAIL" = 0 ] && STORAGE_PASSED=1
RECEIPT="$MOJO_RS_STATE/storage-receipt.json"
cat > "$RECEIPT" <<EOF
{
  "schema_version": 1,
  "generated_at": "$(mojo_rs_now_utc)",
  "docker_root": "$MOJO_RS_DOCKER_ROOT",
  "docker_host": "$MOJO_RS_DOCKER_HOST",
  "docker_reported_root_dir": "$DOCKER_REPORTED_ROOT",
  "docker_normalized_root_dir": "$CANON",
  "ubuntu_image": "${RESOLVED:-}",
  "ubuntu_image_sha256": "$UBUNTU_SHA",
  "build_cache_root": "$MOJO_RS_BUILD_CACHE",
  "artifact_root": "$MOJO_RS_ARTIFACT_ROOT",
  "free_docker_bytes": $AVAIL_DOCKER,
  "free_artifact_bytes": $AVAIL_ART,
  "min_free_bytes": $MIN_FREE,
  "min_free_build_bytes": $MIN_FREE_BUILD,
  "storage_policy_passed": $([ "$STORAGE_PASSED" = 1 ] && echo true || echo false)
}
EOF
cat "$RECEIPT"

if [ "$STORAGE_PASSED" = 1 ]; then
  mojo_rs_ok "storage layout verified"
else
  mojo_rs_fail "storage layout verification FAILED (see diagnostics above)"
fi
