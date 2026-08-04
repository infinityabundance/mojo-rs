//! # mojo-rs-system
//!
//! Idiomatic safe Rust System API above the native core. Ownership is
//! enforced by the type system; compatibility lives at the C ABI boundary.
#![deny(missing_docs)]

pub mod data_pipe;
pub mod message_pipe;
pub mod shared_buffer;
pub mod trap;

/// Convenience re-exports.
pub mod prelude {
    pub use crate::message_pipe::MessagePipeEndpoint;
}
