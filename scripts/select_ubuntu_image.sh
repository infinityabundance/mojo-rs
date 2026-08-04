#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# select_ubuntu_image.sh — resolve and verify the Ubuntu minimal source image.
#
# Rules (addendum §6):
#   1. Prefer an explicit MOJO_RS_UBUNTU_IMAGE.
#   2. Otherwise inspect MOJO_RS_UBUNTU_IMAGE_DIR.
#   3. Verify file type and readability.
#   4. Record filename, size, format, mtime, and SHA-256.
#   5. Fail with a clear diagnostic when no suitable image exists.
#   6. Fail when selection would be ambiguous (never silently pick one).
#   7. The source image is NEVER modified.
#
# Emits a JSON receipt on stdout.
# ---------------------------------------------------------------------------
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/configure_local_environment.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

mojo_rs_require_cmd file
mojo_rs_require_cmd sha256sum

# Known-good Ubuntu minimal base image (noble server cloud image) default hash.
DEFAULT_EXPECTED_SHA="18a42173dc0c9a02c8230212c978b14cc3bbcff173f95dfa954cdaaa04f4a172"

candidate_ext() { # 1:filename
  case "$1" in
    *.qcow2|*.raw|*.img|*.iso|*.vmdk) return 0 ;;
    *) return 1 ;;
  esac
}

candidate_name() { # 1:filename — must look like an Ubuntu image
  case "$1" in
    *ubuntu*|*noble*|*jammy*|*focal*|*plucky*|*questing*) return 0 ;;
    *) return 1 ;;
  esac
}

SELECTED=""
SELECTED_SHA=""
SELECTED_SIZE=""
SELECTED_FORMAT=""
SELECTED_MTIME=""

if [ -n "${MOJO_RS_UBUNTU_IMAGE:-}" ]; then
  IMG="$MOJO_RS_UBUNTU_IMAGE"
  [ -f "$IMG" ] || mojo_rs_fail "MOJO_RS_UBUNTU_IMAGE not found: $IMG"
  [ -r "$IMG" ] || mojo_rs_fail "MOJO_RS_UBUNTU_IMAGE not readable: $IMG"
  SELECTED="$IMG"
else
  DIR="$MOJO_RS_UBUNTU_IMAGE_DIR"
  [ -d "$DIR" ] || mojo_rs_fail "MOJO_RS_UBUNTU_IMAGE_DIR does not exist: $DIR"
  [ -r "$DIR" ] || mojo_rs_fail "MOJO_RS_UBUNTU_IMAGE_DIR not readable: $DIR"

  CANDIDATES=()
  while IFS= read -r -d '' f; do
    CANDIDATES+=("$f")
  done < <(find "$DIR" -maxdepth 1 -type f \( -name '*.qcow2' -o -name '*.raw' -o -name '*.img' -o -name '*.iso' -o -name '*.vmdk' \) -print0 2>/dev/null | sort -z)

  SUITABLE=()
  for f in "${CANDIDATES[@]}"; do
    base="$(basename "$f")"
    if candidate_name "$base"; then
      SUITABLE+=("$f")
    fi
  done

  if [ "${#SUITABLE[@]}" -eq 0 ]; then
    mojo_rs_fail "no Ubuntu image found under $DIR
  (set MOJO_RS_UBUNTU_IMAGE to an absolute path to select explicitly)"
  fi
  if [ "${#SUITABLE[@]}" -gt 1 ]; then
    printf 'FATAL: ambiguous Ubuntu image selection under %s:\n' "$DIR" >&2
    for f in "${SUITABLE[@]}"; do printf '  %s\n' "$f" >&2; done
    printf 'Set MOJO_RS_UBUNTU_IMAGE to select one explicitly. Never silent selection.\n' >&2
    exit 1
  fi
  SELECTED="${SUITABLE[0]}"
fi

# --- verify -----------------------------------------------------------------
mojo_rs_log "selected image: $SELECTED"
SELECTED_SIZE="$(stat -c %s "$SELECTED")"
SELECTED_MTIME="$(stat -c %y "$SELECTED")"
SELECTED_FORMAT="$(file -b "$SELECTED")"
mojo_rs_info "size: $SELECTED_SIZE bytes; mtime: $SELECTED_MTIME; format: $SELECTED_FORMAT"

mojo_rs_info "computing SHA-256 (may take a moment for large images)..."
SELECTED_SHA="$(mojo_rs_sha256 "$SELECTED")"
mojo_rs_info "sha256: $SELECTED_SHA"

# Emit the receipt.
cat <<EOF
{
  "schema_version": 1,
  "selected": "$SELECTED",
  "size_bytes": $SELECTED_SIZE,
  "mtime": "$SELECTED_MTIME",
  "format": "$SELECTED_FORMAT",
  "sha256": "$SELECTED_SHA",
  "default_expected_sha256": "$DEFAULT_EXPECTED_SHA",
  "readonly_policy": "the source image is never modified by mojo-rs"
}
EOF

# Note: the DEFAULT_EXPECTED_SHA is advisory; strict equality is enforced by the
# caller when a specific admitted base is required (see import_ubuntu_base.sh).
