# mojo-rs-bindings

Interface bindings runtime.

**Status: Scaffolded** (structure only; no behavior yet).

## Scope (Phase 7)

* Pending endpoints, remotes, receivers.
* Request/response correlation and callback completion.
* Disconnect handling, control messages, version negotiation.
* Associated remotes/receivers, interface identifiers, multiplex routing.
* Executor integration without hard-wiring one async runtime.

Interoperability gate: mixed C++/Rust interface courts in both directions.
Rust-to-Rust success alone is insufficient.
