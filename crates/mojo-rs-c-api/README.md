# mojo-rs-c-api

Export of the compatible public Mojo C System ABI (epoch 1).

**Status: Scaffolded** (structure only; no behavior yet).

## Requirements (Phase 6)

* Exact symbol names, calling conventions, integer widths, struct sizes,
  alignments, enum/flag values.
* Correct option-structure version behavior (old sizes accepted).
* Correct null-pointer handling.
* No panic may cross the ABI boundary — all panics contained and converted to
  safe failure behavior.
* Ownership transitions exactly match the C contract.
* ABI courts compare exported symbol sets, header compilation, constants,
  struct sizes, field offsets, alignments, and unchanged C/C++ clients.

## Structure

* `types.rs` — mirror of the pinned C types (ground truth: `atlas/reference`).
* `symbols.rs` — `#[no_mangle] extern "C"` exports.
* `containment.rs` — panic containment (catch_unwind at the boundary).
* `versioning.rs` — option-struct version handling.
