//! # mojo-rs
//!
//! Facade crate: the native Rust implementation of Chromium Mojo IPC,
//! serialization, bindings, and the public Mojo System C ABI, developed
//! through forensic differential parity against a pinned official Chromium
//! Mojo oracle.
//!
//! Status discipline: capability claims live in `atlas/feature-matrix.json`
//! and move up the sealed ladder only with court receipts.
//!
//! The facade re-exports the runtime and language-toolchain crates:
//!
//! * [`wire`] — wire-format encoding, decoding, and validation
//! * [`core`] — native runtime: handle table, dispatchers, pipes, routing
//! * [`platform`] — Linux-first platform layer (transport, shared memory, fds)
//! * [`system`] — idiomatic safe Rust System API
//! * [`c_api`] — the compatible public Mojo C System ABI (epoch 1)
//! * [`io`] — executor/task integration without a hard-wired async runtime
//! * [`bindings`] — generated-binding runtime: remotes, receivers, control
//! * [`mojom`] — the Mojom language toolchain (lexer, parser, AST, validation)
//! * [`codegen`] — deterministic Rust code generation from mojom definitions
//!
//! Forensic tooling (casefiles, the oracle harness, interop clients, and test
//! support) is intentionally not re-exported here; depend on those crates
//! directly when building courts.
#![deny(missing_docs)]

pub use mojo_rs_bindings as bindings;
pub use mojo_rs_c_api as c_api;
pub use mojo_rs_codegen as codegen;
pub use mojo_rs_core as core;
pub use mojo_rs_io as io;
pub use mojo_rs_mojom as mojom;
pub use mojo_rs_platform as platform;
pub use mojo_rs_system as system;
pub use mojo_rs_wire as wire;

/// The pinned Chromium compatibility epoch.
pub const COMPATIBILITY_EPOCH: &str = "151.0.7922.105";

/// The pinned Chromium commit for the epoch.
pub const COMPATIBILITY_COMMIT: &str = "bfa3579138998e2fbb981725570fa588c5b6f8cd";
