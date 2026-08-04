# ---------------------------------------------------------------------------
# mojo-rs oracle image — toolchain for building the pinned official Chromium
# Mojo oracle (CoreIpcz epoch) plus the oracle test driver.
#
# The pinned Chromium SOURCE is NOT baked into this image: it is fetched,
# verified, and patched on the host (scripts/fetch_oracle_source.sh) and bind-
# mounted read-only at /work/oracle-source. Build outputs persist at
# /work/oracle-build. This keeps image builds fast and incremental.
# ---------------------------------------------------------------------------
# syntax=docker/dockerfile:1
ARG ORACLE_BASE_IMAGE=mojo-rs/ubuntu-base:latest
FROM ${ORACLE_BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC0

# Base build toolchain.
RUN apt-get update && apt-get install -y --no-install-recommends \
    sudo curl ca-certificates lsb-release pkg-config build-essential clang \
    ninja-build python3 git xz-utils bzip2 file lld llvm \
    && rm -rf /var/lib/apt/lists/*

# Canonical Chromium build dependencies for the pinned revision. Note that the
# pinned build/install-build-deps.sh is only a wrapper that execs its sibling
# install-build-deps.py from the same directory, so BOTH files must be fetched.
# --no-chromeos-fonts skips a nonessential Google-storage download.
#
# After the canonical set we install the packages the pinned gn graph requires
# that the script does not cover (discovered empirically via
# scripts/discover_oracle_deps.sh against this pinned revision):
#   libpipewire-0.3-dev  -> remoting/host/linux pkg_config("pipewire_config")
#   mesa-common-dev      -> dri.pc (dridriverdir query) via content/gpu
#   libva-dev            -> libva pkg_config via media/gpu
ARG CHROMIUM_COMMIT=bfa3579138998e2fbb981725570fa588c5b6f8cd
ARG CHROMIUM_TAG=151.0.7922.105
RUN curl -fsSL "https://chromium.googlesource.com/chromium/src/+/refs/tags/${CHROMIUM_TAG}/build/install-build-deps.sh?format=TEXT" \
        | base64 -d > /tmp/install-build-deps.sh \
    && curl -fsSL "https://chromium.googlesource.com/chromium/src/+/refs/tags/${CHROMIUM_TAG}/build/install-build-deps.py?format=TEXT" \
        | base64 -d > /tmp/install-build-deps.py \
    && chmod +x /tmp/install-build-deps.sh /tmp/install-build-deps.py \
    && /tmp/install-build-deps.sh --no-prompt --no-chromeos-fonts \
    && apt-get install -y --no-install-recommends \
        libpipewire-0.3-dev mesa-common-dev libva-dev \
    && rm -f /tmp/install-build-deps.sh /tmp/install-build-deps.py \
    && rm -rf /var/lib/apt/lists/*

# Pinned depot_tools (rolling tooling repo; commit pinned in atlas/pins.json).
ARG DEPOT_TOOLS_COMMIT=d22ef3bf62a8c3c76d9c7427015bdfec7665587a
RUN git clone https://chromium.googlesource.com/chromium/tools/depot_tools /opt/depot_tools \
    && git -C /opt/depot_tools checkout ${DEPOT_TOOLS_COMMIT} \
    && cd /opt/depot_tools && ./update_depot_tools --no-history \
    && rm -rf /opt/depot_tools/.git
ENV PATH="/opt/depot_tools:${PATH}"

COPY docker/oracle/scripts/oracle-build.sh /usr/local/bin/oracle-build.sh
RUN chmod +x /usr/local/bin/oracle-build.sh

WORKDIR /work
