# mojo-rs-wire

Wire-format encoding, decoding, and validation for Chromium Mojo messages.

**Status: Scaffolded** (structure only; no behavior yet).

## Scope (Phase 1)

* Message headers (v0/v1/v2/v3 layouts — see `atlas/wire/wire-format.json`).
* Relative pointers, struct/array/map/string/union encoding.
* Handle and interface/associated-endpoint encoding.
* Checked size calculations, alignment, bounds, and versioning.
* Malformed-input rejection with exact error classification.
* Golden fixtures and differential courts against the pinned C++ and
  official Rust-generated traffic.

## Security contract

* No unchecked offset arithmetic.
* No trusting lengths from untrusted messages.
* No pointer interpretation before complete validation.
* No panics on malformed wire input.
* No unbounded allocation based on attacker-controlled lengths.

## Modules

* `layout` — wire type layouts (ground truth from `atlas/reference/wire`).
* `pointer` — relative pointers and encoding/decoding.
* `message` — message model: header, payload, handles.
* `value` — typed value encoding (structs, arrays, maps, strings, unions).
* `validate` — the validation engine with exact error classification.
* `error` — wire error taxonomy.
