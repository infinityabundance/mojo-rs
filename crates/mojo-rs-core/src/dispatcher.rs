//! Dispatcher abstraction: the trait every handle-backed object implements.
//!
//! The official Mojo Core has a `Dispatcher` hierarchy; here the trait
//! carries the externally observable surface (signals, closure, watch
//! registration). Dispatcher types are extensible (message pipes, data pipes,
//! shared buffers, traps, invitations, platform handles).

use std::any::Any;
use std::sync::Arc;

use crate::signal::{Signals, SignalsState};
use crate::trap::WatchCallback;

/// The kind of dispatcher (mirrors the official dispatcher types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DispatcherType {
    /// /// A message pipe endpoint.
    MessagePipe,
    /// /// A data pipe producer endpoint.
    DataPipeProducer,
    /// /// A data pipe consumer endpoint.
    DataPipeConsumer,
    /// /// A shared buffer.
    SharedBuffer,
    /// /// A trap.
    Trap,
    /// /// An invitation.
    Invitation,
    /// /// A wrapped platform handle.
    PlatformHandle,
    /// /// A watcher (deprecated in the official API).
    Watcher,
}

impl DispatcherType {
    /// The official `MojoHandleType` name for this dispatcher.
    pub fn name(self) -> &'static str {
        use DispatcherType::*;
        match self {
            MessagePipe => "MOJO_HANDLE_TYPE_MESSAGE_PIPE",
            DataPipeProducer => "MOJO_HANDLE_TYPE_DATA_PIPE_PRODUCER",
            DataPipeConsumer => "MOJO_HANDLE_TYPE_DATA_PIPE_CONSUMER",
            SharedBuffer => "MOJO_HANDLE_TYPE_SHARED_BUFFER",
            Trap => "MOJO_HANDLE_TYPE_TRAP",
            Invitation => "MOJO_HANDLE_TYPE_INVITATION",
            PlatformHandle => "MOJO_HANDLE_TYPE_PLATFORM_HANDLE",
            Watcher => "MOJO_HANDLE_TYPE_WATCHER",
        }
    }
}

/// A handle object (dispatcher) in the core.
pub trait Dispatcher: Send + Sync {
    /// The dispatcher kind.
    fn dispatcher_type(&self) -> DispatcherType;

    /// Whether the dispatcher can be duplicated via `MojoDuplicateBufferHandle`
    /// (only shared buffers and message pipes in the official API surface).
    fn is_duplicable(&self) -> bool {
        false
    }

    /// Query the current signal state.
    fn query_signals(&self) -> SignalsState;

    /// Called when the last handle to this dispatcher is closed. Must be
    /// idempotent and safe to call concurrently with other operations.
    fn on_closed(&self);

    /// Register a watch for `signals` on this dispatcher. The callback is
    /// invoked when the signal state changes such that the watch becomes
    /// satisfied (or unsatisfiable). Returns a watch id used for
    /// cancellation; the watch is cancelled automatically on closure.
    fn start_watch(&self, signals: Signals, callback: WatchCallback) -> WatchId;

    /// Cancel a previously registered watch. Cancellation is idempotent and
    /// must not produce a callback after it returns.
    fn cancel_watch(&self, id: WatchId);

    /// Type-erased access for safe downcasting to concrete dispatchers.
    fn as_any(&self) -> &dyn Any;
}

/// Identifies a registered watch on a dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatchId(u64);

impl WatchId {
    /// Create a watch id (assigned by the dispatcher).
    pub fn new(v: u64) -> WatchId {
        WatchId(v)
    }
}

/// Safe downcast helper: `&MessagePipe` from a dispatcher reference.
pub fn message_pipe_ref(d: &dyn Dispatcher) -> Option<&crate::pipe::MessagePipe> {
    d.as_any().downcast_ref::<crate::pipe::MessagePipe>()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn dispatcher_type_names() {
        assert_eq!(
            DispatcherType::MessagePipe.name(),
            "MOJO_HANDLE_TYPE_MESSAGE_PIPE"
        );
        assert_eq!(
            DispatcherType::SharedBuffer.name(),
            "MOJO_HANDLE_TYPE_SHARED_BUFFER"
        );
    }
}
