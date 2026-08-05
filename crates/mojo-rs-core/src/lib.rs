//! # mojo-rs-core
//!
//! The native Mojo runtime core: handle table, dispatchers, message pipes,
//! signals, traps, waits, invitations, shared buffers, data pipes, resource
//! accounting, and shutdown. State machines are explicit; ownership is
//! explicit; concurrency is documented with lock ordering.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

pub mod data_pipe;
pub mod dispatcher;
pub mod error;
pub mod handle;
pub mod message;
pub mod pipe;
pub mod platform_handle;
pub mod ring_buffer;
pub mod shared_buffer;
pub mod signal;
pub mod trap;
pub mod wait;

/// Convenience re-exports of the core API.
pub mod prelude {
    pub use crate::data_pipe::{DataPipe, DataPipeEnd, ReadFlags};
    pub use crate::dispatcher::{Dispatcher, DispatcherType, WatchId};
    pub use crate::error::{CoreError, CoreResult};
    pub use crate::handle::{Handle, HandleTable, MojoHandleValue};
    pub use crate::message::{Message, MessageBody};
    pub use crate::pipe::{End, MessagePipe, MessagePipeDispatcher, ReadOutcome};
    pub use crate::shared_buffer::{BufferMapping, BufferMode, MappingTable, SharedBuffer};
    pub use crate::signal::{Signals, SignalsState};
    pub use crate::trap::{Trap, TrapCallback, TrapEvent, WatchCallback};
    pub use crate::wait::Waiter;
}
