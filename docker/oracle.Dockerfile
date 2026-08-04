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

# System build toolchain for the pinned Chromium revision.
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates xz-utils bzip2 file lsb-release pkg-config \
    build-essential clang lld llvm ninja-build python3 python3-distutils \
    libglib2.0-dev libnss3-dev libdrm-dev libxkbcommon-dev libgtk-3-dev \
    libasound2-dev libpulse-dev libpci-dev libatk-bridge2.0-dev \
    && rm -rf /var/lib/apt/lists/*

# Pinned depot_tools (rolling tooling repo; commit pinned in atlas/pins.json).
ARG DEPOT_TOOLS_COMMIT=d22ef3bf62a8c3c76d9c7427015bdfec7665587a
RUN git clone https://chromium.googlesource.com/chromium/tools/depot_tools /opt/depot_tools \
    && git -C /opt/depot_tools checkout ${DEPOT_TOOLS_COMMIT} \
    && rm -rf /opt/depot_tools/.git
ENV PATH="/opt/depot_tools:${PATH}"

COPY docker/oracle/scripts/oracle-build.sh /usr/local/bin/oracle-build.sh
RUN chmod +x /usr/local/bin/oracle-build.sh

WORKDIR /work
