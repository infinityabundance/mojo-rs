//! Data pipes: producer/consumer endpoints over a shared-memory ring buffer.
//!
//! Mirrors the pinned epoch's `mojo/core/ipcz_driver/data_pipe.{h,cc}`
//! (Chromium 151.0.7922.105, CoreIpcz architecture):
//!
//! * A pair shares one ring buffer (backed by a shared-memory mapping) plus a
//!   per-direction queue of control messages ("parcels"). A control message is
//!   a single `u32` byte count: the producer sends the count of bytes it wrote
//!   (the consumer extends its readable range on flush), and the consumer
//!   sends the count of bytes it consumed (the producer discards on flush).
//! * `SendPeerUpdate` skips zero-count messages, so the *presence* of a
//!   pending parcel implies new data (or new capacity), which is exactly how
//!   the official signal/trap machinery observes the pipe.
//! * Every public operation mirrors the official error-code and ordering
//!   semantics exactly (see the per-method comments for the official
//!   operation order, which is externally observable).
//! * The in-process core serializes the whole pair behind one lock (the
//!   official implementation guards each endpoint separately and routes
//!   control messages through an ipcz portal pair); the cross-process path
//!   (Phase 5) replaces the pair lock with shared-memory ownership plus
//!   portal control messages on the wire. The externally observable state
//!   machine is identical.
//!
//! Signal semantics mirror the official `DataPipe::GetSignals`:
//! * The satisfiable set always contains `PEER_CLOSED`; `PEER_REMOTE` is
//!   satisfiable while the peer is open.
//! * Consumer: `READABLE`/`NEW_DATA_READABLE` are satisfied when new data
//!   arrived (pending parcel or latched `has_new_data`) or data is buffered;
//!   both stay satisfiable while the peer is open.
//! * Producer: `WRITABLE` is satisfied when capacity is available or a
//!   pending "consumed" parcel exists, and is satisfiable while the peer is
//!   open.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mojo_rs_platform::shm::{Access, SharedMemory};

use crate::dispatcher::{Dispatcher, DispatcherType, WatchId};
use crate::error::{CoreError, CoreResult};
use crate::ring_buffer::{DirectReader, DirectWriter, RingBuffer, Span};
use crate::signal::{Signals, SignalsState};
use crate::trap::WatchCallback;

/// Which endpoint of a data pipe pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataPipeEnd {
    /// The producer endpoint (writes data).
    Producer,
    /// The consumer endpoint (reads data).
    Consumer,
}

impl DataPipeEnd {
    fn idx(self) -> usize {
        match self {
            DataPipeEnd::Producer => 0,
            DataPipeEnd::Consumer => 1,
        }
    }

    fn peer(self) -> DataPipeEnd {
        match self {
            DataPipeEnd::Producer => DataPipeEnd::Consumer,
            DataPipeEnd::Consumer => DataPipeEnd::Producer,
        }
    }
}

/// Flags for one-phase reads (official `MojoReadDataFlags` semantics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadFlags {
    /// Require that all requested bytes can be read (official
    /// `MOJO_READ_DATA_FLAG_ALL_OR_NONE`). Ignored when `query` is set.
    pub all_or_none: bool,
    /// Query the number of bytes available without reading
    /// (`MOJO_READ_DATA_FLAG_QUERY`). May not be combined with `discard` or
    /// `peek`.
    pub query: bool,
    /// Discard read data rather than copying (`MOJO_READ_DATA_FLAG_DISCARD`).
    /// May not be combined with `query` or `peek`.
    pub discard: bool,
    /// Read data without removing it (`MOJO_READ_DATA_FLAG_PEEK`). May not be
    /// combined with `query` or `discard`.
    pub peek: bool,
}

/// The outcome of a one-phase read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutcome {
    /// The output `num_bytes`: the number of bytes read/peeked/discarded, or
    /// the available size for a query.
    pub num_bytes: u32,
    /// The bytes read (or peeked). Empty for query and discard.
    pub data: Vec<u8>,
}

/// The state shared by both endpoints of a data pipe pair.
struct DataPipeShared {
    inner: Mutex<PairState>,
}

/// One endpoint's state.
struct EndpointState {
    local_closed: bool,
    peer_closed: bool,
    /// Pending control messages from the peer (the official control portal's
    /// unread parcels). Each value is a byte count.
    control: VecDeque<u32>,
    /// An active two-phase writer (producer) or reader (consumer).
    two_phase: Option<TwoPhase>,
    /// Latched "new data arrived since the last read attempt" (consumer only;
    /// official `has_new_data_`).
    has_new_data: bool,
    watchers: Vec<WatcherRegistration>,
}

/// An active two-phase operation.
enum TwoPhase {
    /// A captured available-capacity span on the producer.
    Writer(DirectWriter),
    /// A captured readable-data span on the consumer.
    Reader(DirectReader),
}

struct WatcherRegistration {
    id: WatchId,
    signals: Signals,
    callback: WatchCallback,
    cancelled: bool,
}

/// The whole pair state (one lock; see the module docs).
///
/// Each endpoint owns its own `RingBuffer` view over its own mapping of the
/// shared region — exactly the official model (the pair creates the region,
/// maps it for the consumer, duplicates the region, and maps the duplicate for
/// the producer). The two ranges are kept in sync by the control messages:
/// the producer's writes extend its own range and notify the consumer to
/// extend ITS range on flush; the consumer's reads discard from its own range
/// and notify the producer to discard on flush.
struct PairState {
    /// The producer's ring view.
    producer: RingBuffer,
    /// The consumer's ring view.
    consumer: RingBuffer,
    /// Endpoint 0 = producer, 1 = consumer.
    endpoints: [EndpointState; 2],
}

/// The next watch id (process-global counter; ids only need uniqueness per
/// dispatcher, matching the message-pipe core).
static NEXT_WATCH_ID: AtomicU64 = AtomicU64::new(1);

/// A single endpoint of a data pipe pair: the dispatcher stored in the handle
/// table (the official model has one DataPipe object per endpoint).
pub struct DataPipe {
    shared: Arc<DataPipeShared>,
    end: DataPipeEnd,
    element_size: u32,
}

impl DataPipe {
    /// Create a data pipe pair with `element_size`-byte elements and
    /// `capacity` total bytes, mirroring `MojoCreateDataPipeIpcz` (returns
    /// `(producer, consumer)`).
    ///
    /// Returns `InvalidArgument` when `capacity == 0`, `element_size == 0`, or
    /// `capacity < element_size` (the official C entry's validation, which
    /// runs before any allocation). Returns `ResourceExhausted` when the
    /// shared-memory backing cannot be created or mapped.
    pub fn create_pair(
        element_size: u32,
        capacity: u32,
    ) -> CoreResult<(Arc<DataPipe>, Arc<DataPipe>)> {
        if capacity == 0 || element_size == 0 || capacity < element_size {
            return Err(CoreError::InvalidArgument);
        }
        let mem = SharedMemory::create("mojo-rs-data-pipe", capacity as usize)
            .map_err(|_| CoreError::ResourceExhausted)?;
        // Official `CreatePair`: the consumer region is created and mapped; the
        // producer gets its own duplicate mapping of the same pages.
        let consumer_mapping = mem
            .map(0, capacity as usize, Access::ReadWrite)
            .map_err(|_| CoreError::ResourceExhausted)?;
        let producer_mem = mem.duplicate().map_err(|_| CoreError::ResourceExhausted)?;
        let producer_mapping = producer_mem
            .map(0, capacity as usize, Access::ReadWrite)
            .map_err(|_| CoreError::ResourceExhausted)?;
        let shared = Arc::new(DataPipeShared {
            inner: Mutex::new(PairState {
                producer: RingBuffer::new(producer_mapping),
                consumer: RingBuffer::new(consumer_mapping),
                endpoints: [EndpointState::new(), EndpointState::new()],
            }),
        });
        let producer = Arc::new(DataPipe {
            shared: Arc::clone(&shared),
            end: DataPipeEnd::Producer,
            element_size,
        });
        let consumer = Arc::new(DataPipe {
            shared,
            end: DataPipeEnd::Consumer,
            element_size,
        });
        Ok((producer, consumer))
    }

    /// This endpoint's role.
    pub fn end(&self) -> DataPipeEnd {
        self.end
    }

    /// The element size of this pipe.
    pub fn element_size(&self) -> u32 {
        self.element_size
    }

    /// One-phase write (official `DataPipe::WriteData`).
    ///
    /// `elements` must contain `num_bytes` bytes. Returns the number of bytes
    /// written. Error ordering (externally observable):
    /// 1. `num_bytes % element_size != 0` → `InvalidArgument` (no flush).
    /// 2. active two-phase writer → `Busy`.
    /// 3. peer closed → `FailedPrecondition`.
    /// 4. ALL_OR_NONE with insufficient capacity → `ShouldWait` (empty input)
    ///    or `OutOfRange`.
    /// 5. partial write of 0 bytes with non-empty input → `ShouldWait`.
    pub fn write(&self, elements: &[u8], num_bytes: u32, all_or_none: bool) -> CoreResult<u32> {
        let element_size = self.element_size as usize;
        if num_bytes as usize % element_size != 0 {
            return Err(CoreError::InvalidArgument);
        }
        let mut state = self.lock()?;
        self.flush_locked(&mut state);
        let end = self.end.idx();
        let peer = self.end.peer().idx();
        if state.endpoints[end].two_phase.is_some() {
            return Err(CoreError::Busy);
        }
        if state.endpoints[end].peer_closed {
            return Err(CoreError::FailedPrecondition);
        }
        let write_size: usize;
        if all_or_none {
            if !state.producer.write_all(&elements[..num_bytes as usize]) {
                return Err(if num_bytes == 0 {
                    CoreError::ShouldWait
                } else {
                    CoreError::OutOfRange
                });
            }
            write_size = num_bytes as usize;
        } else {
            write_size = state.producer.write(&elements[..num_bytes as usize]);
            if write_size == 0 && !elements.is_empty() {
                return Err(CoreError::ShouldWait);
            }
        }
        let written = write_size as u32;
        // Official `SendPeerUpdate` skips zero-count messages (the presence of
        // a parcel is the signal).
        let callbacks = if written > 0 {
            state.endpoints[peer].control.push_back(written);
            collect_watchers(&mut state, peer)
        } else {
            Vec::new()
        };
        drop(state);
        invoke_all(callbacks);
        Ok(written)
    }

    /// Begin a two-phase write (official `DataPipe::BeginWriteData`). Returns
    /// the writable span (address + length). The caller writes into the span
    /// and must call `end_write`.
    ///
    /// Error ordering: active two-phase writer → `Busy`; peer closed →
    /// `FailedPrecondition`; no available capacity → `ShouldWait`. Note that
    /// the official epoch ignores the flags/hint entirely.
    pub fn begin_write(&self) -> CoreResult<Span> {
        let mut state = self.lock()?;
        self.flush_locked(&mut state);
        let end = self.end.idx();
        if state.endpoints[end].two_phase.is_some() {
            return Err(CoreError::Busy);
        }
        if state.endpoints[end].peer_closed {
            return Err(CoreError::FailedPrecondition);
        }
        let writer = state.producer.begin_write();
        if writer.span().is_empty() {
            return Err(CoreError::ShouldWait);
        }
        let span = writer.span();
        state.endpoints[end].two_phase = Some(TwoPhase::Writer(writer));
        Ok(span)
    }

    /// End a two-phase write (official `DataPipe::EndWriteData`). The two-phase
    /// operation is ended on failure too. `produced` must be a multiple of the
    /// element size and no larger than the span returned by `begin_write`.
    pub fn end_write(&self, produced: u32) -> CoreResult<()> {
        let mut state = self.lock()?;
        let end = self.end.idx();
        let peer = self.end.peer().idx();
        let callbacks = {
            let Some(two) = state.endpoints[end].two_phase.take() else {
                return Err(CoreError::FailedPrecondition);
            };
            if produced as usize % self.element_size as usize != 0 {
                return Err(CoreError::InvalidArgument);
            }
            if produced == 0 {
                return Ok(());
            }
            let TwoPhase::Writer(writer) = two else {
                return Err(CoreError::InvalidArgument);
            };
            if !writer.commit(produced as usize, &mut state.producer) {
                return Err(CoreError::InvalidArgument);
            }
            state.endpoints[peer].control.push_back(produced);
            collect_watchers(&mut state, peer)
        };
        drop(state);
        invoke_all(callbacks);
        Ok(())
    }

    /// One-phase read (official `DataPipe::ReadData`).
    ///
    /// `elements` must be `Some` (with `len == num_bytes`) unless `query` or
    /// `discard` is set. Error ordering (externally observable):
    /// 1. flag combination `(peek && discard)` or `(query && (peek ||
    ///    discard))` → `InvalidArgument` (no flush).
    /// 2. non-null-required read with null elements and `num_bytes > 0` →
    ///    `InvalidArgument` (no flush).
    /// 3. active two-phase reader → `Busy` (after flush).
    /// 4. `num_bytes % element_size != 0` → `InvalidArgument` (after flush;
    ///    skipped for query).
    /// 5. ALL_OR_NONE insufficient data → `FailedPrecondition` (peer closed)
    ///    or `OutOfRange`.
    /// 6. partial read with no data → `FailedPrecondition` (peer closed) or
    ///    `ShouldWait`.
    pub fn read(
        &self,
        mut elements: Option<&mut [u8]>,
        num_bytes: u32,
        flags: ReadFlags,
    ) -> CoreResult<ReadOutcome> {
        if (flags.peek && flags.discard) || (flags.query && (flags.peek || flags.discard)) {
            return Err(CoreError::InvalidArgument);
        }
        let allow_partial = !flags.all_or_none;
        if !(flags.discard || flags.query) {
            if elements.is_none() && num_bytes > 0 {
                return Err(CoreError::InvalidArgument);
            }
        }
        let mut state = self.lock()?;
        self.flush_locked(&mut state);
        let end = self.end.idx();
        let peer = self.end.peer().idx();
        if state.endpoints[end].two_phase.is_some() {
            return Err(CoreError::Busy);
        }
        let data_size = state.consumer.data_size();
        if flags.query {
            return Ok(ReadOutcome {
                num_bytes: data_size as u32,
                data: Vec::new(),
            });
        }
        if num_bytes as usize % self.element_size as usize != 0 {
            return Err(CoreError::InvalidArgument);
        }
        // Official: the latch is cleared before the read branches, including
        // when the read subsequently fails.
        state.endpoints[end].has_new_data = false;
        let peer_closed = state.endpoints[end].peer_closed;
        if !allow_partial {
            let success = if flags.discard {
                state.consumer.discard_all(num_bytes as usize)
            } else if flags.peek {
                let out = elements.as_deref_mut().unwrap_or(&mut []);
                state.consumer.peek_all(&mut out[..num_bytes as usize])
            } else {
                let out = elements.as_deref_mut().unwrap_or(&mut []);
                state.consumer.read_all(&mut out[..num_bytes as usize])
            };
            if !success {
                return Err(if peer_closed {
                    CoreError::FailedPrecondition
                } else {
                    CoreError::OutOfRange
                });
            }
            let read_size = num_bytes;
            let data = if flags.discard {
                Vec::new()
            } else {
                let out = elements.as_deref_mut().unwrap_or(&mut []);
                out[..read_size as usize].to_vec()
            };
            if flags.peek || read_size == 0 {
                return Ok(ReadOutcome {
                    num_bytes: read_size,
                    data,
                });
            }
            let callbacks = push_and_collect(&mut state, peer, read_size);
            drop(state);
            invoke_all(callbacks);
            return Ok(ReadOutcome {
                num_bytes: read_size,
                data,
            });
        }
        // Partial (default) path.
        if data_size == 0 {
            return Err(if peer_closed {
                CoreError::FailedPrecondition
            } else {
                CoreError::ShouldWait
            });
        }
        let read_size: u32;
        let data: Vec<u8>;
        if flags.discard {
            read_size = std::cmp::min(num_bytes, data_size as u32);
            state.consumer.discard(read_size as usize);
            data = Vec::new();
        } else if flags.peek {
            let out = elements.as_deref_mut().unwrap_or(&mut []);
            read_size = state.consumer.peek(&mut out[..num_bytes as usize]) as u32;
            data = out[..read_size as usize].to_vec();
        } else {
            let out = elements.as_deref_mut().unwrap_or(&mut []);
            read_size = state.consumer.read(&mut out[..num_bytes as usize]) as u32;
            data = out[..read_size as usize].to_vec();
        }
        if flags.peek || read_size == 0 {
            return Ok(ReadOutcome {
                num_bytes: read_size,
                data,
            });
        }
        let callbacks = push_and_collect(&mut state, peer, read_size);
        drop(state);
        invoke_all(callbacks);
        Ok(ReadOutcome {
            num_bytes: read_size,
            data,
        })
    }

    /// Begin a two-phase read (official `DataPipe::BeginReadData`). Returns
    /// the readable span. Error ordering: active two-phase reader → `Busy`;
    /// no data → `FailedPrecondition` (peer closed) or `ShouldWait`.
    pub fn begin_read(&self) -> CoreResult<Span> {
        let mut state = self.lock()?;
        self.flush_locked(&mut state);
        let end = self.end.idx();
        if state.endpoints[end].two_phase.is_some() {
            return Err(CoreError::Busy);
        }
        let reader = state.consumer.begin_read();
        if reader.span().is_empty() {
            return Err(if state.endpoints[end].peer_closed {
                CoreError::FailedPrecondition
            } else {
                CoreError::ShouldWait
            });
        }
        let span = reader.span();
        state.endpoints[end].two_phase = Some(TwoPhase::Reader(reader));
        state.endpoints[end].has_new_data = false;
        Ok(span)
    }

    /// End a two-phase read (official `DataPipe::EndReadData`). The two-phase
    /// operation is ended on failure too.
    pub fn end_read(&self, consumed: u32) -> CoreResult<()> {
        let mut state = self.lock()?;
        let end = self.end.idx();
        let peer = self.end.peer().idx();
        let callbacks = {
            let Some(two) = state.endpoints[end].two_phase.take() else {
                return Err(CoreError::FailedPrecondition);
            };
            if consumed as usize % self.element_size as usize != 0 {
                return Err(CoreError::InvalidArgument);
            }
            if consumed == 0 {
                return Ok(());
            }
            let TwoPhase::Reader(reader) = two else {
                return Err(CoreError::InvalidArgument);
            };
            if !reader.consume(consumed as usize, &mut state.consumer) {
                return Err(CoreError::InvalidArgument);
            }
            state.endpoints[peer].control.push_back(consumed);
            collect_watchers(&mut state, peer)
        };
        drop(state);
        invoke_all(callbacks);
        Ok(())
    }

    /// Query the current signal state (mirrors the C entry: flush peer updates
    /// first, then `GetSignals`).
    pub fn query_signals(&self) -> SignalsState {
        let mut state = match self.lock() {
            Ok(s) => s,
            Err(_) => return SignalsState::default(),
        };
        self.flush_locked(&mut state);
        signals_of(&mut state, self.end.idx())
    }

    /// Close this endpoint locally. The peer observes `PEER_CLOSED`; this
    /// endpoint's watchers are cancelled with `Cancelled`.
    pub fn close(&self) {
        let mut state = match self.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let end = self.end.idx();
        let peer = self.end.peer().idx();
        if state.endpoints[end].local_closed {
            return; // idempotent
        }
        state.endpoints[end].local_closed = true;
        let cancelled = cancel_watchers(&mut state.endpoints[end]);
        state.endpoints[peer].peer_closed = true;
        let peer_callbacks = collect_watchers(&mut state, peer);
        drop(state);
        for (_id, cb, st) in cancelled {
            cb(st, crate::trap::WatchKind::Cancelled);
        }
        invoke_all(peer_callbacks);
    }

    /// Register a watch for `signals` on this endpoint.
    pub fn register_watch(&self, signals: Signals, callback: WatchCallback) -> WatchId {
        let id = WatchId::new(NEXT_WATCH_ID.fetch_add(1, Ordering::Relaxed));
        let mut state = match self.lock() {
            Ok(s) => s,
            Err(_) => return id,
        };
        let end = self.end.idx();
        if state.endpoints[end].local_closed {
            return id;
        }
        state.endpoints[end].watchers.push(WatcherRegistration {
            id,
            signals,
            callback,
            cancelled: false,
        });
        id
    }

    /// Cancel a previously registered watch.
    pub fn cancel_registered_watch(&self, id: WatchId) {
        let mut state = match self.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let end = self.end.idx();
        if let Some(w) = state.endpoints[end]
            .watchers
            .iter_mut()
            .find(|w| w.id == id)
        {
            w.cancelled = true;
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, PairState>, CoreError> {
        self.shared.inner.lock().map_err(|_| CoreError::Internal)
    }

    /// Drain pending control messages from the peer and apply them (official
    /// `FlushUpdatesFromPeer` + `DrainPeerUpdates`): the producer discards
    /// consumed bytes; the consumer extends its readable range and latches
    /// `has_new_data`. Accumulation stops on overflow (the overflowing message
    /// is still consumed), exactly like the official drain.
    fn flush_locked(&self, state: &mut PairState) {
        let end = self.end.idx();
        let mut changed: usize = 0;
        loop {
            let Some(value) = state.endpoints[end].control.front().copied() else {
                break;
            };
            state.endpoints[end].control.pop_front();
            match changed.checked_add(value as usize) {
                Some(s) => changed = s,
                None => break, // overflow: stop accumulating (official)
            }
        }
        if changed == 0 {
            return;
        }
        match self.end {
            DataPipeEnd::Producer => {
                state.producer.discard_all(changed);
            }
            DataPipeEnd::Consumer => {
                let ok = state.consumer.extend_data_range(changed);
                debug_assert!(ok);
                state.endpoints[DataPipeEnd::Consumer.idx()].has_new_data = true;
            }
        }
    }
}

impl EndpointState {
    fn new() -> EndpointState {
        EndpointState {
            local_closed: false,
            peer_closed: false,
            control: VecDeque::new(),
            two_phase: None,
            has_new_data: false,
            watchers: Vec::new(),
        }
    }
}

/// Push a peer update (nonzero byte count) and collect the peer's watch
/// callbacks, mirroring the official `SendPeerUpdate` (zero-count messages are
/// skipped: the presence of a parcel is the signal).
fn push_and_collect(
    state: &mut PairState,
    peer: usize,
    count: u32,
) -> Vec<(WatchId, WatchCallback, SignalsState)> {
    if count == 0 {
        return Vec::new();
    }
    state.endpoints[peer].control.push_back(count);
    collect_watchers(state, peer)
}

/// Compute the signal state of an endpoint (official `DataPipe::GetSignals`),
/// including pending control parcels as "local portal parcels". May latch
/// `has_new_data` for the consumer.
fn signals_of(state: &mut PairState, end: usize) -> SignalsState {
    let mut satisfied = Signals::NONE;
    let mut satisfiable = Signals::NONE;
    let peer_closed = state.endpoints[end].peer_closed;
    let parcels = state.endpoints[end].control.len();
    // The control portal always exists in-process; the official returns false
    // (INVALID_ARGUMENT at the C entry) only when it is gone.
    satisfiable = satisfiable | Signals::PEER_CLOSED;
    if peer_closed {
        satisfied = satisfied | Signals::PEER_CLOSED;
    } else {
        satisfiable = satisfiable | Signals::PEER_REMOTE;
    }
    if end == DataPipeEnd::Consumer.idx() {
        let data_size = state.consumer.data_size();
        let new_data_available = state.endpoints[end].has_new_data || parcels > 0;
        if new_data_available {
            state.endpoints[end].has_new_data = true;
            satisfied = satisfied | Signals::NEW_DATA_READABLE;
            satisfiable = satisfiable | Signals::NEW_DATA_READABLE;
        }
        let any_data_available = new_data_available || data_size > 0;
        if any_data_available {
            satisfiable = satisfiable | Signals::READABLE;
            satisfied = satisfied | Signals::READABLE;
        }
        if !peer_closed {
            satisfiable = satisfiable | Signals::READABLE | Signals::NEW_DATA_READABLE;
        }
    } else {
        // Producer.
        if !peer_closed {
            satisfiable = satisfiable | Signals::WRITABLE;
            if state.producer.available_capacity() > 0 || parcels > 0 {
                satisfied = satisfied | Signals::WRITABLE;
            }
        }
    }
    SignalsState {
        satisfied,
        satisfiable,
    }
}

/// Collect the callbacks that must fire for an endpoint's watchers whose
/// conditions are now satisfied or unsatisfiable (same gate as the message
/// pipe; mirrors the official ipcz trap conditions on the control portal).
fn collect_watchers(
    state: &mut PairState,
    end: usize,
) -> Vec<(WatchId, WatchCallback, SignalsState)> {
    let sig = signals_of(state, end);
    let mut out = Vec::new();
    for w in &mut state.endpoints[end].watchers {
        if w.cancelled {
            continue;
        }
        let fired = sig.is_satisfied(w.signals) || sig.is_unsatisfiable(w.signals);
        if fired {
            out.push((w.id, Arc::clone(&w.callback), sig));
        }
    }
    out
}

/// Cancel all watchers on an endpoint (local close): the trap receives a
/// `Cancelled` notification.
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

/// Invoke collected watch callbacks after releasing the pair lock.
fn invoke_all(callbacks: Vec<(WatchId, WatchCallback, SignalsState)>) {
    for (_id, cb, state) in callbacks {
        cb(state, crate::trap::WatchKind::Changed);
    }
}

impl Dispatcher for DataPipe {
    fn dispatcher_type(&self) -> DispatcherType {
        match self.end {
            DataPipeEnd::Producer => DispatcherType::DataPipeProducer,
            DataPipeEnd::Consumer => DispatcherType::DataPipeConsumer,
        }
    }

    fn is_duplicable(&self) -> bool {
        false
    }

    fn query_signals(&self) -> SignalsState {
        DataPipe::query_signals(self)
    }

    fn on_closed(&self) {
        // The last (only) handle to this endpoint was closed: close it.
        self.close();
    }

    fn start_watch(&self, signals: Signals, callback: WatchCallback) -> WatchId {
        self.register_watch(signals, callback)
    }

    fn cancel_watch(&self, id: WatchId) {
        self.cancel_registered_watch(id);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn pair() -> (Arc<DataPipe>, Arc<DataPipe>) {
        DataPipe::create_pair(1, 64).unwrap()
    }

    #[test]
    fn create_rejects_bad_options_like_the_c_entry() {
        // capacity 0
        assert!(DataPipe::create_pair(1, 0).is_err());
        // element size 0
        assert!(DataPipe::create_pair(0, 64).is_err());
        // capacity < element size
        assert!(DataPipe::create_pair(8, 4).is_err());
        // A valid pair works.
        assert!(DataPipe::create_pair(4, 64).is_ok());
    }

    #[test]
    fn initial_signals() {
        let (p, c) = pair();
        let ps = p.query_signals();
        assert!(ps.satisfied.contains(Signals::WRITABLE));
        assert!(!ps.satisfiable.contains(Signals::READABLE));
        let cs = c.query_signals();
        assert!(!cs.satisfied.contains(Signals::READABLE));
        assert!(cs.satisfiable.contains(Signals::READABLE));
        assert!(cs.satisfiable.contains(Signals::NEW_DATA_READABLE));
        assert!(!cs.satisfied.contains(Signals::NEW_DATA_READABLE));
        assert!(cs.satisfiable.contains(Signals::PEER_CLOSED));
    }

    #[test]
    fn one_phase_write_read() {
        let (p, c) = pair();
        assert_eq!(p.write(&[1, 2, 3], 3, false).unwrap(), 3);
        let cs = c.query_signals();
        assert!(cs.satisfied.contains(Signals::READABLE));
        assert!(cs.satisfied.contains(Signals::NEW_DATA_READABLE));
        let mut buf = [0u8; 3];
        let out = c.read(Some(&mut buf), 3, ReadFlags::default()).unwrap();
        assert_eq!(out.num_bytes, 3);
        assert_eq!(out.data, vec![1, 2, 3]);
        // After the read, NEW_DATA_READABLE clears (no pending parcels).
        let cs = c.query_signals();
        assert!(!cs.satisfied.contains(Signals::NEW_DATA_READABLE));
        assert!(!cs.satisfied.contains(Signals::READABLE));
    }

    #[test]
    fn partial_write_and_should_wait() {
        let (p, c) = pair();
        // Write to fill the 64-byte ring.
        let full = vec![0xabu8; 64];
        assert_eq!(p.write(&full, 64, false).unwrap(), 64);
        assert_eq!(p.write(&[1], 1, false).unwrap_err(), CoreError::ShouldWait);
        // ALL_OR_NONE over capacity.
        assert_eq!(
            p.write(&[1, 2], 2, true).unwrap_err(),
            CoreError::OutOfRange
        );
        // Consume 4, then a partial write of 6 writes 4.
        let mut buf = [0u8; 4];
        let out = c.read(Some(&mut buf), 4, ReadFlags::default()).unwrap();
        assert_eq!(out.num_bytes, 4);
        assert_eq!(p.write(&[9, 9, 9, 9, 9, 9], 6, false).unwrap(), 4);
        assert_eq!(p.write(&[], 0, false).unwrap(), 0); // empty write OK
    }

    #[test]
    fn all_or_none_write_semantics() {
        let (p, c) = pair();
        let full = vec![0x11u8; 64];
        p.write(&full, 64, false).unwrap();
        assert_eq!(
            p.write(&[1, 2], 2, true).unwrap_err(),
            CoreError::OutOfRange
        );
        assert_eq!(p.write(&[], 0, true).unwrap(), 0); // empty ALL_OR_NONE OK
    }

    #[test]
    fn read_query_discard_peek() {
        let (p, c) = pair();
        p.write(&[1, 2, 3, 4, 5], 5, false).unwrap();
        // Query.
        let q = c
            .read(
                None,
                0,
                ReadFlags {
                    query: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(q.num_bytes, 5);
        assert!(q.data.is_empty());
        // Peek.
        let mut buf = [0u8; 2];
        let pk = c
            .read(
                Some(&mut buf),
                2,
                ReadFlags {
                    peek: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(pk.num_bytes, 2);
        assert_eq!(pk.data, vec![1, 2]);
        // Data still there.
        assert_eq!(
            c.read(
                None,
                0,
                ReadFlags {
                    query: true,
                    ..Default::default()
                }
            )
            .unwrap()
            .num_bytes,
            5
        );
        // Discard 2.
        let d = c
            .read(
                None,
                2,
                ReadFlags {
                    discard: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(d.num_bytes, 2);
        assert_eq!(
            c.read(
                None,
                0,
                ReadFlags {
                    query: true,
                    ..Default::default()
                }
            )
            .unwrap()
            .num_bytes,
            3
        );
        // Read the rest.
        let mut buf = [0u8; 8];
        let out = c.read(Some(&mut buf), 8, ReadFlags::default()).unwrap();
        assert_eq!(out.num_bytes, 3);
        assert_eq!(out.data, vec![3, 4, 5]);
    }

    #[test]
    fn invalid_flag_combinations() {
        let (_, c) = pair();
        assert_eq!(
            c.read(
                None,
                0,
                ReadFlags {
                    peek: true,
                    discard: true,
                    ..Default::default()
                }
            )
            .unwrap_err(),
            CoreError::InvalidArgument
        );
        assert_eq!(
            c.read(
                None,
                0,
                ReadFlags {
                    query: true,
                    peek: true,
                    ..Default::default()
                }
            )
            .unwrap_err(),
            CoreError::InvalidArgument
        );
        assert_eq!(
            c.read(
                None,
                0,
                ReadFlags {
                    query: true,
                    discard: true,
                    ..Default::default()
                }
            )
            .unwrap_err(),
            CoreError::InvalidArgument
        );
    }

    #[test]
    fn element_size_alignment() {
        let (p, c) = DataPipe::create_pair(4, 64).unwrap();
        assert_eq!(
            p.write(&[1, 2, 3], 3, false).unwrap_err(),
            CoreError::InvalidArgument
        );
        assert_eq!(p.write(&[1, 2, 3, 4], 4, false).unwrap(), 4);
        // Misaligned read: INVALID_ARGUMENT. The official flush still runs
        // before the element-size check, so the peer update is applied and the
        // data is available to the next (aligned) read.
        let mut buf2 = [0u8; 3];
        assert_eq!(
            c.read(Some(&mut buf2), 3, ReadFlags::default())
                .unwrap_err(),
            CoreError::InvalidArgument
        );
        let mut buf3 = [0u8; 4];
        let out3 = c.read(Some(&mut buf3), 4, ReadFlags::default()).unwrap();
        assert_eq!(out3.num_bytes, 4);
        assert_eq!(out3.data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn two_phase_flow() {
        let (p, c) = pair();
        let span = p.begin_write().unwrap();
        assert_eq!(span.len(), 64);
        let mut s = span;
        // SAFETY: the span is the captured available-capacity view, live for
        // the two-phase window; exclusive access is held here.
        let w = unsafe { s.as_mut_slice() };
        w[..3].copy_from_slice(&[1, 2, 3]);
        p.end_write(3).unwrap();
        assert_eq!(p.begin_write().unwrap().len(), 61);

        let span = c.begin_read().unwrap();
        assert_eq!(span.len(), 3);
        assert_eq!(span.as_slice(), &[1, 2, 3]);
        c.end_read(3).unwrap();
        assert_eq!(c.begin_read().unwrap_err(), CoreError::ShouldWait);
    }

    #[test]
    fn two_phase_busy_and_bad_commit() {
        let (p, c) = pair();
        let _span = p.begin_write().unwrap();
        assert_eq!(p.write(&[1], 1, false).unwrap_err(), CoreError::Busy);
        assert_eq!(p.begin_write().unwrap_err(), CoreError::Busy);
        // Commit more than the span: INVALID_ARGUMENT; the two-phase is ended.
        assert_eq!(p.end_write(65).unwrap_err(), CoreError::InvalidArgument);
        // Now the pipe is writable again.
        assert_eq!(p.write(&[1], 1, false).unwrap(), 1);
        // End without begin.
        assert_eq!(c.end_read(1).unwrap_err(), CoreError::FailedPrecondition);
        assert_eq!(p.end_write(1).unwrap_err(), CoreError::FailedPrecondition);
    }

    #[test]
    fn peer_closure_semantics() {
        let (p, c) = pair();
        p.write(&[7, 8], 2, false).unwrap();
        p.close();
        // Consumer: data still readable; PEER_CLOSED satisfied.
        let cs = c.query_signals();
        assert!(cs.satisfied.contains(Signals::PEER_CLOSED));
        assert!(cs.satisfied.contains(Signals::READABLE));
        let mut buf = [0u8; 8];
        let out = c.read(Some(&mut buf), 8, ReadFlags::default()).unwrap();
        assert_eq!(out.num_bytes, 2);
        // Now empty + peer closed.
        assert_eq!(
            c.read(Some(&mut buf), 8, ReadFlags::default()).unwrap_err(),
            CoreError::FailedPrecondition
        );
        let cs = c.query_signals();
        assert!(cs.satisfied.contains(Signals::PEER_CLOSED));
        assert!(!cs.satisfiable.contains(Signals::READABLE));
        assert!(!cs.satisfiable.contains(Signals::WRITABLE));
    }

    #[test]
    fn write_to_closed_peer_fails() {
        let (p, c) = pair();
        c.close();
        assert_eq!(
            p.write(&[1], 1, false).unwrap_err(),
            CoreError::FailedPrecondition
        );
        assert_eq!(p.begin_write().unwrap_err(), CoreError::FailedPrecondition);
        let ps = p.query_signals();
        assert!(ps.satisfied.contains(Signals::PEER_CLOSED));
        assert!(!ps.satisfiable.contains(Signals::WRITABLE));
    }

    #[test]
    fn producer_full_signal_state() {
        let (p, c) = pair();
        let full = vec![0x42u8; 64];
        p.write(&full, 64, false).unwrap();
        let ps = p.query_signals();
        assert!(!ps.satisfied.contains(Signals::WRITABLE));
        assert!(ps.satisfiable.contains(Signals::WRITABLE));
        // Consumer consumes 4 -> producer WRITABLE becomes satisfied after the
        // consumer's read pushes the update.
        let mut buf = [0u8; 4];
        c.read(Some(&mut buf), 4, ReadFlags::default()).unwrap();
        let ps = p.query_signals();
        assert!(ps.satisfied.contains(Signals::WRITABLE));
        assert_eq!(p.write(&[1, 2, 3, 4], 4, false).unwrap(), 4);
    }

    #[test]
    fn watch_fires_on_readable() {
        use std::sync::mpsc;
        let (p, c) = pair();
        let (tx, rx) = mpsc::channel();
        let cb: WatchCallback = Arc::new(move |state, _kind| {
            let _ = tx.send(state);
        });
        c.register_watch(Signals::READABLE, cb);
        p.write(&[9], 1, false).unwrap();
        let state = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(state.satisfied.contains(Signals::READABLE));
    }

    #[test]
    fn watch_cancelled_on_local_close() {
        use std::sync::mpsc;
        let (p, _c) = pair();
        let (tx, rx) = mpsc::channel();
        let cb: WatchCallback = Arc::new(move |_state, kind| {
            if kind == crate::trap::WatchKind::Cancelled {
                let _ = tx.send(());
            }
        });
        p.register_watch(Signals::WRITABLE, cb);
        p.close();
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    }
}
