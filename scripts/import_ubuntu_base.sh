#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# import_ubuntu_base.sh — import the verified Ubuntu minimal image into the
# ISOLATED project daemon as a Docker base image, hash-keyed.
#
# The source image under MOJO_RS_UBUNTU_IMAGE_DIR is treated as read-only:
# extraction happens in scratch space under $MOJO_RS_DOCKER_ROOT/tmp. The
# extracted rootfs is tarred with root ownership and `docker import`ed into the
# project daemon only (never the production daemon).
#
# Resulting tag: mojo-rs/ubuntu-base:<sha256-prefix>
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

# The admitted Ubuntu minimal base (noble server cloud image, amd64) sha256.
EXPECTED_SHA="${MOJO_RS_UBUNTU_BASE_SHA256:-18a42173dc0c9a02c8230212c978b14cc3bbcff173f95dfa954cdaaa04f4a172}"
BASE_TAG="mojo-rs/ubuntu-base:${EXPECTED_SHA:0:16}"

mojo_rs_require_cmd docker
mojo_rs_require_cmd qemu-img
mojo_rs_require_cmd sfdisk
mojo_rs_require_cmd debugfs

# Resolve the image (must be explicitly selectable; never ambiguous).
RESOLVED="$(bash "$SCRIPT_DIR/select_ubuntu_image.sh" 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['selected'])")"
GOT_SHA="$(bash "$SCRIPT_DIR/select_ubuntu_image.sh" 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['sha256'])")"

if [ "$GOT_SHA" != "$EXPECTED_SHA" ]; then
  mojo_rs_fail "base image sha256 mismatch: got $GOT_SHA expected $EXPECTED_SHA
  (the admitted base image hash changed; update MOJO_RS_UBUNTU_BASE_SHA256 only after re-admission)"
fi
mojo_rs_ok "admitted base image verified: sha256=$GOT_SHA"

mojo_rs_export_docker_env
if docker image inspect "$BASE_TAG" >/dev/null 2>&1; then
  mojo_rs_ok "base image already present: $BASE_TAG"
  exit 0
fi

WORK="$MOJO_RS_DOCKER_ROOT/tmp/ubuntu-base-${EXPECTED_SHA:0:16}"
ROOTFS_TAR="$MOJO_RS_DOCKER_ROOT/tmp/ubuntu-rootfs-${EXPECTED_SHA:0:16}.tar"
mkdir -p "$WORK"

if [ ! -f "$ROOTFS_TAR" ]; then
  mojo_rs_log "extracting the Ubuntu minimal rootfs (read-only source; one-time, cached)"
  RAW="$WORK/base.raw"
  PART="$WORK/root.part"
  ROOTFS_DIR="$WORK/rootfs"
  rm -rf "$ROOTFS_DIR"; mkdir -p "$ROOTFS_DIR"
  qemu-img convert -O raw "$RESOLVED" "$RAW"
  P1_START="$(sfdisk -d "$RAW" | awk -F'start=' '/raw1 :/{split($2,a,","); print a[1]}')"
  P1_SIZE="$(sfdisk -d "$RAW" | awk -F'size=' '/raw1 :/{split($2,a,","); print a[1]}')"
  [ -n "${P1_START:-}" ] && [ -n "${P1_SIZE:-}" ] \
    || mojo_rs_fail "cannot locate the root partition in $RESOLVED"
  dd if="$RAW" of="$PART" bs=512 skip="$P1_START" count="$P1_SIZE" conv=sparse status=none
  ( cd "$ROOTFS_DIR" && debugfs -R 'rdump / .' "$PART" >/dev/null 2>&1 || true )
  ( cd "$ROOTFS_DIR" && tar --owner=0 --group=0 --numeric-owner \
      --exclude='./var/lib/snapd/*' -cf "$ROOTFS_TAR" . ) \
    || mojo_rs_fail "rootfs tar failed"
  rm -f "$RAW" "$PART"; rm -rf "$ROOTFS_DIR"
fi

mojo_rs_log "importing base image into the ISOLATED daemon"
docker import "$ROOTFS_TAR" "$BASE_TAG" >/dev/null || mojo_rs_fail "base image import failed"
docker image inspect "$BASE_TAG" >/dev/null 2>&1 || mojo_rs_fail "base image missing after import"
mojo_rs_ok "base image imported: $BASE_TAG"
