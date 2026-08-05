//! ipcz interop: the node-link wire protocol layer and the native side of the
//! Phase 3 interop seal (official C++ ⇄ native Rust).
//!
//! The official traffic is captured by `scripts/run_invite_court.sh` and kept
//! as golden fixtures in `testdata/ipcz/`; the parsers and encoders here must
//! reproduce the official framing exactly (byte-identical golden tests).

#![deny(missing_docs)]

pub mod acceptor;
pub mod channel;
pub mod link_memory;
pub mod messages;
pub mod router;
pub mod routing;
pub mod wire;
