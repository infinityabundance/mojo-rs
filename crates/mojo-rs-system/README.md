# mojo-rs-system

Idiomatic safe Rust System API above the native core.

**Status: Scaffolded** (structure only; no behavior yet).

## Design

Rust ownership prevents the failure modes the C API can only document:

* Double closing — impossible by construction (`Drop`).
* Use after transfer — `transfer()` consumes the handle.
* Use after unmap / after message consumption — consumed by the type system.
* Aliasing mutable shared-memory mappings — shared mapping types are `&`-only
  or explicitly `Arc`-guarded.

The safe API is not weakened to imitate C ergonomics. Compatibility is the
job of `mojo-rs-c-api`.
