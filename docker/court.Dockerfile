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
    sudo curl ca-certificates lsb-release pkg-config build-essential clang \
    ninja-build python3 git xz-utils bzip2 file lld llvm \
    jq strace gdb binutils libc6-dbg \
    && rm -rf /var/lib/apt/lists/*

# Canonical Chromium build dependencies (pinned revision).
ARG CHROMIUM_TAG=151.0.7922.105
RUN curl -fsSL "https://chromium.googlesource.com/chromium/src/+/refs/tags/${CHROMIUM_TAG}/build/install-build-deps.sh?format=TEXT" \
    | base64 -d > /tmp/install-build-deps.sh \
    && chmod +x /tmp/install-build-deps.sh \
    && /tmp/install-build-deps.sh --no-prompt \
    && rm -f /tmp/install-build-deps.sh

# Pinned depot_tools.
ARG DEPOT_TOOLS_COMMIT=d22ef3bf62a8c3c76d9c7427015bdfec7665587a
RUN git clone https://chromium.googlesource.com/chromium/tools/depot_tools /opt/depot_tools \
    && git -C /opt/depot_tools checkout ${DEPOT_TOOLS_COMMIT} \
    && cd /opt/depot_tools && ./update_depot_tools --no-history \
    && rm -rf /opt/depot_tools/.git
ENV PATH="/opt/depot_tools:${PATH}"

COPY docker/court/scripts/court-run.sh /usr/local/bin/court-run.sh
RUN chmod +x /usr/local/bin/court-run.sh

WORKDIR /work
