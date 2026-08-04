//! # mojo-rs-core
//!
//! The native Mojo runtime core: handle table, dispatchers, message pipes,
//! signals, traps, waits, invitations, shared buffers, data pipes, resource
//! accounting, and shutdown. State machines are explicit; ownership is
//! explicit; concurrency is documented with lock ordering.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

pub mod dispatcher;
pub mod handle;
pub mod message;
pub mod pipe;
pub mod signal;
pub mod trap;
pub mod wait;

pub mod prelude {
    pub use crate::dispatcher::Dispatcher;
    pub use crate::handle::{Handle, HandleTable, Token};
    pub use crate::message::{Message, MessageBody};
    pub use crate::pipe::MessagePipe;
    pub use crate::signal::Signals;
    pub use crate::trap::{Trap, TriggerContext};
    pub use crate::wait::Waiter;
}
