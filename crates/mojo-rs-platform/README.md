# mojo-rs-platform

Platform layer for mojo-rs. Linux first; the core never depends on Unix
semantics directly.

**Status: Scaffolded** (structure only; no behavior yet).

## Scope (Phase 3+)

* Unix-domain transport (socketpair, node channels).
* Descriptor transfer via `SCM_RIGHTS` (with ancillary-data validation,
  count-mismatch handling, descriptor exhaustion, partial reads/writes).
* Shared-memory objects (memfd/shm_open) and mappings.
* Nonblocking behavior, polling, and wakeups.
* Peer death detection.
* Process bootstrapping.

## Platform gates

* Linux: `Platform-sealed` required before any claim.
* Windows / macOS / Fuchsia: separate future gates — never claimed from
  portable abstractions alone.

## Unsafe policy

Unsafe code is confined to this crate's `sys` modules, each with `SAFETY:`
invariants, wrapped by safe ownership types (`OwnedFd`, `Mapping`, ...).
