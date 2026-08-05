//! # mojo-rs-system
//!
//! Idiomatic safe Rust System API above the native core. Ownership is
//! enforced by the type system; compatibility lives at the C ABI boundary.
//! See the module docs for the ownership model of each capability.
#![deny(missing_docs)]

pub mod data_pipe;
pub mod error;
pub mod message_pipe;
pub mod shared_buffer;
pub mod trap;

/// Convenience re-exports.
pub mod prelude {
    pub use crate::data_pipe::{
        DataPipeConsumer, DataPipeOptions, DataPipePair, DataPipeProducer, ReadFlags, ReadOutcome,
        ReadTransaction, WriteTransaction, close_consumer, close_producer, create,
    };
    pub use crate::error::{SystemError, SystemResult};
    pub use crate::message_pipe::MessagePipeEndpoint;
    pub use crate::shared_buffer::{Mapping, SharedBuffer};
}
