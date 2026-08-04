# mojo-rs-oracle

Oracle-side harness tooling.

**Status: Scaffolded** (structure only; no behavior yet).

This crate contains NO candidate runtime code and MUST NOT depend on
mojo-rs-core/wire/system/platform/c-api. It talks to the official oracle
driver binary (built from the pinned Chromium revision, see `oracle/`) through
the casefile protocol and emits machine-readable events.

The oracle driver itself is C++ (in `oracle/driver/`), compiled inside the
pinned Chromium tree by a deterministic patch. This crate provides the Rust
side: casefile replay client, event capture, process management.
