# mojo-rs-io

Executor/task integration for mojo-rs.

**Status: Scaffolded** (structure only; no behavior yet).

The core is runtime-agnostic; this crate adapts it to the embedding
application's executor without hard-wiring one async runtime.
