# ---------------------------------------------------------------------------
# mojo-rs court image — the forensic parity pipeline: oracle build, baseline,
# candidate build, candidate phase (oracle isolated), classification, and
# no-delegation proof — all inside one hermetic container.
# ---------------------------------------------------------------------------
# syntax=docker/dockerfile:1
ARG COURT_BASE_IMAGE=mojo-rs/ubuntu-base:latest
FROM ${COURT_BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC0

RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates xz-utils bzip2 file lsb-release pkg-config \
    build-essential clang lld llvm ninja-build python3 \
    jq strace gdb binutils libc6-dbg \
    libglib2.0-dev libnss3-dev libdrm-dev libxkbcommon-dev libgtk-3-dev \
    libasound2-dev libpulse-dev libpci-dev libatk-bridge2.0-dev \
    && rm -rf /var/lib/apt/lists/*

# Pinned depot_tools (for gn/ninja during the oracle phase).
ARG DEPOT_TOOLS_COMMIT=d22ef3bf62a8c3c76d9c7427015bdfec7665587a
RUN git clone https://chromium.googlesource.com/chromium/tools/depot_tools /opt/depot_tools \
    && git -C /opt/depot_tools checkout ${DEPOT_TOOLS_COMMIT} \
    && rm -rf /opt/depot_tools/.git
ENV PATH="/opt/depot_tools:${PATH}"

COPY docker/court/scripts/court-run.sh /usr/local/bin/court-run.sh
RUN chmod +x /usr/local/bin/court-run.sh

WORKDIR /work
