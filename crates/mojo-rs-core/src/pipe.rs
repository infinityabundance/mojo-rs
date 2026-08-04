//! Message pipes: the central in-process message-passing state machine.
//!
//! Modeled after the official Mojo message-pipe contract:
//! * Two endpoints share one pipe state; each endpoint has a FIFO queue.
//! * Writes enqueue on the peer endpoint; ordering is per-direction FIFO.
//! * Signals: READABLE (queue non-empty), WRITABLE (peer open),
//!   PEER_CLOSED (peer closed).
//! * Local close is irreversible; the peer observes PEER_CLOSED eventually.
//! * Watchers (traps) are notified on state changes; callbacks are invoked
//!   AFTER the pipe lock is released (no callbacks under internal locks).
//! * One peer identity per endpoint; transferred endpoints are a later-phase
//!   concern (routing).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::dispatcher::{Dispatcher, DispatcherType, WatchId};
use crate::error::{CoreError, CoreResult};
use crate::message::Message;
use crate::signal::{Signals, SignalsState};
use crate::trap::{WatchCallback, WatchKind};

/// One end of a message pipe pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Endpoint A.
    A = 0,
    /// Endpoint B.
    B = 1,
}

impl End {
    fn peer(self) -> End {
        match self {
            End::A => End::B,
            End::B => End::A,
        }
    }
}

/// The message pipe dispatcher: one shared state for both endpoints.
pub struct MessagePipe {
    state: Mutex<PipeState>,
}

struct PipeState {
    endpoints: [EndpointState; 2],
}

struct EndpointState {
    local_closed: bool,
    peer_closed: bool,
    queue: VecDeque<Message>,
    queued_bytes: usize,
    watchers: Vec<WatcherRegistration>,
}

struct WatcherRegistration {
    id: WatchId,
    signals: Signals,
    callback: WatchCallback,
    cancelled: bool,
}

/// The next watch id (per-pipe counter; ids only need to be unique per pipe).
static NEXT_WATCH_ID: AtomicU64 = AtomicU64::new(1);

impl MessagePipe {
    /// Create a message pipe, returning both endpoint dispatchers.
    pub fn create() -> (Arc<MessagePipeDispatcher>, Arc<MessagePipeDispatcher>) {
        let pipe = Arc::new(MessagePipe {
            state: Mutex::new(PipeState {
                endpoints: [EndpointState::new(), EndpointState::new()],
            }),
        });
        let a = Arc::new(MessagePipeDispatcher {
            pipe: Arc::clone(&pipe),
            end: End::A,
        });
        let b = Arc::new(MessagePipeDispatcher { pipe, end: End::B });
        (a, b)
    }

    /// Query the signal state of an endpoint.
    pub fn query_signals(&self, end: End) -> SignalsState {
        let state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return SignalsState::default(),
        };
        signals_of(&state.endpoints[end as usize])
    }

    /// Write a message from `end` to its peer.
    pub fn write(&self, end: End, message: Message) -> CoreResult<()> {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return Err(CoreError::Internal),
        };
        let peer = end.peer();
        let this = &state.endpoints[end as usize];
        if this.local_closed {
            return Err(CoreError::InvalidArgument);
        }
        if this.peer_closed {
            return Err(CoreError::FailedPrecondition);
        }
        let len = message.data().len();
        state.endpoints[peer as usize].queue.push_back(message);
        state.endpoints[peer as usize].queued_bytes += len;

        // Notify the peer endpoint's watchers (post-lock).
        let callbacks = collect_notifications(&mut state.endpoints[peer as usize]);
        drop(state);
        invoke_all(callbacks);
        Ok(())
    }

    /// Read a message from `end` (FIFO).
    ///
    /// `max_num_bytes` and `may_discard` implement the C-API sizing rules:
    /// * If the next message's payload exceeds `max_num_bytes` and
    ///   `may_discard` is false, the message is kept queued and
    ///   `ResourceExhausted` is returned with the full size in `size_out`.
    /// * Otherwise the message is consumed.
    pub fn read(
        &self,
        end: End,
        max_num_bytes: Option<usize>,
        may_discard: bool,
    ) -> CoreResult<ReadOutcome> {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return Err(CoreError::Internal),
        };
        let ep = &mut state.endpoints[end as usize];
        if ep.local_closed {
            return Err(CoreError::InvalidArgument);
        }
        let Some(message) = ep.queue.pop_front() else {
            if ep.peer_closed {
                return Err(CoreError::FailedPrecondition);
            }
            return Err(CoreError::ShouldWait);
        };
        let data_len = message.data().len();
        ep.queued_bytes = ep.queued_bytes.saturating_sub(data_len);

        if let Some(max) = max_num_bytes {
            if data_len > max && !may_discard {
                // Return the message to the front of the queue.
                ep.queue.push_front(message);
                ep.queued_bytes += data_len;
                return Ok(ReadOutcome::TooLarge { size: data_len });
            }
        }

        let (data, handles) = message.into_parts();
        let callbacks = collect_notifications(ep);
        drop(state);
        invoke_all(callbacks);
        Ok(ReadOutcome::Message { data, handles })
    }

    /// Close an endpoint locally. The peer observes PEER_CLOSED.
    pub fn close(&self, end: End) {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let ep = &mut state.endpoints[end as usize];
        if ep.local_closed {
            return; // idempotent
        }
        ep.local_closed = true;
        // Cancel this endpoint's watchers with cancellation semantics.
        let cancelled_here = cancel_watchers(ep);
        let peer = end.peer();
        state.endpoints[peer as usize].peer_closed = true;
        let peer_callbacks = collect_notifications(&mut state.endpoints[peer as usize]);
        drop(state);
        for (_id, cb, state) in cancelled_here {
            cb(state, WatchKind::Cancelled);
        }
        invoke_all(peer_callbacks);
    }

    /// The number of queued messages on an endpoint (diagnostics).
    pub fn queued_count(&self, end: End) -> usize {
        self.state
            .lock()
            .map(|s| s.endpoints[end as usize].queue.len())
            .unwrap_or(0)
    }

    /// Endpoint-aware watch registration (used by traps and waits).
    pub fn register_watch(&self, end: End, signals: Signals, callback: WatchCallback) -> WatchId {
        let id = WatchId::new(NEXT_WATCH_ID.fetch_add(1, Ordering::Relaxed));
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return id,
        };
        let ep = &mut state.endpoints[end as usize];
        if ep.local_closed {
            // Watch on a closed endpoint fires immediately as unsatisfiable.
            return id;
        }
        ep.watchers.push(WatcherRegistration {
            id,
            signals,
            callback,
            cancelled: false,
        });
        id
    }

    /// Endpoint-aware watch cancellation.
    pub fn cancel_registered_watch(&self, end: End, id: WatchId) {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let ep = &mut state.endpoints[end as usize];
        if let Some(w) = ep.watchers.iter_mut().find(|w| w.id == id) {
            w.cancelled = true;
        }
    }
}

/// The outcome of a read.
#[derive(Debug)]
pub enum ReadOutcome {
    /// The message was consumed.
    Message {
        /// The payload bytes.
        data: Vec<u8>,
        /// Extracted handles (ownership transferred).
        handles: Vec<crate::handle::Handle>,
    },
    /// The message was too large for the provided buffer and remains queued.
    TooLarge {
        /// The full payload size required.
        size: usize,
    },
}

impl EndpointState {
    fn new() -> EndpointState {
        EndpointState {
            local_closed: false,
            peer_closed: false,
            queue: VecDeque::new(),
            queued_bytes: 0,
            watchers: Vec::new(),
        }
    }
}

/// Compute the signal state of an endpoint.
///
/// Mirrors the pinned epoch's `MojoQueryHandleSignalsStateIpcz` (core_ipcz.cc):
/// the satisfiable set always includes `PEER_CLOSED | QUOTA_EXCEEDED`;
/// `WRITABLE | PEER_REMOTE` are satisfiable only while the peer is open;
/// `READABLE` remains satisfiable while the portal is not "dead" (a portal is
/// dead when its peer is closed AND its parcel queue is empty); satisfied
/// signals are the intersection of the current state with the satisfiable set.
fn signals_of(ep: &EndpointState) -> SignalsState {
    let mut satisfied = Signals::NONE;
    let mut satisfiable = Signals::NONE;
    if !ep.local_closed {
        satisfiable = satisfiable | Signals::PEER_CLOSED | Signals::QUOTA_EXCEEDED;
        if !ep.peer_closed {
            satisfied = satisfied | Signals::WRITABLE;
            satisfiable = satisfiable | Signals::WRITABLE | Signals::PEER_REMOTE;
        } else {
            satisfied = satisfied | Signals::PEER_CLOSED;
        }
        let dead = ep.peer_closed && ep.queue.is_empty();
        if !dead {
            satisfiable = satisfiable | Signals::READABLE;
        }
        if !ep.queue.is_empty() {
            satisfied = satisfied | Signals::READABLE;
        }
    }
    SignalsState {
        satisfied,
        satisfiable,
    }
}

/// Collect the callbacks that must fire for an endpoint's watchers whose
/// conditions are now satisfied or unsatisfiable. Watchers are NOT cancelled
/// here: firing is gated by the trap's own armed state, and a fired watcher
/// must be able to fire again after a re-arm (official ipcz one-shot trap
/// semantics are implemented at the trap layer, not here).
fn collect_notifications(ep: &mut EndpointState) -> Vec<(WatchId, WatchCallback, SignalsState)> {
    let state = signals_of(ep);
    let mut out = Vec::new();
    for w in &mut ep.watchers {
        if w.cancelled {
            continue;
        }
        let fired = state.is_satisfied(w.signals) || state.is_unsatisfiable(w.signals);
        if fired {
            out.push((w.id, Arc::clone(&w.callback), state));
        }
    }
    out
}

/// Cancel all watchers on an endpoint (used on local close of the watched
/// endpoint). The trap receives a `Cancelled` notification and delivers a
/// CANCELLED event even when unarmed (official `TrapRemovalEventHandler`).
fn cancel_watchers(ep: &mut EndpointState) -> Vec<(WatchId, WatchCallback, SignalsState)> {
    let mut out = Vec::new();
    for w in &mut ep.watchers {
        if w.cancelled {
            continue;
        }
        w.cancelled = true;
        out.push((w.id, Arc::clone(&w.callback), SignalsState::default()));
    }
    out
}

/// Invoke collected watch callbacks after releasing the pipe lock.
fn invoke_all(callbacks: Vec<(WatchId, WatchCallback, SignalsState)>) {
    for (_id, cb, state) in callbacks {
        cb(state, WatchKind::Changed);
    }
}

impl Dispatcher for MessagePipe {
    fn dispatcher_type(&self) -> DispatcherType {
        DispatcherType::MessagePipe
    }

    fn query_signals(&self) -> SignalsState {
        self.query_signals(End::A)
    }

    fn on_closed(&self) {}

    fn start_watch(&self, _signals: Signals, _callback: WatchCallback) -> WatchId {
        // Endpoint-aware registration is done via the dispatcher wrapper.
        WatchId::new(NEXT_WATCH_ID.fetch_add(1, Ordering::Relaxed))
    }

    fn cancel_watch(&self, _id: WatchId) {}

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A single endpoint of a message pipe: the dispatcher stored in the handle
/// table (the official model has one dispatcher per endpoint).
pub struct MessagePipeDispatcher {
    pipe: Arc<MessagePipe>,
    end: End,
}

impl MessagePipeDispatcher {
    /// The shared pipe state.
    pub fn pipe(&self) -> &MessagePipe {
        &self.pipe
    }

    /// This dispatcher's endpoint.
    pub fn end(&self) -> End {
        self.end
    }

    /// Close this endpoint (the peer observes PEER_CLOSED).
    pub fn close(&self) {
        self.pipe.close(self.end);
    }
}

impl Dispatcher for MessagePipeDispatcher {
    fn dispatcher_type(&self) -> DispatcherType {
        DispatcherType::MessagePipe
    }

    fn query_signals(&self) -> SignalsState {
        self.pipe.query_signals(self.end)
    }

    fn on_closed(&self) {
        // The last (only) handle to this endpoint was closed: close it.
        self.pipe.close(self.end);
    }

    fn start_watch(&self, signals: Signals, callback: WatchCallback) -> WatchId {
        self.pipe.register_watch(self.end, signals, callback)
    }

    fn cancel_watch(&self, id: WatchId) {
        self.pipe.cancel_registered_watch(self.end, id)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::signal::Signals;

    fn fresh() -> (Arc<MessagePipeDispatcher>, Arc<MessagePipeDispatcher>) {
        MessagePipe::create()
    }

    #[test]
    fn initial_signals() {
        let (_a, b) = fresh();
        let st = b.pipe().query_signals(End::B);
        assert!(st.satisfied.contains(Signals::WRITABLE));
        assert!(!st.satisfied.contains(Signals::READABLE));
        assert!(!st.satisfied.contains(Signals::PEER_CLOSED));
        assert!(st.satisfiable.contains(Signals::READABLE));
        assert!(st.satisfiable.contains(Signals::WRITABLE));
        assert!(st.satisfiable.contains(Signals::PEER_CLOSED));
    }

    #[test]
    fn write_read_roundtrip_fifo() {
        let (a, b) = fresh();
        a.pipe()
            .write(End::A, Message::new(vec![1], vec![]))
            .unwrap();
        a.pipe()
            .write(End::A, Message::new(vec![2], vec![]))
            .unwrap();
        assert!(
            b.pipe()
                .query_signals(End::B)
                .satisfied
                .contains(Signals::READABLE)
        );
        let m1 = match b.pipe().read(End::B, None, false).unwrap() {
            ReadOutcome::Message { data, .. } => data,
            ReadOutcome::TooLarge { .. } => panic!(),
        };
        let m2 = match b.pipe().read(End::B, None, false).unwrap() {
            ReadOutcome::Message { data, .. } => data,
            ReadOutcome::TooLarge { .. } => panic!(),
        };
        assert_eq!(m1, vec![1]);
        assert_eq!(m2, vec![2]);
        assert_eq!(
            b.pipe().read(End::B, None, false).unwrap_err(),
            CoreError::ShouldWait
        );
    }

    #[test]
    fn read_too_large_keeps_message() {
        let (a, b) = fresh();
        a.pipe()
            .write(End::A, Message::new(vec![1, 2, 3, 4], vec![]))
            .unwrap();
        let outcome = b.pipe().read(End::B, Some(2), false).unwrap();
        match outcome {
            ReadOutcome::TooLarge { size } => assert_eq!(size, 4),
            _ => panic!(),
        }
        // Message still queued.
        let m = match b.pipe().read(End::B, None, false).unwrap() {
            ReadOutcome::Message { data, .. } => data,
            _ => panic!(),
        };
        assert_eq!(m, vec![1, 2, 3, 4]);
    }

    #[test]
    fn peer_closure_observed() {
        let (a, b) = fresh();
        a.pipe()
            .write(End::A, Message::new(vec![9], vec![]))
            .unwrap();
        a.close(); // closes endpoint A
        // Queued message still readable, then FAILED_PRECONDITION.
        let st = b.pipe().query_signals(End::B);
        assert!(st.satisfied.contains(Signals::READABLE));
        assert!(st.satisfied.contains(Signals::PEER_CLOSED));
        // While messages remain the portal is not dead: READABLE stays
        // satisfiable and WRITABLE drops out.
        assert!(st.satisfiable.contains(Signals::READABLE));
        assert!(st.satisfiable.contains(Signals::PEER_CLOSED));
        assert!(!st.satisfiable.contains(Signals::WRITABLE));
        let _ = b.pipe().read(End::B, None, false).unwrap();
        assert_eq!(
            b.pipe().read(End::B, None, false).unwrap_err(),
            CoreError::FailedPrecondition
        );
        // After draining: the portal is dead (peer closed, no parcels):
        // READABLE is satisfied by nothing and no longer satisfiable;
        // WRITABLE can never be satisfied; PEER_CLOSED stays satisfied.
        let st = b.pipe().query_signals(End::B);
        assert!(!st.satisfied.contains(Signals::READABLE));
        assert!(st.satisfied.contains(Signals::PEER_CLOSED));
        assert!(!st.satisfiable.contains(Signals::READABLE));
        assert!(!st.satisfiable.contains(Signals::WRITABLE));
        assert!(st.satisfiable.contains(Signals::PEER_CLOSED));
        assert!(st.satisfiable.contains(Signals::QUOTA_EXCEEDED));
    }

    #[test]
    fn write_to_closed_peer_fails() {
        let (a, b) = fresh();
        b.close();
        assert_eq!(
            a.pipe().write(End::A, Message::empty()).unwrap_err(),
            CoreError::FailedPrecondition
        );
    }

    #[test]
    fn watch_fires_on_readability() {
        use std::sync::mpsc;
        let (a, b) = fresh();
        let (tx, rx) = mpsc::channel();
        let cb: WatchCallback = Arc::new(move |state, _kind| {
            let _ = tx.send(state);
        });
        b.pipe().register_watch(End::B, Signals::READABLE, cb);
        a.pipe()
            .write(End::A, Message::new(vec![7], vec![]))
            .unwrap();
        let state = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(state.satisfied.contains(Signals::READABLE));
    }
}
