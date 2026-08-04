//! # mojo-rs
//!
//! Facade crate: the native Rust implementation of Chromium Mojo IPC,
//! serialization, bindings, and the public Mojo System C ABI, developed
//! through forensic differential parity against a pinned official Chromium
//! Mojo oracle.
//!
//! Status discipline: capability claims live in `atlas/feature-matrix.json`
//! and move up the sealed ladder only with court receipts.
#![deny(missing_docs)]

pub use mojo_rs_core as core;
pub use mojo_rs_system as system;

/// The pinned Chromium compatibility epoch.
pub const COMPATIBILITY_EPOCH: &str = "151.0.7922.105";

/// The pinned Chromium commit for the epoch.
pub const COMPATIBILITY_COMMIT: &str = "bfa3579138998e2fbb981725570fa588c5b6f8cd";
