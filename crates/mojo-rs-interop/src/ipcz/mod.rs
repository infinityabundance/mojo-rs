//! ipcz interop: the node-link wire protocol layer for the native side of the
//! Phase 3 interop seal (official C++ ⇄ native Rust).
//!
//! The official traffic is captured by `scripts/run_invite_court.sh` and kept
//! as golden fixtures in `testdata/ipcz/`; the parsers here must reproduce the
//! official framing exactly.

#![deny(missing_docs)]

pub mod wire;
