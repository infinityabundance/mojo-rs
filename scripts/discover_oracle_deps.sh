#!/usr/bin/env bash
# discover_oracle_deps.sh — iterative gn-gen probe that discovers the exact
# pkg-config packages the pinned Chromium gn graph requires and installs them
# via apt until `gn gen` succeeds. Outputs the discovered package set as a
# receipt (both human-readable and apt-installable list).
#
# Intended to run inside the oracle image with the pinned source bind-mounted
# read-only at /work/oracle-source/src (see docker/oracle.Dockerfile).
set -uo pipefail

SRC=/work/oracle-source/src
OUT=/tmp/gn-out
GN="$SRC/buildtools/linux64/gn"
ARGS='is_debug=false is_component_build=false use_custom_libcxx=false v8_enable_sandbox=false symbol_level=0 enable_nacl=false treat_warnings_as_errors=false use_sysroot=false'

mkdir -p "$OUT"
cd "$SRC"

if ! apt-get update >>/tmp/apt.log 2>&1; then
  echo "!!! apt-get update failed"
  tail -n 20 /tmp/apt.log
  exit 5
fi

INSTALLED=()
declare -A PKG2APT=(
  # explicit pkg-config-name -> Ubuntu noble apt package mappings.
  # Names are the strings passed to build/config/linux/pkg-config.py.
  ["libpipewire-0.3"]="libpipewire-0.3-dev"
  ["atspi2"]="libatspi2.0-dev"
  ["colord"]="libcolord-dev"
  ["dav1d"]="libdav1d-dev"
  ["dbus"]="libdbus-1-dev"
  ["dri"]="mesa-common-dev"
  ["egl"]="libegl1-mesa-dev"
  ["gbm"]="libgbm-dev"
  ["gdk_pixbuf"]="libgdk-pixbuf-2.0-dev"
  ["gio"]="libglib2.0-dev"
  ["gl"]="libgl1-mesa-dev"
  ["glib"]="libglib2.0-dev"
  ["graphene"]="libgraphene-1.0-dev"
  ["gtk_config"]="libgtk-3-dev"
  ["lcms2"]="liblcms2-dev"
  ["libdrm"]="libdrm-dev"
  ["libevdev"]="libevdev-dev"
  ["libffi"]="libffi-dev"
  ["libgbm"]="libgbm-dev"
  ["libinput"]="libinput-dev"
  ["libopenjpeg2"]="libopenjp2-7-dev"
  ["libpulse"]="libpulse-dev"
  ["libsystemd"]="libsystemd-dev"
  ["libudev"]="libudev-dev"
  ["libva"]="libva-dev"
  ["libvpx"]="libvpx-dev"
  ["mtdev"]="libmtdev-dev"
  ["nss"]="libnss3-dev"
  ["pangocairo"]="libpango1.0-dev"
  ["pcre2"]="libpcre2-dev"
  ["pixman"]="libpixman-1-dev"
  ["wayland_client_config"]="libwayland-dev"
  ["wayland_cursor"]="libwayland-dev"
  ["wayland_cursor_config"]="libwayland-dev"
  ["wayland_egl"]="libwayland-dev"
  ["wayland_egl_config"]="libwayland-dev"
  ["wayland_server_config"]="libwayland-dev"
  ["xkbcommon"]="libxkbcommon-dev"
  ["atk"]="libatk1.0-dev"
  ["cairo"]="libcairo2-dev"
  ["freetype_from_pkgconfig"]="libfreetype-dev"
  ["harfbuzz_from_pkgconfig"]="libharfbuzz-dev"
  ["lcms2_from_pkgconfig"]="liblcms2-dev"
  ["libopenjpeg2_from_pkgconfig"]="libopenjp2-7-dev"
  ["libpulse"]="libpulse-dev"
  ["libvpx"]="libvpx-dev"
  ["libyuv"]="libyuv-dev"
)

extract_missing_pkg() {
  # The gn error block looks like:
  #   Command: python3 .../pkg-config.py [flags] <name>
  #   Returned 1.
  # Flags like --dridriverdir may precede the package name; skip them.
  grep -A2 "Command: python3 .*pkg-config.py" /tmp/gen.log \
    | grep -oP 'pkg-config\.py \K.*$' \
    | tr ' ' '\n' \
    | grep -v '^--' | head -1 || true
}

resolve_apt_pkg() {
  local name="$1"
  if [ -n "${PKG2APT[$name]:-}" ]; then
    echo "${PKG2APT[$name]}"
    return 0
  fi
  # Heuristic fallbacks, logged loudly so the mapping table stays complete.
  case "$name" in
    lib*) echo "${name}-dev" ;;
    *)    echo "lib${name}-dev" ;;
  esac
}

for i in $(seq 1 60); do
  echo "=== gn-gen probe iteration $i ==="
  if "$GN" gen "$OUT" --args="$ARGS" >/tmp/gen.log 2>&1; then
    echo "GN GEN SUCCESS after $i iterations"
    printf '%s\n' "${INSTALLED[@]:-}" | sort -u > /tmp/discovered-pkgs.txt
    echo "--- discovered packages ---"
    cat /tmp/discovered-pkgs.txt
    exit 0
  fi
  MISSING="$(extract_missing_pkg)"
  if [ -z "$MISSING" ]; then
    echo "!!! could not parse missing pkg-config name; gen.log tail:"
    tail -n 30 /tmp/gen.log
    exit 2
  fi
  APT="$(resolve_apt_pkg "$MISSING")"
  echo "missing pkg-config: $MISSING -> apt: $APT"
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$APT" >>/tmp/apt.log 2>&1 \
    || { echo "!!! apt install failed for $APT (from pkg-config name $MISSING)"; tail -n 20 /tmp/apt.log; exit 3; }
  INSTALLED+=("$APT")
done

echo "!!! exhausted iteration budget"
exit 4
