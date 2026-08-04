//! Platform-handle dispatcher: wraps a native descriptor in a Mojo handle.

use std::sync::Arc;

use mojo_rs_platform::fd::OwnedFd;

use crate::dispatcher::{Dispatcher, DispatcherType, WatchId};
use crate::signal::SignalsState;
use crate::trap::WatchCallback;

/// A dispatcher wrapping a platform descriptor.
pub struct PlatformHandleDispatcher {
    fd: OwnedFd,
}

impl PlatformHandleDispatcher {
    /// Wrap an owned descriptor.
    pub fn new(fd: OwnedFd) -> Arc<PlatformHandleDispatcher> {
        Arc::new(PlatformHandleDispatcher { fd })
    }

    /// The wrapped descriptor (borrowed).
    pub fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.fd.as_raw_fd()
    }

    /// Release the descriptor (for `MojoUnwrapPlatformHandle`).
    pub fn take_fd(&self) -> Option<OwnedFd> {
        // The dispatcher holds the fd; unwrap duplicates it out.
        self.fd.try_dup().ok()
    }
}

impl Dispatcher for PlatformHandleDispatcher {
    fn dispatcher_type(&self) -> DispatcherType {
        DispatcherType::PlatformHandle
    }

    fn query_signals(&self) -> SignalsState {
        SignalsState::default()
    }

    fn on_closed(&self) {}

    fn start_watch(&self, _signals: crate::signal::Signals, _callback: WatchCallback) -> WatchId {
        WatchId::new(0)
    }

    fn cancel_watch(&self, _id: WatchId) {}

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
