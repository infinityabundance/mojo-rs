# ---------------------------------------------------------------------------
# mojo-rs candidate image — toolchain for building the native Rust candidate.
# ---------------------------------------------------------------------------
# syntax=docker/dockerfile:1
ARG CANDIDATE_BASE_IMAGE=mojo-rs/ubuntu-base:latest
FROM ${CANDIDATE_BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC0

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates build-essential pkg-config file \
    && rm -rf /var/lib/apt/lists/*

ARG RUST_TOOLCHAIN=1.96.0
ENV RUST_TOOLCHAIN=${RUST_TOOLCHAIN}

COPY docker/candidate/scripts/candidate-build.sh /usr/local/bin/candidate-build.sh
RUN chmod +x /usr/local/bin/candidate-build.sh

WORKDIR /repo
