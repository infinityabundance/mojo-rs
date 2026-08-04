# mojo-rs

> **mojo-rs** is a native Rust implementation of Chromium Mojo IPC — serialization,
> bindings, and the public Mojo System C ABI — developed through forensic
> differential parity against a pinned official Chromium Mojo oracle.

**Status: implementation in progress.** This project does **not** yet claim to be
a drop-in replacement. Every capability is labelled with a precise status from
the sealed ladder below; unsealed claims are not made. See
[`atlas/feature-matrix.json`](atlas/feature-matrix.json) for the authoritative
capability matrix and [`WORKLOG.md`](WORKLOG.md) for the current cycle report.

## Status ladder

| Label | Meaning |
|---|---|
| Scaffolded | Structure exists; no behavior. |
| Parsed | Inputs/spec understood; inventory recorded. |
| Implemented | Behavior exists in the candidate. |
| Unit-tested | Candidate behavior covered by candidate tests. |
| Oracle-compared | Differential observations against the pinned oracle recorded. |
| Interoperable | Bidirectional exchange with official implementations demonstrated. |
| ABI-compatible | Symbols/layouts/calling conventions verified against unchanged clients. |
| Stress-sealed | Concurrency/fuzz/leak evidence closed. |
| Platform-sealed | Native platform receipts closed on the declared platform. |
| Drop-in sealed | Full §15 criteria met for the declared epoch/platform. |

A passing unit test is not parity. A self-round-trip is not interoperability.
An API stub returning success is worse than an explicit unsupported error.

## Repository layout

```text
atlas/        machine-readable cartography of the pinned Chromium epoch
casefiles/    replayable case files (schema, generated, curated, adversarial)
courts/       court definitions and manifests
crates/       the Rust workspace (mojo-rs-* crates)
docker/       hermetic laboratory (Dockerfiles, compose, daemon bootstrap)
docs/         long-form engineering notes and policies
evidence/     manifests, receipts, curated residuals
oracle/       oracle driver sources (C++ harness + patches, pinned)
scripts/      environment, daemon, court, and verification tooling
tools/        host-side fetch/verify helpers
```

## Quick start

```bash
# 1. One-time local configuration (copy the template, edit for your host)
cp .env.example config/local.env   # then set MOJO_RS_DOCKER_ROOT etc.

# 2. Verify environment and storage, start the isolated project Docker daemon
scripts/verify_environment.sh
scripts/configure_local_environment.sh
scripts/start_project_docker.sh
scripts/verify_storage_layout.sh

# 3. One-command clean reproduction (env -> daemon -> oracle -> candidate -> court)
scripts/reproduce_all.sh
```

See [`docs/quickstart.md`](docs/quickstart.md) for details.

## Pinned compatibility epoch

| Item | Value |
|---|---|
| Chromium tag | `151.0.7922.105` |
| Chromium commit | `bfa3579138998e2fbb981725570fa588c5b6f8cd` |
| Linux stable platform | x86_64 |
| Rust toolchain | 1.96.0 (MSRV 1.85.1) |

See [`atlas/pins.json`](atlas/pins.json).

## Non-negotiables

* No linkage to, loading of, or delegation to official Mojo Core.
* No mechanical C++→Rust transliteration.
* Every substantive compatibility claim points to a reproducible receipt.
* The oracle is physically and logically separate from the candidate.
* No panic may cross the C ABI boundary.

## License

MIT OR Apache-2.0.
