//! The Phase 5 routing acceptor: a full non-broker ipcz node.
//!
//! This is the candidate side of the routing interop court. It implements the
//! pinned ipcz `Router` state machine for a non-broker node over a single
//! NodeLink to the broker:
//!
//! * terminal routers on central and peripheral links, with sequenced parcel
//!   queues and closure propagation (`AcceptInboundParcel`,
//!   `AcceptOutboundParcel`, `AcceptRouteClosureFrom`,
//!   `AcceptRouteDisconnectedFrom`, `Flush`);
//! * portal transfer: `Router::Deserialize` (including the
//!   `proxy_already_bypassed` decaying-link setup) and the proxy-path
//!   serialization (`SerializeNewRouterAndConfigureProxy` +
//!   `BeginProxyingToNewRouter`);
//! * proxy bypass completion: `StopProxying`, `StopProxyingToLocalPeer`, and
//!   the broker's `BypassPeerWithLink` bootstrap bypass;
//! * bridge chains: `MergeRoute` (the invitation attachments are merged onto
//!   the remote initial portals), bridge parcel forwarding over local bridge
//!   links (`AcceptOutboundParcel`), the acceptor's own bridge bypass
//!   (`MaybeStartBridgeBypass` / `StartBridgeBypassFromLocalPeer`), and the
//!   bridge-aware `StopProxyingToLocalPeer` / `AcceptRouteClosureFrom`;
//! * shared `RouterLinkState` coordination (`TryLock`, `SetSideStable`,
//!   `allowed_bypass_request_source`) and the shared sublink allocator.
//!
//! The node is single-threaded (one poll loop), so routers carry no internal
//! locks: the acceptor serializes every operation, matching the official
//! observable state machine. Unsupported inbound messages (`BypassPeer`,
//! `AcceptBypassLink`, `ProxyWillStop`) are rejected explicitly rather than
//! silently ignored.

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::io::{IntoRawFd, RawFd};

use mojo_rs_casefile::events::{Event, EventKind};
use mojo_rs_platform::fd::OwnedFd;
use mojo_rs_platform::socket::socketpair;

use crate::ipcz::channel::{Channel, ChannelError, RecvResult};
use crate::ipcz::link_memory::{
    LinkMemory, LinkMemoryError, MAX_INITIAL_PORTALS, ROUTER_LINK_STATE_SIZE,
};
use crate::ipcz::messages::{
    self, AcceptParcel, DecodedMessage, FragmentDescriptor, MSG_ID_ACCEPT_BYPASS_LINK,
    MSG_ID_ACCEPT_INTRODUCTION, MSG_ID_ACCEPT_PARCEL, MSG_ID_BYPASS_PEER,
    MSG_ID_CONNECT_FROM_BROKER_TO_BROKER, MSG_ID_CONNECT_TO_REFERRED_BROKER,
    MSG_ID_CONNECT_TO_REFERRED_NON_BROKER, MSG_ID_NON_BROKER_REFERRAL_ACCEPTED,
    MSG_ID_NON_BROKER_REFERRAL_REJECTED, MSG_ID_PROXY_WILL_STOP, MSG_ID_REFER_NON_BROKER,
    MSG_ID_REJECT_INTRODUCTION, MSG_ID_REQUEST_INDIRECT_INTRODUCTION, MSG_ID_REQUEST_INTRODUCTION,
    MSG_ID_REQUEST_MEMORY, NodeName, RouterDescriptor, handle_type,
};
use crate::ipcz::router::{Edge, Link, LinkKind, LinkSide, Object, Parcel, Router};
use crate::ipcz::wire::{WireError, set_message_sequence_number};

/// Errors from the routing acceptor.
#[derive(Debug)]
pub enum RoutingError {
    /// A channel-level failure.
    Channel(ChannelError),
    /// A link-memory failure.
    Memory(LinkMemoryError),
    /// A protocol violation or malformed message.
    BadParcel(&'static str),
    /// A wire-format violation in an inbound message.
    Wire(WireError),
    /// An unexpected message at this point of the flow.
    Unexpected(&'static str),
    /// A message type the routing acceptor does not implement.
    Unsupported(u8, &'static str),
    /// An I/O error.
    Io(std::io::Error),
}

impl From<ChannelError> for RoutingError {
    fn from(e: ChannelError) -> Self {
        RoutingError::Channel(e)
    }
}

impl From<LinkMemoryError> for RoutingError {
    fn from(e: LinkMemoryError) -> Self {
        RoutingError::Memory(e)
    }
}

impl From<std::io::Error> for RoutingError {
    fn from(e: std::io::Error) -> Self {
        RoutingError::Io(e)
    }
}

impl From<WireError> for RoutingError {
    fn from(e: WireError) -> Self {
        RoutingError::Wire(e)
    }
}

impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingError::Channel(e) => write!(f, "channel: {e}"),
            RoutingError::Memory(e) => write!(f, "link memory: {e:?}"),
            RoutingError::BadParcel(m) => write!(f, "bad parcel: {m}"),
            RoutingError::Wire(e) => write!(f, "wire: {e}"),
            RoutingError::Unexpected(m) => write!(f, "unexpected: {m}"),
            RoutingError::Unsupported(id, m) => write!(f, "unsupported message {id}: {m}"),
            RoutingError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for RoutingError {}

/// The negotiated initial portal count (max attachments + 1 internal portal,
/// matching the official invitation acceptance).
const ACCEPTOR_INITIAL_PORTALS: u32 = 8;
/// The bootstrap pipe sublink (initial portal 1).
const BOOTSTRAP_SUBLINK: u64 = 1;
/// Router identities for local-only routers (bridge-chain routers have no
/// sublink, so their identities come from a counter far above any sublink id).
const LOCAL_RID_BASE: u64 = 1 << 32;

/// A processed parcel: its payload and any deserialized portal identities.
struct ProcessedParcel {
    /// The application payload.
    payload: Vec<u8>,
    /// Identity sublinks of deserialized portals, in handle order.
    identities: Vec<u64>,
}

/// The Phase 5 routing acceptor state machine.
pub struct RoutingAcceptor {
    /// The channel to the broker (NodeLink 0).
    channel: Channel,
    /// The adopted broker-link memory (set by the Connect handshake).
    link_memory: Option<LinkMemory>,
    /// The direct peer link (NodeLink 1, the multi-node courts' referrer
    /// link), established by the referral acceptance.
    direct: Option<DirectLink>,
    /// Per-link outgoing message sequence number (after Connect).
    next_link_seq: u64,
    /// Routers by identity (their first primary sublink, or a local rid).
    routers: HashMap<u64, Router>,
    /// (NodeLink, sublink) -> owning router identity.
    owners: HashMap<(u64, u64), u64>,
    /// Parcels for sublinks whose router is not yet established.
    early_parcels: HashMap<(u64, u64), VecDeque<AcceptParcel>>,
    /// The broker's node name (from the Connect greeting).
    broker_name: NodeName,
    /// The referrer's node name (multi-node courts; from the referral
    /// acceptance).
    referrer_name: NodeName,
    /// This node's own name (the Connect greeting's receiver name, or the
    /// referral acceptance's assigned name).
    local_name: NodeName,
    /// The app-facing bootstrap pipe router (the invitation attachment).
    bootstrap_rid: u64,
    /// The next identity for local-only routers.
    next_rid: u64,
    /// In-flight `RequestMemory` requests by buffer size, FIFO per size
    /// (`NodeLink::pending_memory_requests_`: the callbacks are keyed by the
    /// requested size and completed in request order on `ProvideMemory`).
    pending_memory_requests: HashMap<u32, VecDeque<PendingMemoryRequest>>,
    /// Block sizes with an in-flight capacity request (one per block size;
    /// mirrors `NodeLinkMemory::capacity_callbacks_`'s `need_new_request`).
    capacity_pending: HashSet<u32>,
    /// Parcels deferred because their data fragment references a buffer not
    /// yet received (`NodeLink::WaitForParcelFragmentToResolve`), keyed by
    /// buffer id and completed when the `AddBlockBuffer` arrives.
    pending_fragments: HashMap<u64, VecDeque<(AcceptParcel, Vec<OwnedFd>)>>,
    /// Events in casefile format.
    events: Vec<Event>,
    /// Event sequence counter.
    event_seq: u64,
}

/// The direct peer NodeLink (link id 1) in the multi-node courts.
pub struct DirectLink {
    /// The channel to the referrer node.
    channel: Channel,
    /// The adopted link memory.
    memory: LinkMemory,
    /// The referrer's node name.
    remote_name: NodeName,
    /// Outgoing message sequence number.
    next_link_seq: u64,
}

/// The NodeLink id of the broker link.
pub const LINK_ID_BROKER: u64 = 0;
/// The NodeLink id of the direct peer link (multi-node courts).
pub const LINK_ID_DIRECT: u64 = 1;

/// A pending `RequestMemory` (a `ProvideMemory` reply is expected): the
/// completion carries the requested buffer size and the block size the
/// caller's `RequestBlockCapacity` lobbied for.
struct PendingMemoryRequest {
    /// The requested buffer size (the `RequestMemory`/`ProvideMemory` size).
    buffer_size: u32,
    /// The block size the completion must initialize and share.
    block_size: u32,
}

impl RoutingAcceptor {
    /// Start the routing acceptor on an inherited socket descriptor.
    pub fn new(fd: std::os::unix::io::RawFd) -> Result<RoutingAcceptor, RoutingError> {
        let channel = Channel::adopt(fd)?;
        Ok(RoutingAcceptor {
            channel,
            link_memory: None,
            direct: None,
            next_link_seq: 0,
            routers: HashMap::new(),
            owners: HashMap::new(),
            early_parcels: HashMap::new(),
            broker_name: NodeName::invalid(),
            referrer_name: NodeName::invalid(),
            local_name: NodeName::invalid(),
            bootstrap_rid: 0,
            next_rid: LOCAL_RID_BASE,
            pending_memory_requests: HashMap::new(),
            capacity_pending: HashSet::new(),
            pending_fragments: HashMap::new(),
            events: Vec::new(),
            event_seq: 0,
        })
    }

    /// Emit an event.
    fn emit(&mut self, op_id: u64, kind: EventKind, result: &str) {
        self.event_seq += 1;
        self.events.push(Event {
            seq: self.event_seq,
            op_id,
            event: kind,
            result: result.to_string(),
            handle: None,
            payload_hex: None,
            handles: None,
            signals: None,
            trigger_context: None,
            signals_state: None,
            outputs: None,
            process: None,
            pid: None,
            fd: None,
            num_bytes: None,
            size: None,
            note: None,
        });
    }

    /// Emit a `message` event with a payload and handle count.
    fn emit_message(&mut self, op_id: u64, payload: &[u8], handles: usize) {
        self.event_seq += 1;
        self.events.push(Event {
            seq: self.event_seq,
            op_id,
            event: EventKind::Message,
            result: "MOJO_RESULT_OK".to_string(),
            handle: None,
            payload_hex: Some(hex::encode(payload)),
            handles: Some((0..handles).map(|i| format!("h{i}")).collect()),
            signals: None,
            trigger_context: None,
            signals_state: None,
            outputs: None,
            process: None,
            pid: None,
            fd: None,
            num_bytes: None,
            size: None,
            note: None,
        });
    }

    /// The events emitted so far.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    fn memory(&self) -> Result<&LinkMemory, RoutingError> {
        self.link_memory
            .as_ref()
            .ok_or(RoutingError::Unexpected("link memory not established"))
    }

    fn memory_mut(&mut self) -> Result<&mut LinkMemory, RoutingError> {
        self.link_memory
            .as_mut()
            .ok_or(RoutingError::Unexpected("link memory not established"))
    }

    /// The link memory of the NodeLink identified by `link_id`.
    fn memory_for(&self, link_id: u64) -> Result<&LinkMemory, RoutingError> {
        if link_id == LINK_ID_DIRECT {
            return self
                .direct
                .as_ref()
                .map(|d| &d.memory)
                .ok_or(RoutingError::Unexpected("direct link not established"));
        }
        self.memory()
    }

    /// The link memory of the NodeLink identified by `link_id` (mutable).
    fn memory_mut_for(&mut self, link_id: u64) -> Result<&mut LinkMemory, RoutingError> {
        if link_id == LINK_ID_DIRECT {
            return self
                .direct
                .as_mut()
                .map(|d| &mut d.memory)
                .ok_or(RoutingError::Unexpected("direct link not established"));
        }
        self.memory_mut()
    }

    /// The channel of the NodeLink identified by `link_id`.
    fn channel_for(&mut self, link_id: u64) -> Result<&mut Channel, RoutingError> {
        if link_id == LINK_ID_DIRECT {
            return self
                .direct
                .as_mut()
                .map(|d| &mut d.channel)
                .ok_or(RoutingError::Unexpected("direct link not established"));
        }
        Ok(&mut self.channel)
    }

    /// The remote node name of the NodeLink identified by `link_id`.
    fn remote_name_for(&self, link_id: u64) -> Result<NodeName, RoutingError> {
        if link_id == LINK_ID_DIRECT {
            return self
                .direct
                .as_ref()
                .map(|d| d.remote_name)
                .ok_or(RoutingError::Unexpected("direct link not established"));
        }
        Ok(self.broker_name)
    }

    /// Run the full routing scenario.
    pub fn run(&mut self) -> Result<(), RoutingError> {
        self.emit(0, EventKind::Lifecycle, "MOJO_RESULT_OK");
        self.connect()?;
        self.emit(1, EventKind::Result, "MOJO_RESULT_OK");
        self.emit(2, EventKind::Result, "MOJO_RESULT_OK");

        // Step 1: receive the transferred portal on the bootstrap pipe. The
        // broker serialized it with `SerializeNewRouterWithLocalPeer`
        // (`proxy_already_bypassed = true`).
        let (transfer, transfer_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::AcceptParcel(p)
                    if p.sublink == BOOTSTRAP_SUBLINK && p.handle_types.contains(&handle_type::PORTAL)
            )
        })?;
        // The transfer's arrival marks the point where the oracle's side-B
        // stable marks have become observable to the broker; mark ours now so
        // this side wins the bridge-bypass lock, matching the baseline.
        self.mark_initial_links_stable()?;
        let transfer_payload;
        let b1_identity;
        match transfer {
            DecodedMessage::AcceptParcel(p) => {
                let processed = self.process_accept_parcel(p, transfer_fds, LINK_ID_BROKER)?;
                if processed.identities.len() != 1 {
                    return Err(RoutingError::BadParcel(
                        "expected exactly one transferred portal",
                    ));
                }
                transfer_payload = processed.payload;
                b1_identity = processed.identities[0];
            }
            _ => return Err(RoutingError::Unexpected("transfer predicate misrouted")),
        }
        if transfer_payload != b"transfer-b1" {
            return Err(RoutingError::BadParcel("unexpected transfer payload"));
        }
        self.emit_message(3, &transfer_payload, 1);

        // Step 2: receive w1 on the transferred portal's route. The broker's
        // WithLocalPeer serialization leaves the local peer (A) with a new
        // central link to B' and forwards any parcel A already queued (w1)
        // over the decaying peripheral link (new_sublink + 1); the parcel can
        // therefore arrive on either the central or the decaying sublink.
        // Any routing messages in between (the broker's bootstrap-route bridge
        // bypass, the portal-0 closure) are dispatched to the routers.
        let b1_sublinks: Vec<u64> = {
            let router = self
                .routers
                .get(&b1_identity)
                .ok_or(RoutingError::BadParcel("b1 router missing"))?;
            let mut out = Vec::with_capacity(2);
            if let Some(l) = &router.outward.primary {
                out.push(l.sublink);
            }
            if let Some(l) = router.outward.decaying_link() {
                out.push(l.sublink);
            }
            out
        };
        let (w1, w1_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::AcceptParcel(p)
                    if p.handle_types.is_empty() && b1_sublinks.contains(&p.sublink)
            )
        })?;
        let w1_payload = match w1 {
            DecodedMessage::AcceptParcel(p) => {
                self.process_accept_parcel(p, w1_fds, LINK_ID_BROKER)?
                    .payload
            }
            _ => return Err(RoutingError::Unexpected("w1 predicate misrouted")),
        };
        if w1_payload != b"w1" {
            return Err(RoutingError::BadParcel("unexpected w1 payload"));
        }
        self.emit_message(4, &w1_payload, 0);

        // Drain any routing messages the broker sent behind w1 (its bridge
        // bypass of the bootstrap route, and the portal-0 closure) so the
        // bootstrap router transmits on its migrated primary sublink below.
        self.drain_available()?;

        // The broker's own bridge bypass is a deterministic response to the
        // `FlushRouter` we just sent (its deferred bypass attempt unblocks once
        // our side of the bypass link is stable). The oracle acceptor's IO
        // thread processes it before the application's next puts, so the
        // transfer-back rides on the broker-assigned sublink (15 in the
        // baseline); the single-threaded candidate waits for it explicitly to
        // reproduce the same observable ordering.
        let bootstrap_now = self.bootstrap_sublink()?;
        let (bb, bb_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::BypassPeerWithLink(b) if b.sublink == bootstrap_now
            )
        })?;
        if let DecodedMessage::BypassPeerWithLink(b) = bb {
            self.dispatch(
                DecodedMessage::BypassPeerWithLink(b),
                bb_fds,
                LINK_ID_BROKER,
            )?;
        }

        // Step 3: send r1 over the wire on the b1 route.
        self.put(b1_identity, b"r1".to_vec(), Vec::new())?;
        self.emit(5, EventKind::Result, "MOJO_RESULT_OK");

        // Step 4: send the transfer-back on the bootstrap with the b1 handle.
        // The bootstrap (attachment) router transmits on its current primary
        // sublink (it migrated to the broker's bypass link above).
        self.put(
            self.bootstrap_rid,
            b"transfer-back".to_vec(),
            vec![Object::Router(b1_identity)],
        )?;
        self.emit(6, EventKind::Result, "MOJO_RESULT_OK");

        // Step 5: the broker completes the bypass; the proxy on this node
        // receives StopProxying and decays away.
        let (sp, _sp_fds) = self.recv_until(
            |d| matches!(d, DecodedMessage::StopProxying(s) if s.sublink == b1_identity),
        )?;
        if let DecodedMessage::StopProxying(s) = sp {
            self.dispatch(DecodedMessage::StopProxying(s), Vec::new(), LINK_ID_BROKER)?;
        }
        self.emit(7, EventKind::Result, "MOJO_RESULT_OK");

        // Step 6: the broker closes its bootstrap end; RouteClosed arrives on
        // the bootstrap route's current primary sublink.
        let bootstrap_now = self.bootstrap_sublink()?;
        let (rc, rc_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::RouteClosed(r) if r.sublink == bootstrap_now
            )
        })?;
        if let DecodedMessage::RouteClosed(r) = rc {
            self.dispatch(DecodedMessage::RouteClosed(r), rc_fds, LINK_ID_BROKER)?;
        }
        self.emit(8, EventKind::Message, "MOJO_RESULT_FAILED_PRECONDITION");

        // Step 7: close the bootstrap portal locally. The broker already
        // closed its end, so no closure message is transmitted (the primary
        // link was released when the peer's RouteClosed arrived).
        self.close_route(self.bootstrap_rid)?;
        self.emit(9, EventKind::Lifecycle, "MOJO_RESULT_OK");
        Ok(())
    }

    /// The memory-expansion scenario (`invite-broker-memory` /
    /// `invite-acceptor-memory`): seals the parcel-fragment allocation and
    /// free-list-reuse semantics against the official broker.
    ///
    /// The primary buffer's 256-byte block pool holds exactly 8 allocable
    /// blocks, so the 9th parcel of 200 bytes (m8) falls back to inline data
    /// (the pinned mojo embedder sets `IPCZ_MEMORY_FIXED_PARCEL_CAPACITY`,
    /// disabling parcel-data expansion). The broker reads m0..m8 only after
    /// the sync marker, freeing the blocks (LIFO); m9 and m10 then reuse the
    /// freed blocks from the primary buffer — the free-list reuse is part of
    /// the seal (fragment offsets must match the baseline). The `RequestMemory` /
    /// `ProvideMemory` machinery is implemented but not exercised by this
    /// court: its only reachable trigger in this epoch is `RouterLinkState`
    /// pool exhaustion, which requires the proxy-bypass machinery (the next
    /// Phase 5 gate).
    ///
    /// Event sequence mirrors the oracle acceptor driver:
    /// 0 lifecycle, 1-2 result (connect), 3 message (transfer-b1), 4 message
    /// (w1), 5-13 result (m0..m8), 14 result (sync), 15 result (transfer-back),
    /// 16 message (w3), 17-18 result (m9, m10), 19 message (peer closed),
    /// 20 result (close), 21 lifecycle.
    pub fn run_memory(&mut self) -> Result<(), RoutingError> {
        self.emit(0, EventKind::Lifecycle, "MOJO_RESULT_OK");
        self.connect()?;
        self.emit(1, EventKind::Result, "MOJO_RESULT_OK");
        self.emit(2, EventKind::Result, "MOJO_RESULT_OK");

        // Step 1: receive the transferred portal on the bootstrap pipe.
        let (transfer, transfer_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::AcceptParcel(p)
                    if p.sublink == BOOTSTRAP_SUBLINK && p.handle_types.contains(&handle_type::PORTAL)
            )
        })?;
        self.mark_initial_links_stable()?;
        let b1_identity;
        match transfer {
            DecodedMessage::AcceptParcel(p) => {
                let processed = self.process_accept_parcel(p, transfer_fds, LINK_ID_BROKER)?;
                if processed.identities.len() != 1 {
                    return Err(RoutingError::BadParcel(
                        "expected exactly one transferred portal",
                    ));
                }
                b1_identity = processed.identities[0];
                if processed.payload != b"transfer-b1" {
                    return Err(RoutingError::BadParcel("unexpected transfer payload"));
                }
            }
            _ => return Err(RoutingError::Unexpected("transfer predicate misrouted")),
        }
        self.emit_message(3, b"transfer-b1", 1);

        // Step 2: receive w1 on the transferred portal's route.
        let b1_sublinks: Vec<u64> = {
            let router = self
                .routers
                .get(&b1_identity)
                .ok_or(RoutingError::BadParcel("b1 router missing"))?;
            let mut out = Vec::with_capacity(2);
            if let Some(l) = &router.outward.primary {
                out.push(l.sublink);
            }
            if let Some(l) = router.outward.decaying_link() {
                out.push(l.sublink);
            }
            out
        };
        let (w1, w1_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::AcceptParcel(p)
                    if p.handle_types.is_empty() && b1_sublinks.contains(&p.sublink)
            )
        })?;
        let w1_payload = match w1 {
            DecodedMessage::AcceptParcel(p) => {
                self.process_accept_parcel(p, w1_fds, LINK_ID_BROKER)?
                    .payload
            }
            _ => return Err(RoutingError::Unexpected("w1 predicate misrouted")),
        };
        if w1_payload != b"w1" {
            return Err(RoutingError::BadParcel("unexpected w1 payload"));
        }
        self.emit_message(4, &w1_payload, 0);
        // The broker's own bridge bypass follows the `FlushRouter` we sent;
        // process it so the transfer-back rides on the broker-assigned sublink
        // (deterministic ordering, as in `run`).
        let bootstrap_now = self.bootstrap_sublink()?;
        let (bb, bb_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::BypassPeerWithLink(b) if b.sublink == bootstrap_now
            )
        })?;
        if let DecodedMessage::BypassPeerWithLink(b) = bb {
            self.dispatch(
                DecodedMessage::BypassPeerWithLink(b),
                bb_fds,
                LINK_ID_BROKER,
            )?;
        }

        // Step 3: m0..m8 on B'. The 8 primary 256-byte blocks are consumed by
        // m0..m7; m8's fragment allocation fails (the mojo embedder disables
        // parcel-data expansion), so m8 travels inline — exactly like the
        // oracle baseline.
        for i in 0..9u32 {
            let frag = self.put(b1_identity, Self::memory_payload(i), Vec::new())?;
            if i < 8 {
                // m0..m7 are fragment-backed from the primary 256-block pool.
                let f = frag.ok_or(RoutingError::BadParcel(
                    "pre-exhaustion parcel unexpectedly inline",
                ))?;
                if f.buffer_id != crate::ipcz::link_memory::PRIMARY_BUFFER_ID {
                    return Err(RoutingError::BadParcel(
                        "pre-exhaustion parcel not from the primary buffer",
                    ));
                }
            } else if frag.is_some() {
                // m8's allocation must fail (all 8 blocks consumed).
                return Err(RoutingError::BadParcel(
                    "exhaustion parcel unexpectedly fragment-backed",
                ));
            }
            self.emit(5 + i as u64, EventKind::Result, "MOJO_RESULT_OK");
        }

        // Step 4: the sync marker on the bootstrap pipe; the broker reads m0..m8
        // only after receiving it (so the 256-blocks stay allocated at m8's put).
        self.put(self.bootstrap_rid, b"sync".to_vec(), Vec::new())?;
        self.emit(14, EventKind::Result, "MOJO_RESULT_OK");

        // Step 5: the transfer-back on the bootstrap with the b1 handle. The
        // `ProvideMemory` round trip completes during this exchange.
        self.put(
            self.bootstrap_rid,
            b"transfer-back".to_vec(),
            vec![Object::Router(b1_identity)],
        )?;
        self.emit(15, EventKind::Result, "MOJO_RESULT_OK");

        // Step 6: drain the broker's routing messages (its bypass of the
        // bootstrap route, the proxy teardown) and wait for w3. The broker
        // sends w3 after its own w2 round trip, so by the time it arrives the
        // broker has read (and freed) m0..m8 — the m9/m10 allocations below
        // are therefore deterministically served by the freed primary blocks.
        let bootstrap_now = self.bootstrap_sublink()?;
        let (w3, w3_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::AcceptParcel(p)
                    if p.sublink == bootstrap_now && p.handle_types.is_empty()
            )
        })?;
        let w3_payload = match w3 {
            DecodedMessage::AcceptParcel(p) => {
                self.process_accept_parcel(p, w3_fds, LINK_ID_BROKER)?
                    .payload
            }
            _ => return Err(RoutingError::Unexpected("w3 predicate misrouted")),
        };
        if w3_payload != b"w3" {
            return Err(RoutingError::BadParcel("unexpected w3 payload"));
        }
        self.emit_message(16, &w3_payload, 0);

        // Step 7: m9 and m10 on the bootstrap pipe — fragment-backed from the
        // primary buffer, reusing the blocks the broker freed when it read
        // m0..m8 (LIFO free-list: m9 gets block 8, m10 gets block 7).
        for (i, op) in [(9u32, 17u64), (10, 18)] {
            let frag = self.put(self.bootstrap_rid, Self::memory_payload(i), Vec::new())?;
            let f = frag.ok_or(RoutingError::BadParcel(
                "post-read parcel unexpectedly inline",
            ))?;
            if f.buffer_id != crate::ipcz::link_memory::PRIMARY_BUFFER_ID {
                return Err(RoutingError::BadParcel(
                    "post-read parcel not from the primary buffer",
                ));
            }
            if f.size != 256 {
                return Err(RoutingError::BadParcel(
                    "post-read parcel has the wrong block size",
                ));
            }
            self.emit(op, EventKind::Result, "MOJO_RESULT_OK");
        }

        // Step 8: the broker closes its bootstrap end; RouteClosed arrives on
        // the bootstrap route's current primary sublink.
        let bootstrap_now = self.bootstrap_sublink()?;
        let (rc, rc_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::RouteClosed(r) if r.sublink == bootstrap_now
            )
        })?;
        if let DecodedMessage::RouteClosed(r) = rc {
            self.dispatch(DecodedMessage::RouteClosed(r), rc_fds, LINK_ID_BROKER)?;
        }
        self.emit(19, EventKind::Message, "MOJO_RESULT_FAILED_PRECONDITION");

        // Step 9: close the bootstrap portal locally.
        self.close_route(self.bootstrap_rid)?;
        self.emit(20, EventKind::Result, "MOJO_RESULT_OK");
        self.emit(21, EventKind::Lifecycle, "MOJO_RESULT_OK");
        Ok(())
    }

    /// The block-capacity exhaustion scenario (`invite-broker-exhaust` /
    /// `invite-acceptor-exhaust`): seals the RECEIVE side of the
    /// `RequestBlockCapacity` expansion against the official broker.
    ///
    /// Each portal transfer through the bootstrap pipe consumes one 64-byte
    /// `RouterLinkState` block on the broker (both ends hold their pairs, so
    /// nothing is freed). The primary buffer's 64-byte pool holds 1483
    /// allocable blocks; at the 1482nd transfer the broker's
    /// `TryAllocateRouterLinkState` fails, so:
    ///
    /// * that transfer falls back to the plain proxy path
    ///   (`SerializeNewRouterAndConfigureProxy`, no proxy-bypass fields — the
    ///   transferred router's outward peer is local);
    /// * the broker lobbies `RequestBlockCapacity(64)` (unconditional lobby),
    ///   allocates a 64 KiB buffer locally, and shares it via
    ///   `AddBlockBuffer{id=1, 64}` — this acceptor adopts it
    ///   (`OnAddBlockBuffer`) and resolves the later transfers' `RouterLinkState`
    ///   fragments from buffer 1 (cross-buffer fragment resolution);
    /// * the broker's proxy for the 1482nd transfer runs
    ///   `StartSelfBypassToLocalPeer` (its outward peer is local) and sends
    ///   `BypassPeerWithLink` with a state from the new buffer — this
    ///   acceptor adopts the bypass with the sealed routing-court machinery.
    ///
    /// The acceptor's own `RequestMemory` send path is not exercised: the
    /// acceptor never exhausts its own pool in this scenario, and the epoch's
    /// mojo embedder disables parcel-data expansion (see STATUS.md).
    ///
    /// Event sequence mirrors the oracle acceptor driver: 0 lifecycle, 1-2
    /// result (connect), 3 message (transfer-b1), 4 message (w1), 5..1489
    /// messages (transfer-2..1486), 1490 message (peer closed), 1491 result
    /// (close), 1492 lifecycle.
    pub fn run_exhaust(&mut self) -> Result<(), RoutingError> {
        const EXHAUST_TRANSFERS: u32 = 1486;
        self.emit(0, EventKind::Lifecycle, "MOJO_RESULT_OK");
        self.connect()?;
        self.emit(1, EventKind::Result, "MOJO_RESULT_OK");
        self.emit(2, EventKind::Result, "MOJO_RESULT_OK");

        // The broker's IO thread flushes asynchronously, so the transfers
        // arrive out of route-sequence order (and the w1 may arrive before the
        // transfer-b1). Each portal-bearing parcel is therefore verified
        // against its route sequence number (`rseq 0` is transfer-b1, `rseq k`
        // is transfer-{k+1}); the routed delivery reorders via the sequenced
        // queue; parcels for not-yet-established sublinks (the w1) are
        // deferred in `early_parcels`.
        let mut b1_identity: Option<u64> = None;
        let mut transfer_count = 0u32;
        let mut w1_seen = false;
        while transfer_count < EXHAUST_TRANSFERS || !w1_seen {
            // Every AcceptParcel from the broker is either a portal transfer
            // or the w1; the routing messages are dispatched by `recv_until`.
            let (msg, fds) = self.recv_until(|d| matches!(d, DecodedMessage::AcceptParcel(_)))?;
            match msg {
                DecodedMessage::AcceptParcel(p)
                    if p.handle_types.contains(&handle_type::PORTAL) =>
                {
                    let rseq = p.sequence_number;
                    let processed_parcel = self.process_accept_parcel(p, fds, LINK_ID_BROKER)?;
                    let expected = if rseq == 0 {
                        b"transfer-b1".to_vec()
                    } else {
                        format!("transfer-{}", rseq + 1).into_bytes()
                    };
                    if processed_parcel.payload != expected {
                        return Err(RoutingError::BadParcel("unexpected transfer payload"));
                    }
                    if rseq == 0 {
                        if processed_parcel.identities.len() != 1 {
                            return Err(RoutingError::BadParcel(
                                "expected exactly one transferred portal",
                            ));
                        }
                        b1_identity = Some(processed_parcel.identities[0]);
                    }
                    // The first portal parcel marks the point where the
                    // oracle's side-B stable marks become observable.
                    if transfer_count == 0 {
                        self.mark_initial_links_stable()?;
                    }
                    self.emit_message(3 + transfer_count as u64, &processed_parcel.payload, 1);
                    transfer_count += 1;
                }
                DecodedMessage::AcceptParcel(p) => {
                    // The only no-handle parcel from the broker is w1 (on the
                    // transferred pipe's route; it may be deferred in
                    // `early_parcels` until the transfer-b1's router exists).
                    let payload = self.process_accept_parcel(p, fds, LINK_ID_BROKER)?.payload;
                    if payload != b"w1" {
                        return Err(RoutingError::BadParcel("unexpected w1 payload"));
                    }
                    w1_seen = true;
                    self.emit_message(4, &payload, 0);
                }
                _ => return Err(RoutingError::Unexpected("exhaust predicate misrouted")),
            }
        }

        // Step 4: the broker closed its bootstrap end; RouteClosed arrives on
        // the bootstrap route's current primary sublink.
        let bootstrap_now = self.bootstrap_sublink()?;
        let (rc, rc_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::RouteClosed(r) if r.sublink == bootstrap_now
            )
        })?;
        if let DecodedMessage::RouteClosed(r) = rc {
            self.dispatch(DecodedMessage::RouteClosed(r), rc_fds, LINK_ID_BROKER)?;
        }
        self.emit(
            3 + (EXHAUST_TRANSFERS as u64 - 1) * 2 + 1,
            EventKind::Message,
            "MOJO_RESULT_FAILED_PRECONDITION",
        );

        // Step 5: close the bootstrap portal locally.
        self.close_route(self.bootstrap_rid)?;
        self.emit(
            3 + (EXHAUST_TRANSFERS as u64 - 1) * 2 + 2,
            EventKind::Result,
            "MOJO_RESULT_OK",
        );
        self.emit(
            3 + (EXHAUST_TRANSFERS as u64 - 1) * 2 + 3,
            EventKind::Lifecycle,
            "MOJO_RESULT_OK",
        );
        Ok(())
    }

    /// The acceptor-initiated block-capacity exhaustion scenario
    /// (`invite-broker-bypass` / `invite-acceptor-bypass`): seals the SEND
    /// side of the `RequestMemory`/`ProvideMemory`/`AddBlockBuffer` round
    /// trip.
    ///
    /// The scenario opens with the sealed routing-court prelude (the broker
    /// transfers a portal `B` and writes `w1`, which anchors the side-B stable
    /// marks and the bridge-bypass ordering). The acceptor then creates fresh
    /// local pairs and transfers one end of each through the bootstrap pipe
    /// (`SerializeNewRouterWithLocalPeer`): each transfer allocates ONE
    /// `RouterLinkState` from the shared 64-byte pool (held while both ends of
    /// the pair stay open). When the pool exhausts,
    /// `TryAllocateRouterLinkState` fails and its unconditional lobby sends
    /// `RequestBlockCapacity(64)` -> `RequestMemory` to the broker; the
    /// broker's `ProvideMemory` is adopted by `on_provide_memory`, which
    /// shares the new buffer via `AddBlockBuffer` (the native SEND being
    /// sealed) and registers it locally; subsequent transfers resolve their
    /// link states from the new buffer (cross-buffer fragment resolution).
    ///
    /// The broker's event stream is the primary equivalence (byte-identical
    /// between the oracle-acceptor baseline and the native). The exhaustion
    /// point itself is allocation-interleaving dependent (the peer's IO
    /// thread frees the transfer payload fragments concurrently) and is a
    /// documented normalized residual.
    pub fn run_bypass(&mut self) -> Result<(), RoutingError> {
        const BYPASS_TRANSFERS: u32 = 1520;
        self.emit(0, EventKind::Lifecycle, "MOJO_RESULT_OK");
        self.connect()?;
        self.emit(1, EventKind::Result, "MOJO_RESULT_OK");
        self.emit(2, EventKind::Result, "MOJO_RESULT_OK");

        // Step 1: receive the transferred portal on the bootstrap pipe (the
        // routing-court prelude; the transfer's arrival marks the point where
        // the oracle's side-B stable marks have become observable).
        let (transfer, transfer_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::AcceptParcel(p)
                    if p.sublink == BOOTSTRAP_SUBLINK && p.handle_types.contains(&handle_type::PORTAL)
            )
        })?;
        self.mark_initial_links_stable()?;
        let b1_identity;
        match transfer {
            DecodedMessage::AcceptParcel(p) => {
                let processed = self.process_accept_parcel(p, transfer_fds, LINK_ID_BROKER)?;
                if processed.identities.len() != 1 {
                    return Err(RoutingError::BadParcel(
                        "expected exactly one transferred portal",
                    ));
                }
                b1_identity = processed.identities[0];
                if processed.payload != b"transfer-b1" {
                    return Err(RoutingError::BadParcel("unexpected transfer payload"));
                }
            }
            _ => return Err(RoutingError::Unexpected("transfer predicate misrouted")),
        }
        self.emit_message(3, b"transfer-b1", 1);

        // Step 2: receive w1 on the transferred portal's route (the broker's
        // WithLocalPeer serialization forwards it over the decaying peripheral
        // link).
        let b1_sublinks: Vec<u64> = {
            let router = self
                .routers
                .get(&b1_identity)
                .ok_or(RoutingError::BadParcel("b1 router missing"))?;
            let mut out = Vec::with_capacity(2);
            if let Some(l) = &router.outward.primary {
                out.push(l.sublink);
            }
            if let Some(l) = router.outward.decaying_link() {
                out.push(l.sublink);
            }
            out
        };
        let (w1, w1_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::AcceptParcel(p)
                    if p.handle_types.is_empty() && b1_sublinks.contains(&p.sublink)
            )
        })?;
        let w1_payload = match w1 {
            DecodedMessage::AcceptParcel(p) => {
                self.process_accept_parcel(p, w1_fds, LINK_ID_BROKER)?
                    .payload
            }
            _ => return Err(RoutingError::Unexpected("w1 predicate misrouted")),
        };
        if w1_payload != b"w1" {
            return Err(RoutingError::BadParcel("unexpected w1 payload"));
        }
        self.emit_message(4, &w1_payload, 0);

        // Drain the broker's routing messages, then wait for its own bridge
        // bypass of the bootstrap route so the transfers ride the settled
        // primary sublink (deterministic ordering, as in `run`).
        self.drain_available()?;
        let bootstrap_now = self.bootstrap_sublink()?;
        let (bb, bb_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::BypassPeerWithLink(b) if b.sublink == bootstrap_now
            )
        })?;
        if let DecodedMessage::BypassPeerWithLink(b) = bb {
            self.dispatch(
                DecodedMessage::BypassPeerWithLink(b),
                bb_fds,
                LINK_ID_BROKER,
            )?;
        }
        self.drain_available()?;

        // Step 3: the transfer loop. `held` keeps the local ends of the pairs
        // open so the transferred routers' link states stay allocated. Draining
        // after each put adopts the broker's `ProvideMemory` promptly.
        let mut held: Vec<u64> = Vec::with_capacity(BYPASS_TRANSFERS as usize);
        let mut expansion_seen = false;
        for i in 0..BYPASS_TRANSFERS {
            let (p1, p2) = self.open_portals()?;
            let payload = format!("transfer-{i}").into_bytes();
            self.put(self.bootstrap_rid, payload, vec![Object::Router(p2)])?;
            held.push(p1);
            self.drain_available()?;
            // Track the expansion: once the `ProvideMemory` was adopted, an
            // extra block buffer exists.
            let extra = self
                .memory()
                .map(|m| m.extra_buffer_ids().len())
                .unwrap_or(0);
            if extra > 0 {
                expansion_seen = true;
            }
            self.emit(3 + i as u64, EventKind::Result, "MOJO_RESULT_OK");
        }
        if !expansion_seen {
            return Err(RoutingError::Unexpected(
                "no block-capacity expansion observed",
            ));
        }
        let extra_buffers = self
            .memory()
            .map(|m| m.extra_buffer_ids())
            .unwrap_or_default();
        if extra_buffers.iter().any(|id| *id == 0) || extra_buffers.is_empty() {
            return Err(RoutingError::BadParcel(
                "expansion did not register a new buffer",
            ));
        }
        drop(held);

        // Step 4: the sync marker; the broker verifies it after all transfers.
        self.put(self.bootstrap_rid, b"sync".to_vec(), Vec::new())?;
        self.emit(
            3 + BYPASS_TRANSFERS as u64,
            EventKind::Result,
            "MOJO_RESULT_OK",
        );

        // Step 5: the broker closes its bootstrap end; RouteClosed arrives on
        // the bootstrap route's current primary sublink.
        let bootstrap_now = self.bootstrap_sublink()?;
        let (rc, rc_fds) = self.recv_until(|d| {
            matches!(
                d,
                DecodedMessage::RouteClosed(r) if r.sublink == bootstrap_now
            )
        })?;
        if let DecodedMessage::RouteClosed(r) = rc {
            self.dispatch(DecodedMessage::RouteClosed(r), rc_fds, LINK_ID_BROKER)?;
        }
        self.emit(
            4 + BYPASS_TRANSFERS as u64,
            EventKind::Message,
            "MOJO_RESULT_FAILED_PRECONDITION",
        );

        // Step 6: close the bootstrap portal locally.
        self.close_route(self.bootstrap_rid)?;
        self.emit(
            5 + BYPASS_TRANSFERS as u64,
            EventKind::Result,
            "MOJO_RESULT_OK",
        );
        self.emit(
            6 + BYPASS_TRANSFERS as u64,
            EventKind::Lifecycle,
            "MOJO_RESULT_OK",
        );
        Ok(())
    }

    /// A deterministic 200-byte memory-court payload: `m{NN}` plus padding.
    fn memory_payload(i: u32) -> Vec<u8> {
        let mut p = format!("m{i:02}").into_bytes();
        p.resize(200, b'x');
        p
    }

    /// The bootstrap (attachment) router's current primary sublink (its
    /// identity stays `BOOTSTRAP_SUBLINK`-derived; the primary migrates on
    /// bypass).
    fn bootstrap_sublink(&self) -> Result<u64, RoutingError> {
        self.routers
            .get(&self.bootstrap_rid)
            .and_then(|r| r.outward.primary.as_ref())
            .map(|l| l.sublink)
            .ok_or(RoutingError::Unexpected(
                "bootstrap router has no primary link",
            ))
    }

    /// Complete the Connect handshake and establish the initial portals.
    fn connect(&mut self) -> Result<(), RoutingError> {
        let msg = self
            .channel
            .recv()?
            .ok_or(RoutingError::Unexpected("peer closed before Connect"))?;
        let decoded = messages::decode_message(&msg.payload, msg.fds.len())?;
        let DecodedMessage::ConnectFromBrokerToNonBroker(connect) = decoded else {
            return Err(RoutingError::Unexpected(
                "first message was not ConnectFromBrokerToNonBroker",
            ));
        };
        if connect.num_initial_portals > MAX_INITIAL_PORTALS as u32 {
            return Err(RoutingError::BadParcel("excessive initial portals"));
        }
        let mut fds = msg.fds;
        if fds.len() != 1 {
            return Err(RoutingError::Unexpected("Connect carries no link memory"));
        }
        let mem_fd = fds.remove(0).into_raw_fd();
        self.link_memory = Some(LinkMemory::adopt_primary(mem_fd)?);
        self.broker_name = connect.broker_name;
        self.local_name = connect.receiver_name;

        let reply = messages::encode_connect_from_non_broker_to_broker(ACCEPTOR_INITIAL_PORTALS, 0);
        self.channel.send(&reply, &[])?;

        let broker_portals = connect.num_initial_portals as usize;
        for i in 0..broker_portals.min(MAX_INITIAL_PORTALS) {
            let sublink = i as u64;
            let state = FragmentDescriptor {
                buffer_id: 0,
                offset: crate::ipcz::link_memory::LinkMemory::initial_link_state_offset(i) as u32,
                size: ROUTER_LINK_STATE_SIZE as u32,
            };
            let link = Link::remote(0, sublink, LinkKind::Central, LinkSide::B, Some(state));
            let router = if i == 1 {
                // The bootstrap portal (side B of the initial central link).
                Router::new_terminal(link)
            } else {
                let mut r = Router::bare();
                r.outward.set_primary_link(link);
                r
            };
            self.owners.insert((LINK_ID_BROKER, sublink), sublink);
            self.routers.insert(sublink, router);
        }
        // The internal portal 0 carries the shared-memory-service client
        // handshake (`BaseSharedMemoryService::CreateClient`): a boxed
        // Transport endpoint is `Put` on portal 0, then the portal closes.
        // Reproduced byte-exactly (golden fixture `acceptor-to-broker.bin`
        // messages 1-2) so the routing wire capture matches the baseline.
        self.send_shared_memory_client()?;
        // `Invitation::Accept` then merges each attachment slot onto its
        // initial portal (`OpenPortals` + `MergePortals(portals[i+1],
        // bridge)`). The bootstrap pipe (initial portal 1) therefore becomes a
        // three-router bridge chain:
        //
        //   attachment (app-facing) ⟷ R_bridge ⟷ [bridge edge] ⟷ R_remote ⟷
        //   [NodeLink sublink 1] ⟷ broker
        //
        // The broker's end mirrors the chain, so both ends independently run
        // bridge bypass (`MaybeStartBridgeBypass`) to collapse it.
        //
        // NOTE on the side-B stable marks: the official non-broker marks its
        // initial links stable when the Connect handshake completes. The
        // baseline wire shows the oracle acceptor *winning* the sub-1 bypass
        // lock race: the broker's early `BypassPeerWithLink` attempts all fail
        // (its side observes the acceptor's side-B stable only after the
        // transfer arrives), so the acceptor initiates. The candidate matches
        // that observable ordering by marking the initial links stable at the
        // same point in the exchange (just before the first parcel arrives),
        // not at Connect time.
        self.setup_bootstrap_bridge()?;
        self.router_flush(BOOTSTRAP_SUBLINK, false)?;
        Ok(())
    }

    /// Mark this side stable on the initial portal links (the official
    /// `SetOutwardLink`'s `MarkSideStable` for the initial routers). Timed to
    /// match the oracle's observable ordering: after the broker has processed
    /// the Connect reply (so its early bridge-bypass lock attempts defer), but
    /// before the first parcel is processed (so this side's own bypass lock
    /// attempt succeeds).
    fn mark_initial_links_stable(&mut self) -> Result<(), RoutingError> {
        // The initial portals 0 and 1 are the only ones the broker offered
        // (num_initial_portals = 2 in the routing court).
        for i in 0..MAX_INITIAL_PORTALS.min(2) {
            let state = FragmentDescriptor {
                buffer_id: 0,
                offset: crate::ipcz::link_memory::LinkMemory::initial_link_state_offset(i) as u32,
                size: ROUTER_LINK_STATE_SIZE as u32,
            };
            self.memory()?.set_side_stable(state, false)?;
        }
        Ok(())
    }

    /// Build the bootstrap bridge chain: the app-facing attachment router and
    /// the interior bridge router, linked to the initial portal 1 router
    /// (`R_remote`, identity `BOOTSTRAP_SUBLINK`) by a local bridge link, and
    /// to each other by a local central link.
    ///
    /// Mirrors `Invitation::Accept`'s per-attachment `OpenPortals` +
    /// `MergePortals(portals[i + 1], bridge)`: `OpenPortals` creates the
    /// attachment/bridge pair linked by a local central link (born stable);
    /// `MergeRoute` links the bridge router and the initial portal router with
    /// a local bridge link (born unstable).
    fn setup_bootstrap_bridge(&mut self) -> Result<(), RoutingError> {
        let r_bridge_rid = self.next_rid;
        self.next_rid += 1;
        let attachment_rid = self.next_rid;
        self.next_rid += 1;
        self.bootstrap_rid = attachment_rid;

        // OpenPortals: attachment (side A) ⟷ R_bridge (side B), local central,
        // kStable.
        let (attach_link, bridge_central_link) = Link::local_pair(
            LinkKind::Central,
            LinkSide::A,
            attachment_rid,
            r_bridge_rid,
            true,
        );
        // MergeRoute: R_remote (side A) ⟷ R_bridge (side B), local bridge,
        // kUnstable.
        let (remote_bridge_link, bridge_bridge_link) = Link::local_pair(
            LinkKind::Bridge,
            LinkSide::A,
            BOOTSTRAP_SUBLINK,
            r_bridge_rid,
            false,
        );

        let mut attachment = Router::bare();
        attachment.outward.set_primary_link(attach_link);
        let mut r_bridge = Router::bare();
        r_bridge.outward.set_primary_link(bridge_central_link);
        r_bridge.bridge = Some(Edge {
            primary: Some(bridge_bridge_link),
            decaying: None,
        });
        {
            let r_remote = self
                .routers
                .get_mut(&BOOTSTRAP_SUBLINK)
                .ok_or(RoutingError::Unexpected("bootstrap router missing"))?;
            r_remote.bridge = Some(Edge {
                primary: Some(remote_bridge_link),
                decaying: None,
            });
        }
        self.routers.insert(r_bridge_rid, r_bridge);
        self.routers.insert(attachment_rid, attachment);
        Ok(())
    }

    /// The shared-memory-service client handshake on the internal portal 0.
    ///
    /// The official `CreateClient` boxes the remote end of a fresh
    /// `PlatformChannel` as a `Transport` (destination `kNonBroker`) and puts
    /// it on portal 0, then drops the portal, closing the route. The boxed
    /// object is `ObjectHeader{size=8, type=kTransport=0}` + `TransportHeader`
    /// `{destination_type=kNonBroker=1, is_same_remote_process=0,
    /// is_peer_trusted=0, is_trusted_by_peer=0, reserved=0}`.
    fn send_shared_memory_client(&mut self) -> Result<(), RoutingError> {
        let mut obj = Vec::with_capacity(16);
        obj.extend_from_slice(&8u32.to_le_bytes()); // ObjectHeader.size
        obj.extend_from_slice(&0u32.to_le_bytes()); // ObjectHeader.type = kTransport
        obj.extend_from_slice(&1u32.to_le_bytes()); // TransportHeader.destination_type = kNonBroker
        obj.extend_from_slice(&[0u8; 4]); // same_remote, peer_trusted, trusted_by_peer, reserved
        let pair = socketpair()?;
        let payload = messages::encode_accept_parcel_inline(
            0,
            0,
            &[],
            &[handle_type::BOXED_DRIVER_OBJECT],
            &[obj],
        );
        // The transport's remote endpoint travels with the parcel; the local
        // endpoint stays in this process (the oracle's `Broker` client).
        let mut payload = payload;
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, &[pair.b.into_raw_fd()])?;
        // Portal 0 is closed after the request; the broker's service portal
        // observes `RouteClosed(0, 1)` (the request was the only parcel).
        self.send_link_message(messages::encode_route_closed(0, 1))
    }

    /// Receive messages until `pred` matches; every non-matching message is
    /// dispatched to the routers. Returns the matched message and its fds.
    fn recv_until(
        &mut self,
        pred: impl Fn(&DecodedMessage) -> bool,
    ) -> Result<(DecodedMessage, Vec<OwnedFd>), RoutingError> {
        self.recv_until_on(LINK_ID_BROKER, pred)
    }

    /// Receive messages until `pred` matches on the given NodeLink; every
    /// non-matching message is dispatched to the routers with the source link
    /// id. Returns the matched message and its fds.
    fn recv_until_on(
        &mut self,
        link_id: u64,
        pred: impl Fn(&DecodedMessage) -> bool,
    ) -> Result<(DecodedMessage, Vec<OwnedFd>), RoutingError> {
        loop {
            let msg = self
                .channel_for(link_id)?
                .recv()?
                .ok_or(RoutingError::Unexpected("peer closed during exchange"))?;
            let decoded = messages::decode_message(&msg.payload, msg.fds.len())?;
            if pred(&decoded) {
                return Ok((decoded, msg.fds));
            }
            self.dispatch(decoded, msg.fds, link_id)?;
        }
    }

    /// Dispatch one message to the routers. `link_id` identifies the NodeLink
    /// the message arrived on (its sublink ids are scoped to that link).
    fn dispatch(
        &mut self,
        decoded: DecodedMessage,
        fds: Vec<OwnedFd>,
        link_id: u64,
    ) -> Result<(), RoutingError> {
        match decoded {
            DecodedMessage::ConnectFromBrokerToNonBroker(_) => {
                Err(RoutingError::Unexpected("Connect received after handshake"))
            }
            DecodedMessage::ConnectFromNonBrokerToBroker(_) => Err(RoutingError::Unexpected(
                "Connect reply received from broker",
            )),
            DecodedMessage::ReferNonBroker(_) => Err(RoutingError::Unsupported(
                MSG_ID_REFER_NON_BROKER,
                "native broker role not exercised by a sealed court",
            )),
            DecodedMessage::ConnectToReferredBroker(_) => Err(RoutingError::Unsupported(
                MSG_ID_CONNECT_TO_REFERRED_BROKER,
                "referred-node connect not exercised by a sealed court",
            )),
            DecodedMessage::ConnectToReferredNonBroker(_) => Err(RoutingError::Unsupported(
                MSG_ID_CONNECT_TO_REFERRED_NON_BROKER,
                "referral acceptance not exercised by a sealed court",
            )),
            DecodedMessage::NonBrokerReferralAccepted(_) => Err(RoutingError::Unsupported(
                MSG_ID_NON_BROKER_REFERRAL_ACCEPTED,
                "referral acceptance not exercised by a sealed court",
            )),
            DecodedMessage::NonBrokerReferralRejected(_) => Err(RoutingError::Unsupported(
                MSG_ID_NON_BROKER_REFERRAL_REJECTED,
                "referral rejection not exercised by a sealed court",
            )),
            DecodedMessage::ConnectFromBrokerToBroker(_) => Err(RoutingError::Unsupported(
                MSG_ID_CONNECT_FROM_BROKER_TO_BROKER,
                "broker-to-broker connect not exercised by a sealed court",
            )),
            DecodedMessage::RequestIntroduction(_) => Err(RoutingError::Unsupported(
                MSG_ID_REQUEST_INTRODUCTION,
                "native broker role not exercised by a sealed court",
            )),
            DecodedMessage::AcceptIntroduction(_) => Err(RoutingError::Unsupported(
                MSG_ID_ACCEPT_INTRODUCTION,
                "introduction not exercised by a sealed court",
            )),
            DecodedMessage::RejectIntroduction(_) => Err(RoutingError::Unsupported(
                MSG_ID_REJECT_INTRODUCTION,
                "introduction rejection not exercised by a sealed court",
            )),
            DecodedMessage::RequestIndirectIntroduction(_) => Err(RoutingError::Unsupported(
                MSG_ID_REQUEST_INDIRECT_INTRODUCTION,
                "indirect introduction not exercised by a sealed court",
            )),
            DecodedMessage::AddBlockBuffer(b) => {
                if b.buffer_index as usize >= fds.len() {
                    return Err(RoutingError::BadParcel("AddBlockBuffer index out of range"));
                }
                let fd = fds[b.buffer_index as usize].try_dup()?;
                self.memory_mut()?
                    .add_block_buffer(b.buffer_id, fd.into_raw_fd(), b.block_size)?;
                // Resolve any parcels deferred on this buffer
                // (`WaitForParcelFragmentToResolve` completes when the buffer
                // arrives).
                if let Some(queued) = self.pending_fragments.remove(&b.buffer_id) {
                    for (p, p_fds) in queued {
                        self.process_accept_parcel(p, p_fds, LINK_ID_BROKER)?;
                    }
                }
                Ok(())
            }
            DecodedMessage::AcceptParcel(p) => {
                self.process_accept_parcel(p, fds, link_id).map(|_| ())
            }
            DecodedMessage::AcceptParcelDriverObjects(_) => Err(RoutingError::Unsupported(
                MSG_ID_ACCEPT_PARCEL,
                "split parcels not exercised by the routing court",
            )),
            DecodedMessage::RouteClosed(rc) => {
                let Some(rid) = self.owners.get(&(link_id, rc.sublink)).copied() else {
                    // Deactivated sublink: the official `GetRouter` returns
                    // null and the message is silently ignored.
                    return Ok(());
                };
                self.router_route_closed(rid, rc.sublink, rc.sequence_length)
            }
            DecodedMessage::RouteDisconnected(rd) => {
                let Some(rid) = self.owners.get(&(link_id, rd.sublink)).copied() else {
                    return Ok(());
                };
                self.router_disconnected(rid)
            }
            DecodedMessage::BypassPeerWithLink(b) => self.on_bypass_peer_with_link(b, link_id),
            DecodedMessage::StopProxying(s) => {
                let Some(rid) = self.owners.get(&(link_id, s.sublink)).copied() else {
                    return Ok(());
                };
                self.router_stop_proxying(
                    rid,
                    s.inbound_sequence_length,
                    s.outbound_sequence_length,
                )
            }
            DecodedMessage::StopProxyingToLocalPeer(s) => {
                // Routed to the router owning the sublink, exactly like the
                // official `NodeLink::OnStopProxyingToLocalPeer`.
                let Some(rid) = self.owners.get(&(link_id, s.sublink)).copied() else {
                    return Ok(());
                };
                self.router_stop_proxying_to_local_peer(rid, s.outbound_sequence_length)
            }
            DecodedMessage::FlushRouter(f) => {
                if let Some(&rid) = self.owners.get(&(link_id, f.sublink)) {
                    // The official `OnFlushRouter` forces a proxy-bypass
                    // attempt on the target router.
                    self.router_flush(rid, true)?;
                }
                Ok(())
            }
            DecodedMessage::RequestMemory(r) => self.on_request_memory(r.size),
            DecodedMessage::ProvideMemory(r) => {
                // `ProvideMemory` carries exactly one driver object (the
                // buffer) at index 0; the descriptor travels with the message.
                if fds.len() != 1 {
                    return Err(RoutingError::BadParcel(
                        "ProvideMemory without exactly one buffer descriptor",
                    ));
                }
                // SAFETY of the length: checked above.
                let fd = match fds.into_iter().next() {
                    Some(fd) => fd,
                    None => {
                        return Err(RoutingError::BadParcel(
                            "ProvideMemory without a buffer descriptor",
                        ));
                    }
                };
                self.on_provide_memory(r.size, fd)
            }
            DecodedMessage::BypassPeer(_) => Err(RoutingError::Unsupported(
                MSG_ID_BYPASS_PEER,
                "inbound BypassPeer not exercised by the routing court",
            )),
            DecodedMessage::AcceptBypassLink(_) => Err(RoutingError::Unsupported(
                MSG_ID_ACCEPT_BYPASS_LINK,
                "inbound AcceptBypassLink not exercised by the routing court",
            )),
            DecodedMessage::ProxyWillStop(_) => Err(RoutingError::Unsupported(
                MSG_ID_PROXY_WILL_STOP,
                "inbound ProxyWillStop not exercised by the routing court",
            )),
            DecodedMessage::Unknown(id) => Err(RoutingError::Unsupported(
                id,
                "message type not decoded by the interop layer",
            )),
        }
    }

    /// The payload of an AcceptParcel (fragment or inline).
    fn parcel_data(&self, p: &AcceptParcel) -> Result<Vec<u8>, RoutingError> {
        if p.parcel_fragment.is_null() {
            Ok(p.parcel_data.clone())
        } else {
            Ok(self.memory()?.read_parcel_fragment(p.parcel_fragment)?)
        }
    }

    /// Assemble an AcceptParcel (data + deserialized portals), route it to its
    /// router, and flush. Returns the payload and the new portal identities.
    /// `link_id` is the NodeLink the parcel arrived on (its sublink ids are
    /// scoped to that link).
    fn process_accept_parcel(
        &mut self,
        p: AcceptParcel,
        fds: Vec<OwnedFd>,
        link_id: u64,
    ) -> Result<ProcessedParcel, RoutingError> {
        if p.num_subparcels == 0 || p.subparcel_index >= p.num_subparcels || p.num_subparcels > 16 {
            return Err(RoutingError::BadParcel("invalid subparcel bounds"));
        }
        if p.num_subparcels > 1 {
            return Err(RoutingError::Unsupported(
                MSG_ID_ACCEPT_PARCEL,
                "multi-subparcel parcels not exercised by the routing court",
            ));
        }
        let payload = match self.parcel_data(&p) {
            Ok(payload) => payload,
            Err(RoutingError::Memory(crate::ipcz::link_memory::LinkMemoryError::UnknownBuffer)) => {
                // The parcel's data lives in a buffer we have not received yet;
                // defer acceptance until the AddBlockBuffer arrives
                // (`NodeLink::WaitForParcelFragmentToResolve`).
                self.pending_fragments
                    .entry(p.parcel_fragment.buffer_id)
                    .or_default()
                    .push_back((p, fds));
                return Ok(ProcessedParcel {
                    payload: Vec::new(),
                    identities: Vec::new(),
                });
            }
            Err(e) => return Err(e),
        };
        let identities = self.deserialize_portals(&p, link_id)?;
        let mut identities_iter = identities.iter();
        let mut objects: Vec<Object> = Vec::new();
        let mut driver_refs = p.driver_objects.iter();
        let mut is_split = false;
        for &ht in &p.handle_types {
            match ht {
                handle_type::PORTAL => {
                    let rid = identities_iter
                        .next()
                        .copied()
                        .ok_or(RoutingError::BadParcel("missing router descriptor"))?;
                    objects.push(Object::Router(rid));
                }
                handle_type::BOXED_DRIVER_OBJECT => {
                    let obj = driver_refs
                        .next()
                        .ok_or(RoutingError::BadParcel("missing driver object"))?;
                    let _ = obj;
                    let _ = &fds;
                    return Err(RoutingError::Unsupported(
                        MSG_ID_ACCEPT_PARCEL,
                        "driver objects not exercised by the routing court",
                    ));
                }
                handle_type::RELAYED_BOXED_DRIVER_OBJECT => {
                    is_split = true;
                }
                handle_type::BOXED_SUBPARCEL => {
                    return Err(RoutingError::Unsupported(
                        MSG_ID_ACCEPT_PARCEL,
                        "boxed subparcels not exercised by the routing court",
                    ));
                }
                other => {
                    // Any value outside the four known handle types is
                    // malformed input (attacker-controlled); classify without
                    // trusting the value.
                    return Err(RoutingError::BadParcel(match other {
                        _ => "unknown handle type",
                    }));
                }
            }
        }
        if driver_refs.next().is_some() {
            return Err(RoutingError::BadParcel("unclaimed driver objects"));
        }
        if is_split {
            return Err(RoutingError::Unsupported(
                MSG_ID_ACCEPT_PARCEL,
                "split parcels not exercised by the routing court",
            ));
        }

        let rid = match self.owners.get(&(link_id, p.sublink)) {
            Some(&rid) => rid,
            None => {
                self.early_parcels
                    .entry((link_id, p.sublink))
                    .or_default()
                    .push_back(p);
                return Ok(ProcessedParcel {
                    payload,
                    identities,
                });
            }
        };
        let is_outward = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("parcel for unknown router"))?;
            let on_outward = router
                .outward
                .primary
                .as_ref()
                .is_some_and(|l| l.link_id == link_id && l.sublink == p.sublink)
                || router
                    .outward
                    .decaying_link()
                    .is_some_and(|l| l.link_id == link_id && l.sublink == p.sublink);
            if !on_outward {
                let on_inward = router.inward.as_ref().is_some_and(|e| {
                    e.primary
                        .as_ref()
                        .is_some_and(|l| l.link_id == link_id && l.sublink == p.sublink)
                        || e.decaying_link()
                            .is_some_and(|l| l.link_id == link_id && l.sublink == p.sublink)
                });
                if !on_inward {
                    return Err(RoutingError::BadParcel("parcel for unbound sublink"));
                }
            }
            on_outward
        };
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("parcel for unknown router"))?;
            let parcel = Parcel {
                sequence_number: p.sequence_number,
                data: payload.clone(),
                objects,
                // Received fragment data is copied out at delivery; the
                // fragment itself is not retained (the receive-side free is a
                // documented boundary until a court exercises it).
                fragment: None,
            };
            let accepted = if is_outward {
                router.accept_inbound(parcel)
            } else {
                router.accept_outbound(parcel)
            };
            if !accepted {
                return Err(RoutingError::BadParcel("route sequence gap or duplicate"));
            }
        }
        self.router_flush(rid, false)?;
        Ok(ProcessedParcel {
            payload,
            identities,
        })
    }

    /// `Router::Deserialize`: create a new router from a descriptor received
    /// over the NodeLink identified by `link_id`. Returns the new router's
    /// identity sublink (scoped to that link).
    fn router_deserialize(
        &mut self,
        d: RouterDescriptor,
        link_id: u64,
    ) -> Result<u64, RoutingError> {
        let new_decaying_sublink = if d.proxy_already_bypassed {
            Some(d.new_decaying_sublink)
        } else {
            None
        };
        if d.new_sublink == BOOTSTRAP_SUBLINK
            || (new_decaying_sublink == Some(d.new_sublink))
            || (new_decaying_sublink == Some(BOOTSTRAP_SUBLINK))
        {
            return Err(RoutingError::BadParcel("invalid sublink ids in descriptor"));
        }
        // Resolve the link state fragment before touching the router.
        let link_state = if let Some(_decaying) = new_decaying_sublink {
            if d.new_link_state_fragment.is_null() {
                return Err(RoutingError::BadParcel("central link without link state"));
            }
            self.memory_for(link_id)?
                .fragment(d.new_link_state_fragment)?;
            // `AdoptFragmentRefIfValid` does NOT increment: the adoption takes
            // the sender's released reference (the sender's `FragmentRef`
            // `release()`d it into the descriptor). The shared ref count is
            // the sender's link ref + this side's adopted ref.
            Some(d.new_link_state_fragment)
        } else {
            if !d.new_link_state_fragment.is_null() {
                return Err(RoutingError::BadParcel("peripheral link with link state"));
            }
            None
        };

        let mut router = Router::bare();
        router
            .outbound
            .reset_sequence(d.next_outgoing_sequence_number);
        router
            .inbound
            .reset_sequence(d.next_incoming_sequence_number);
        if d.peer_closed {
            router.peer_closed = true;
            if !router
                .inbound
                .set_final_sequence_length(d.closed_peer_sequence_length)
            {
                return Err(RoutingError::BadParcel(
                    "invalid closed peer sequence length",
                ));
            }
        }

        if let Some(decaying_sublink) = new_decaying_sublink {
            // The decaying peripheral outward link forwards parcels already
            // queued or in flight on the sender's node.
            router.outward.set_primary_link(Link::remote(
                link_id,
                decaying_sublink,
                LinkKind::PeripheralOutward,
                LinkSide::B,
                None,
            ));
            router.outward.begin_primary_link_decay();
            router
                .outward
                .set_length_to_decaying_link(router.outbound.current_sequence_number());
            router.outward.set_length_from_decaying_link(
                if d.decaying_incoming_sequence_length > 0 {
                    d.decaying_incoming_sequence_length
                } else {
                    d.next_incoming_sequence_number
                },
            );
            router.outward.set_primary_link(Link::remote(
                link_id,
                d.new_sublink,
                LinkKind::Central,
                LinkSide::B,
                link_state,
            ));
        } else {
            router.outward.set_primary_link(Link::remote(
                link_id,
                d.new_sublink,
                LinkKind::PeripheralOutward,
                LinkSide::B,
                None,
            ));
        }

        self.owners.insert((link_id, d.new_sublink), d.new_sublink);
        if let Some(decaying) = new_decaying_sublink {
            self.owners.insert((link_id, decaying), d.new_sublink);
        }
        self.routers.insert(d.new_sublink, router);

        // Accept early parcels for the new sublink AND its decaying sublink
        // (parcels for the decaying route can arrive before the descriptor is
        // processed, e.g. when the sender's IO thread flushes asynchronously).
        let mut early: Vec<AcceptParcel> =
            match self.early_parcels.remove(&(link_id, d.new_sublink)) {
                Some(q) => q.into_iter().collect(),
                None => Vec::new(),
            };
        if let Some(decaying) = new_decaying_sublink {
            if let Some(q) = self.early_parcels.remove(&(link_id, decaying)) {
                early.extend(q.into_iter());
            }
        }
        for p in early {
            self.process_accept_parcel(p, Vec::new(), link_id)?;
        }

        // If the source router rolled peer-bypass details into the descriptor,
        // begin bypassing the proxy now.
        if d.proxy_peer_node_name.is_valid() {
            let rid = d.new_sublink;
            let target_node = d.proxy_peer_node_name;
            let target_sublink = d.proxy_peer_sublink;
            self.router_bypass_peer(rid, link_id, target_node, target_sublink)?;
        }

        self.router_flush(d.new_sublink, false)?;
        Ok(d.new_sublink)
    }

    /// `Router::BypassPeer` (the descriptor's immediate-bypass path): the
    /// requestor is our peripheral outward peer over `requestor_link_id`; its
    /// outward peer lives on `bypass_target_node` via `bypass_target_sublink`.
    ///
    /// When the bypass target is on another node and we have a link to it, the
    /// bypass is completed by `BypassPeerWithNewRemoteLink`: we allocate a
    /// fresh `RouterLinkState` from the target link's memory, begin decaying
    /// our outward requestor link, create a new central link to the target
    /// node, and send `AcceptBypassLink` over the target link (the outbound
    /// id-31 path sealed by the multi-node court).
    fn router_bypass_peer(
        &mut self,
        rid: u64,
        requestor_link_id: u64,
        bypass_target_node: NodeName,
        bypass_target_sublink: u64,
    ) -> Result<(), RoutingError> {
        // Validate that the source of this request is our peripheral outward
        // peer (`outward_link != &requestor` in the official).
        let requestor_sublink = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("bypass for unknown router"))?;
            let Some(primary) = router.outward.primary.as_ref() else {
                // This Router may have been disconnected already; silently
                // ignore the request (the official returns true).
                return Ok(());
            };
            if primary.link_id != requestor_link_id {
                return Err(RoutingError::BadParcel(
                    "rejecting BypassPeer from a non-outward peer",
                ));
            }
            primary.sublink
        };

        if bypass_target_node != self.local_name {
            // The proxy's outward peer lives on another node: we need a link
            // to it (`Node::GetLink`).
            let target_link_id = self.link_id_for_node(bypass_target_node)?;
            return self.bypass_peer_with_new_remote_link(
                rid,
                requestor_link_id,
                requestor_sublink,
                bypass_target_sublink,
                target_link_id,
            );
        }
        Err(RoutingError::Unsupported(
            MSG_ID_BYPASS_PEER,
            "BypassPeerWithNewLocalLink not exercised by a sealed court",
        ))
    }

    /// The NodeLink id of the link to the given node (`Node::GetLink`). Only
    /// the sealed courts' links exist (broker link + direct peer link).
    fn link_id_for_node(&self, name: NodeName) -> Result<u64, RoutingError> {
        if name == self.broker_name {
            return Ok(LINK_ID_BROKER);
        }
        if self.referrer_name.is_valid() && name == self.referrer_name {
            return Ok(LINK_ID_DIRECT);
        }
        Err(RoutingError::Unsupported(
            MSG_ID_BYPASS_PEER,
            "EstablishLink (unknown node) not exercised by a sealed court",
        ))
    }

    /// `Router::BypassPeerWithNewRemoteLink`: replace our outward link to the
    /// requestor's proxy with a new central link to the bypass target over
    /// `target_link_id`, transmitting `AcceptBypassLink` first (the new link
    /// is only adopted after the message goes out, so the peer can route it).
    fn bypass_peer_with_new_remote_link(
        &mut self,
        rid: u64,
        requestor_link_id: u64,
        requestor_sublink: u64,
        bypass_target_sublink: u64,
        target_link_id: u64,
    ) -> Result<(), RoutingError> {
        let requestor_node = self.remote_name_for(requestor_link_id)?;
        // `TryAllocateRouterLinkState` from the target link's memory; the
        // sealed courts never exhaust here (the official would retry
        // asynchronously on failure).
        let Some(new_link_state) = self
            .memory_mut_for(target_link_id)?
            .try_allocate_link_state()?
        else {
            return Ok(());
        };
        // The new central link holds a copy of the FragmentRef.
        self.memory_for(target_link_id)?
            .add_link_state_ref(new_link_state)?;
        let new_sublink = self.memory_for(target_link_id)?.allocate_sublink_ids(1)?;

        // Begin decaying our outward (requestor) link.
        let length_to_decaying_link = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("bypass for unknown router"))?;
            router.outbound.current_sequence_number()
        };
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("bypass for unknown router"))?;
            if router.disconnected {
                return Ok(());
            }
            if !router.outward.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay the outward link"));
            }
            router
                .outward
                .set_length_to_decaying_link(length_to_decaying_link);
        }

        // `NodeLink::AcceptBypassLink`: inform the bypass target that it can
        // bypass the proxy, providing the new central sublink + link state.
        let msg = messages::encode_accept_bypass_link(
            requestor_node,
            bypass_target_sublink,
            length_to_decaying_link,
            new_sublink,
            new_link_state,
        );
        self.send_link_message_on(target_link_id, msg)?;

        // Adopt the new central link (the official `SetOutwardLink`, which
        // marks side A stable when the edges are stable and flushes).
        let new_link = Link::remote(
            target_link_id,
            new_sublink,
            LinkKind::Central,
            LinkSide::A,
            Some(new_link_state),
        );
        self.set_outward_link(rid, &new_link)?;
        self.router_flush(rid, true)?;
        Ok(())
    }

    /// `OpenPortals`: create a local pair of terminal routers linked by a
    /// local central link, born stable (`LocalRouterLink::CreatePair` with
    /// `InitialState::kStable`). Returns `(a, b)` router identities.
    fn open_portals(&mut self) -> Result<(u64, u64), RoutingError> {
        let a = self.next_rid;
        self.next_rid += 1;
        let b = self.next_rid;
        self.next_rid += 1;
        let (link_a, link_b) = Link::local_pair(LinkKind::Central, LinkSide::A, a, b, true);
        self.routers.insert(a, Router::new_terminal(link_a));
        self.routers.insert(b, Router::new_terminal(link_b));
        Ok((a, b))
    }

    /// `Router::SerializeNewRouter`: serialize this router so a new router can
    /// back its portal on the remote node; this router stays behind as a proxy.
    ///
    /// When the router has a local peer (a locally created pair), the
    /// WithLocalPeer path is preferred: a new central link (with a fresh
    /// `RouterLinkState` allocated from the shared pool) plus a decaying
    /// peripheral link replace the local pair. If no link state can be
    /// allocated, the unconditional `TryAllocateRouterLinkState` lobby fires
    /// (a `RequestMemory` to the broker) and the transfer falls back to the
    /// plain proxy path with the outward link unlocked (the official
    /// `SerializeNewRouterAndConfigureProxy` behavior for a local outward
    /// link).
    fn serialize_router(
        &mut self,
        rid: u64,
        link_id: u64,
    ) -> Result<(u64, RouterDescriptor), RoutingError> {
        // Snapshot the outward primary and its local peer (`Router::SerializeNewRouter`
        // reads these under the lock).
        let (outward_primary, local_peer) = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("serialize for unknown router"))?;
            if router.inward.is_some() {
                return Err(RoutingError::BadParcel("serializing a proxy"));
            }
            let primary = router.outward.primary.clone();
            let local_peer = primary.as_ref().and_then(|l| l.local_peer);
            (primary, local_peer)
        };
        let Some(primary) = outward_primary else {
            return Err(RoutingError::BadParcel(
                "serialize for router with no outward link",
            ));
        };
        // Lock the central link for proxy bypass, if possible. The lock also
        // records the allowed bypass request source (the remote peer on the
        // locked link), matching `RemoteRouterLink::TryLockForBypass`.
        let source = self.broker_name.low;
        let locked = self.try_lock_link_for_bypass(&primary, source)?;

        // The WithLocalPeer path: the router's peer is on this node.
        if let Some(local_peer) = local_peer {
            if locked {
                if let Some(descriptor) =
                    self.serialize_router_with_local_peer(rid, local_peer, link_id)?
                {
                    return Ok((rid, descriptor));
                }
                // No link state available (pool exhausted; the lobby fired).
                // The official path unlocks the (local) outward link and falls
                // back to the plain proxy path below with no proxy-bypass
                // fields.
                self.unlock_link_for_bypass(&primary)?;
            }
        }

        let initiate_proxy_bypass = locked;
        let new_sublink = self.memory()?.allocate_sublink_ids(1)?;
        let mut descriptor = RouterDescriptor {
            new_sublink,
            new_link_state_fragment: FragmentDescriptor::default(),
            proxy_already_bypassed: false,
            ..RouterDescriptor::default()
        };
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("serialize for unknown router"))?;
            descriptor.next_outgoing_sequence_number = router.outbound.current_sequence_number();
            descriptor.next_incoming_sequence_number = router.inbound.current_sequence_number();

            router.inward = Some(Edge::default());
            if router.peer_closed {
                descriptor.peer_closed = true;
                descriptor.closed_peer_sequence_length =
                    router.inbound.final_sequence_length().unwrap_or(0);
                if let Some(inward) = router.inward.as_mut() {
                    inward.begin_primary_link_decay();
                    inward.set_length_to_decaying_link(descriptor.closed_peer_sequence_length);
                    inward.set_length_from_decaying_link(router.outbound.current_sequence_number());
                }
            } else if initiate_proxy_bypass {
                // Only a REMOTE outward link can roll bypass details into the
                // descriptor; a local outward link was already unlocked above.
                let primary = router
                    .outward
                    .primary
                    .clone()
                    .ok_or(RoutingError::BadParcel("no outward primary"))?;
                if !primary.is_local() {
                    descriptor.proxy_peer_node_name = self.broker_name;
                    descriptor.proxy_peer_sublink = primary.sublink;
                    if let Some(inward) = router.inward.as_mut() {
                        inward.begin_primary_link_decay();
                    }
                    router.outward.begin_primary_link_decay();
                }
            }
        }
        // Register the new peripheral inward link on the NodeLink; the router
        // adopts it in begin_proxying after the descriptor is transmitted.
        self.owners.insert((link_id, new_sublink), rid);
        Ok((rid, descriptor))
    }

    /// `Router::SerializeNewRouterWithLocalPeer`: replace the local pair with a
    /// new central link (to the remote node, shared `RouterLinkState`) plus a
    /// decaying peripheral link. The local peer's outward edge is released here
    /// (it adopts the new central link in `begin_proxying`); this router's
    /// outward edge is released there too, and its inward edge (created below
    /// with a deferred decay) adopts the decaying link.
    ///
    /// Returns `None` (after firing the `TryAllocateRouterLinkState` lobby)
    /// when no link state can be allocated; the caller falls back to the plain
    /// proxy path.
    fn serialize_router_with_local_peer(
        &mut self,
        rid: u64,
        local_peer: u64,
        link_id: u64,
    ) -> Result<Option<RouterDescriptor>, RoutingError> {
        // `TryAllocateRouterLinkState`: on failure, unconditionally lobby for
        // more capacity (the `RequestMemory`/`ProvideMemory` round trip) and
        // fall back.
        let Some(new_link_state) = self.memory_mut()?.try_allocate_link_state()? else {
            self.request_block_capacity(crate::ipcz::link_memory::ROUTER_LINK_STATE_SIZE as u32)?;
            return Ok(None);
        };
        // The new central link holds a copy of the FragmentRef
        // (`AddRemoteRouterLink` copies; the descriptor carries the released
        // ref, adopted by the peer without increment).
        self.memory()?.add_link_state_ref(new_link_state)?;
        let proxy_inbound_sequence_length = self
            .routers
            .get(&local_peer)
            .ok_or(RoutingError::BadParcel("serialize with unknown local peer"))?
            .outbound
            .current_sequence_number();
        // The local peer no longer needs its link to us; it will adopt the new
        // central link in `begin_proxying` after the descriptor is transmitted.
        {
            let peer = self
                .routers
                .get_mut(&local_peer)
                .ok_or(RoutingError::BadParcel("serialize with unknown local peer"))?;
            if peer.outward.release_primary_link().is_none() {
                return Err(RoutingError::BadParcel("local peer has no outward link"));
            }
        }
        // A primary sublink for the new central link plus an adjacent decaying
        // peripheral sublink.
        let new_sublink = self.memory()?.allocate_sublink_ids(2)?;
        let decaying_sublink = new_sublink + 1;
        // Register the tentative routes on the NodeLink (adopted in
        // `begin_proxying` after transmission).
        self.owners.insert((link_id, new_sublink), local_peer);
        self.owners.insert((link_id, decaying_sublink), rid);

        let mut descriptor = RouterDescriptor {
            new_sublink,
            new_link_state_fragment: new_link_state,
            new_decaying_sublink: decaying_sublink,
            proxy_already_bypassed: true,
            ..RouterDescriptor::default()
        };
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("serialize for unknown router"))?;
            descriptor.next_outgoing_sequence_number = router.outbound.current_sequence_number();
            descriptor.next_incoming_sequence_number = router.inbound.current_sequence_number();
            descriptor.decaying_incoming_sequence_length = proxy_inbound_sequence_length;
            if let Some(final_len) = router.inbound.final_sequence_length() {
                descriptor.peer_closed = true;
                descriptor.closed_peer_sequence_length = final_len;
            }
            // An inward edge that will immediately begin decaying once it has
            // a link (established in `begin_proxying`).
            router.inward = Some(Edge::default());
            let inward = router
                .inward
                .as_mut()
                .ok_or(RoutingError::BadParcel("inward edge missing"))?;
            inward.begin_primary_link_decay();
            inward.set_length_to_decaying_link(proxy_inbound_sequence_length);
            inward.set_length_from_decaying_link(descriptor.next_outgoing_sequence_number);
        }
        Ok(Some(descriptor))
    }

    /// `Router::BeginProxyingToNewRouter`: after the descriptor was
    /// transmitted, adopt the new links. Two cases mirror the official code:
    ///
    /// * `proxy_already_bypassed` (the WithLocalPeer serialization): this
    ///   router's outward edge releases its old local link (revealing the local
    ///   peer), the inward edge adopts the decaying peripheral link, and the
    ///   local peer adopts the new central link (`SetOutwardLink`, which marks
    ///   side A stable on the shared state).
    /// * plain proxy: the inward edge adopts the peripheral inward link.
    fn begin_proxying(
        &mut self,
        rid: u64,
        d: &RouterDescriptor,
        link_id: u64,
    ) -> Result<(), RoutingError> {
        let new_sublink = d.new_sublink;
        if d.proxy_already_bypassed {
            // Release this router's outward link (the old local link to its
            // peer) and identify the local peer.
            let local_peer = {
                let router = self
                    .routers
                    .get_mut(&rid)
                    .ok_or(RoutingError::BadParcel("begin_proxying for unknown router"))?;
                router
                    .outward
                    .release_primary_link()
                    .and_then(|l| l.local_peer)
            };
            let Some(local_peer) = local_peer else {
                return Err(RoutingError::BadParcel(
                    "with-local-peer transfer without a local peer",
                ));
            };
            {
                let router = self
                    .routers
                    .get_mut(&rid)
                    .ok_or(RoutingError::BadParcel("begin_proxying for unknown router"))?;
                if !router.disconnected {
                    // Adopt the decaying peripheral link as the inward edge;
                    // the edge was marked for deferred decay at serialization,
                    // so the link immediately begins decaying.
                    let Some(inward) = &mut router.inward else {
                        return Err(RoutingError::BadParcel(
                            "no inward edge after with-local-peer serialization",
                        ));
                    };
                    inward.set_primary_link(Link::remote(
                        link_id,
                        d.new_decaying_sublink,
                        LinkKind::PeripheralInward,
                        LinkSide::A,
                        None,
                    ));
                }
            }
            // The local peer's new outward link is the central link; adopt it
            // and mark side A stable (`Router::SetOutwardLink`).
            let central = Link::remote(
                link_id,
                new_sublink,
                LinkKind::Central,
                LinkSide::A,
                Some(d.new_link_state_fragment),
            );
            self.set_outward_link(local_peer, &central)?;
            self.router_flush(rid, true)?;
            self.router_flush(local_peer, true)?;
            return Ok(());
        }

        let mark_stable = {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("begin_proxying for unknown router"))?;
            let Some(inward) = &mut router.inward else {
                return Err(RoutingError::BadParcel(
                    "begin_proxying on a terminal router",
                ));
            };
            if router.outbound.final_sequence_length().is_none() && !router.disconnected {
                inward.set_primary_link(Link::remote(
                    link_id,
                    new_sublink,
                    LinkKind::PeripheralInward,
                    LinkSide::A,
                    None,
                ));
            }
            if router.outward.primary.is_some() && router.outward.is_stable() && inward.is_stable()
            {
                router.outward.primary.as_ref().and_then(|l| l.link_state)
            } else {
                None
            }
        };
        if let Some(state) = mark_stable {
            // Side A (this node allocates the link as side A).
            self.memory()?.set_side_stable(state, true)?;
        }
        if self
            .routers
            .get(&rid)
            .map(|r| r.outbound.final_sequence_length().is_none() && !r.disconnected)
            .unwrap_or(false)
        {
            self.router_flush(rid, false)?;
        }
        Ok(())
    }

    /// `RouterLink::MarkSideStable` on a link: remote links OR the stable bit
    /// into the shared fragment; local links into the shared in-process state.
    fn mark_side_stable_on_link(&mut self, link: &Link) -> Result<(), RoutingError> {
        if let Some(desc) = link.link_state {
            self.memory()?.set_side_stable(desc, link.side.is_a())?;
        }
        if let Some(state) = &link.local_state {
            state.borrow_mut().set_side_stable(link.side.is_a());
        }
        Ok(())
    }

    /// `RouterLink::TryLockForClosure` on a link.
    fn try_lock_link_for_closure(&mut self, link: &Link) -> Result<bool, RoutingError> {
        if let Some(desc) = link.link_state {
            return Ok(self.memory()?.try_lock_link_state(desc, link.side.is_a())?);
        }
        if let Some(state) = &link.local_state {
            return Ok(state.borrow_mut().try_lock(link.side.is_a()));
        }
        Ok(false)
    }

    /// `RouterLink::TryLockForBypass` on a link: lock the link state for a
    /// bypass and record the allowed bypass request source.
    fn try_lock_link_for_bypass(&mut self, link: &Link, source: u64) -> Result<bool, RoutingError> {
        if let Some(desc) = link.link_state {
            if !self.memory()?.try_lock_link_state(desc, link.side.is_a())? {
                return Ok(false);
            }
            self.memory_mut()?
                .write_allowed_bypass_source(desc, source)?;
            return Ok(true);
        }
        if let Some(state) = &link.local_state {
            let mut st = state.borrow_mut();
            if !st.try_lock(link.side.is_a()) {
                return Ok(false);
            }
            st.allowed_bypass_source = source;
            return Ok(true);
        }
        Ok(false)
    }

    /// `RouterLink::Unlock` on a link locked for bypass.
    fn unlock_link_for_bypass(&mut self, link: &Link) -> Result<(), RoutingError> {
        if let Some(desc) = link.link_state {
            self.memory()?.unlock_link_state(desc, link.side.is_a())?;
        }
        if let Some(state) = &link.local_state {
            state.borrow_mut().unlock(link.side.is_a());
        }
        Ok(())
    }

    /// `RouterLink::FlushOtherSideIfWaiting` on a link: if the peer set its
    /// waiting bit on the link state, wake it (a `FlushRouter` message, or a
    /// forced flush of the local peer router).
    fn flush_other_side_if_waiting(&mut self, link: &Link) -> Result<(), RoutingError> {
        if let Some(desc) = link.link_state {
            if self.memory()?.reset_waiting_bit(desc, !link.side.is_a())? {
                self.send_link_message(messages::encode_flush_router(link.sublink))?;
            }
            return Ok(());
        }
        if let Some(state) = &link.local_state {
            if state.borrow_mut().reset_waiting_bit(!link.side.is_a()) {
                if let Some(peer) = link.local_peer {
                    self.router_flush(peer, true)?;
                }
            }
            return Ok(());
        }
        Ok(())
    }

    /// Deliver route closure over a released link: locally to the peer router
    /// (`AcceptRouteClosureFrom` with the link's type), or over the wire as
    /// `RouteClosed`.
    fn deliver_route_closed(&mut self, link: &Link, len: u64) -> Result<(), RoutingError> {
        if let Some(peer) = link.local_peer {
            self.router_route_closed_local(peer, link.kind, len)
        } else {
            self.send_link_message(messages::encode_route_closed(link.sublink, len))
        }
    }

    /// `Router::SetOutwardLink`: adopt a new outward link, marking the side
    /// stable when the link is central and both edges are stable (or the
    /// router is disconnected, in which case the link is dropped).
    fn set_outward_link(&mut self, rid: u64, link: &Link) -> Result<(), RoutingError> {
        let mark = {
            let router = self.routers.get(&rid).ok_or(RoutingError::BadParcel(
                "set outward link for unknown router",
            ))?;
            link.kind.is_central()
                && router.outward.is_stable()
                && router.inward.as_ref().is_none_or(Edge::is_stable)
        };
        if mark {
            self.mark_side_stable_on_link(link)?;
        }
        {
            let router = self.routers.get_mut(&rid).ok_or(RoutingError::BadParcel(
                "set outward link for unknown router",
            ))?;
            if !router.disconnected {
                router.outward.set_primary_link(link.clone());
            }
        }
        Ok(())
    }

    /// `Router::MaybeStartBridgeBypass`: collapse a bridge chain when both
    /// bridge routers have stable outward links, replacing the chain with a
    /// single central link. All three cases (no local peers, one local peer,
    /// two local peers) mirror the pinned `router.cc`.
    fn maybe_start_bridge_bypass(&mut self, rid: u64) -> Result<(), RoutingError> {
        // Snapshot: this router's bridge peer (the router on the other side of
        // the bridge edge).
        let second_bridge = {
            let Some(router) = self.routers.get(&rid) else {
                return Ok(());
            };
            if router.bridge.is_none() || !router.bridge_stable() {
                return Ok(());
            }
            router.bridge.as_ref().and_then(Edge::get_local_peer)
        };
        let Some(second_bridge) = second_bridge else {
            return Ok(());
        };

        // Snapshot both bridge routers' outward primary links.
        let first_link = self
            .routers
            .get(&rid)
            .and_then(|r| r.outward.primary.clone());
        let second_link = self
            .routers
            .get(&second_bridge)
            .and_then(|r| r.outward.primary.clone());
        let (Some(first_link), Some(second_link)) = (first_link, second_link) else {
            return Ok(());
        };

        // The bypass request source for each link is the peer node of the
        // other bridge router's outward link (invalid when that peer is
        // local).
        let first_local_peer = first_link.local_peer;
        let second_local_peer = second_link.local_peer;
        let first_peer_node = if first_link.is_local() {
            0
        } else {
            self.broker_name.low
        };
        let second_peer_node = if second_link.is_local() {
            0
        } else {
            self.broker_name.low
        };

        // Lock both outward links for bypass (`TryLockForBypass`). On failure
        // of the second, unlock the first and give up.
        if !self.try_lock_link_for_bypass(&first_link, second_peer_node)? {
            return Ok(());
        }
        if !self.try_lock_link_for_bypass(&second_link, first_peer_node)? {
            self.unlock_link_for_bypass(&first_link)?;
            return Ok(());
        }

        // Case 1: neither bridge router's outward peer is local. Bypass both
        // bridge routers with a new central link directly to the other bridge
        // router's outward peer.
        if first_local_peer.is_none() && second_local_peer.is_none() {
            for r in [rid, second_bridge] {
                if let Some(router) = self.routers.get_mut(&r) {
                    if !router.outward.begin_primary_link_decay() {
                        return Err(RoutingError::BadParcel("failed to decay outward edge"));
                    }
                    if let Some(edge) = &mut router.bridge {
                        if !edge.begin_primary_link_decay() {
                            return Err(RoutingError::BadParcel("failed to decay bridge edge"));
                        }
                    }
                }
            }
            // The first link is remote (both are); ask its peer to bypass the
            // proxy with a direct link to the second router's outward peer.
            let target_sublink = second_link.sublink;
            self.send_link_message(messages::encode_bypass_peer(
                first_link.sublink,
                self.broker_name,
                target_sublink,
            ))?;
            return Ok(());
        }

        // Case 2: only one bridge router has a local outward peer. The bridge
        // router whose outward peer is local initiates the bypass.
        if second_local_peer.is_none() {
            let Some(link_state) = self.memory_mut()?.try_allocate_link_state()? else {
                // The 64-byte pool is exhausted: lobby for more capacity and
                // defer the bypass (the official retries asynchronously; no
                // sealed court exhausts this pool — documented boundary).
                self.request_block_capacity(
                    crate::ipcz::link_memory::ROUTER_LINK_STATE_SIZE as u32,
                )?;
                return Ok(());
            };
            // The official `AddRemoteRouterLink(new_sublink, link_state, ...)`
            // copies the `FragmentRef` (AddRef): the shared count becomes the
            // link's ref plus the ref transferred to the remote peer in the
            // `BypassPeerWithLink` descriptor.
            self.memory()?.add_link_state_ref(link_state)?;
            return self.start_bridge_bypass_from_local_peer(rid, link_state);
        } else if first_local_peer.is_none() {
            let Some(link_state) = self.memory_mut()?.try_allocate_link_state()? else {
                self.request_block_capacity(
                    crate::ipcz::link_memory::ROUTER_LINK_STATE_SIZE as u32,
                )?;
                return Ok(());
            };
            self.memory()?.add_link_state_ref(link_state)?;
            return self.start_bridge_bypass_from_local_peer(second_bridge, link_state);
        }

        // Case 3: both bridge routers' outward peers are local. All four
        // routers live on this node; bypass synchronously with a new local
        // central link between the two outward peers.
        let (Some(first_local), Some(second_local)) = (first_local_peer, second_local_peer) else {
            return Err(RoutingError::BadParcel(
                "local peer missing in case-3 bypass",
            ));
        };
        let length_from_first_peer = self
            .routers
            .get(&first_local)
            .map(|r| r.outbound_length())
            .unwrap_or(0);
        let length_from_second_peer = self
            .routers
            .get(&second_local)
            .map(|r| r.outbound_length())
            .unwrap_or(0);
        // Decay all six edges (the official order).
        {
            let router = self
                .routers
                .get_mut(&first_local)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            let edge = &mut router.outward;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay first peer edge"));
            }
            edge.set_length_to_decaying_link(length_from_first_peer);
            edge.set_length_from_decaying_link(length_from_second_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&second_local)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            let edge = &mut router.outward;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay second peer edge"));
            }
            edge.set_length_to_decaying_link(length_from_second_peer);
            edge.set_length_from_decaying_link(length_from_first_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("bridge router missing"))?;
            let edge = &mut router.outward;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay this outward edge"));
            }
            edge.set_length_to_decaying_link(length_from_second_peer);
            edge.set_length_from_decaying_link(length_from_first_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&second_bridge)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            let edge = &mut router.outward;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay bridge peer edge"));
            }
            edge.set_length_to_decaying_link(length_from_first_peer);
            edge.set_length_from_decaying_link(length_from_second_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("bridge router missing"))?;
            let edge = router
                .bridge
                .as_mut()
                .ok_or(RoutingError::BadParcel("bridge missing"))?;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay this bridge edge"));
            }
            edge.set_length_to_decaying_link(length_from_first_peer);
            edge.set_length_from_decaying_link(length_from_second_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&second_bridge)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            let edge = router
                .bridge
                .as_mut()
                .ok_or(RoutingError::BadParcel("bridge peer missing bridge"))?;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel(
                    "failed to decay bridge peer's bridge edge",
                ));
            }
            edge.set_length_to_decaying_link(length_from_second_peer);
            edge.set_length_from_decaying_link(length_from_first_peer);
        }
        // New local central link between the two outward peers.
        let (link_a, link_b) = Link::local_pair(
            LinkKind::Central,
            LinkSide::A,
            first_local,
            second_local,
            true,
        );
        {
            let router = self
                .routers
                .get_mut(&first_local)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            router.outward.set_primary_link(link_a);
        }
        {
            let router = self
                .routers
                .get_mut(&second_local)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            router.outward.set_primary_link(link_b);
        }
        self.router_flush(rid, false)?;
        self.router_flush(second_bridge, false)?;
        self.router_flush(first_local, false)?;
        self.router_flush(second_local, false)?;
        Ok(())
    }

    /// `Router::StartBridgeBypassFromLocalPeer`: the bridge router whose
    /// outward peer is local initiates a bypass of both bridge routers with a
    /// new central link from the local peer directly to the other bridge
    /// router's remote outward peer.
    fn start_bridge_bypass_from_local_peer(
        &mut self,
        rid: u64,
        link_state: FragmentDescriptor,
    ) -> Result<(), RoutingError> {
        let (local_peer, other_bridge) = {
            let Some(router) = self.routers.get(&rid) else {
                return Ok(());
            };
            if router.bridge.is_none() || !router.bridge_stable() {
                return Ok(());
            }
            (
                router.outward.get_local_peer(),
                router.bridge.as_ref().and_then(|b| b.get_local_peer()),
            )
        };
        let (Some(local_peer), Some(other_bridge)) = (local_peer, other_bridge) else {
            return Ok(());
        };

        // The other bridge router's outward link must be a remote link to the
        // peer node.
        let remote_link = {
            let router = self
                .routers
                .get(&other_bridge)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            let link = router
                .outward
                .primary
                .clone()
                .ok_or(RoutingError::BadParcel("bridge peer has no outward link"))?;
            if link.is_local() {
                return Err(RoutingError::BadParcel("bridge peer outward link is local"));
            }
            link
        };
        if link_state.is_null() {
            // The official retries asynchronously after allocating a
            // fragment; allocation is synchronous here, so a null state is a
            // protocol violation.
            return Err(RoutingError::BadParcel("null link state for bridge bypass"));
        }

        let length_from_local_peer = self
            .routers
            .get(&local_peer)
            .map(|r| r.outbound_length())
            .unwrap_or(0);
        let bypass_sublink = self.memory()?.allocate_sublink_ids(1)?;

        // Decay all five edges (the official order): the local peer's
        // outward, the other bridge router's outward, this bridge edge, this
        // outward edge, and the other bridge router's bridge edge.
        {
            let router = self
                .routers
                .get_mut(&local_peer)
                .ok_or(RoutingError::BadParcel("local peer missing"))?;
            let edge = &mut router.outward;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel(
                    "failed to decay local peer's outward edge",
                ));
            }
            edge.set_length_to_decaying_link(length_from_local_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&other_bridge)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            let edge = &mut router.outward;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel(
                    "failed to decay bridge peer's outward edge",
                ));
            }
            edge.set_length_to_decaying_link(length_from_local_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("bridge router missing"))?;
            let edge = router
                .bridge
                .as_mut()
                .ok_or(RoutingError::BadParcel("bridge missing"))?;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay this bridge edge"));
            }
            edge.set_length_to_decaying_link(length_from_local_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("bridge router missing"))?;
            let edge = &mut router.outward;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failed to decay this outward edge"));
            }
            edge.set_length_from_decaying_link(length_from_local_peer);
        }
        {
            let router = self
                .routers
                .get_mut(&other_bridge)
                .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
            let edge = router
                .bridge
                .as_mut()
                .ok_or(RoutingError::BadParcel("bridge peer missing bridge"))?;
            if !edge.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel(
                    "failed to decay bridge peer's bridge edge",
                ));
            }
            edge.set_length_from_decaying_link(length_from_local_peer);
        }

        // Notify the remote peer of the bypass, then let the local peer adopt
        // the new central link.
        self.send_link_message(messages::encode_bypass_peer_with_link(
            remote_link.sublink,
            bypass_sublink,
            link_state,
            length_from_local_peer,
        ))?;
        let new_link = Link::remote(
            0,
            bypass_sublink,
            LinkKind::Central,
            LinkSide::A,
            Some(link_state),
        );
        self.set_outward_link(local_peer, &new_link)?;
        self.owners
            .insert((LINK_ID_BROKER, bypass_sublink), local_peer);

        self.router_flush(rid, false)?;
        self.router_flush(other_bridge, false)?;
        self.router_flush(local_peer, false)?;
        Ok(())
    }

    /// `Router::Flush`: forward parcels, finish decays, deliver to the portal,
    /// propagate closure, attempt bridge bypass, and flush the peer if it is
    /// waiting. `force` mirrors `FlushBehavior::kForceProxyBypassAttempt`
    /// (set when the router was flushed by a `FlushRouter` message or a
    /// local-link waiting-bit wakeup).
    fn router_flush(&mut self, rid: u64, force: bool) -> Result<(), RoutingError> {
        // Capture the flush-start conditions (the official lock block captures
        // them before any mutation).
        let (on_central, had_decaying_outward, had_decaying_inward) = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
            (
                router.on_central_link(),
                router.outward_has_decaying(),
                router.inward_has_decaying(),
            )
        };

        // Collect parcels to transmit: outbound (toward the far portal) then
        // inbound (forwarded over the inward edge, or over the bridge edge for
        // merged routers).
        let outbound = self
            .routers
            .get_mut(&rid)
            .map(|r| r.collect_outbound())
            .unwrap_or_default();
        let inbound = {
            let router = self.routers.get_mut(&rid);
            match router {
                Some(r) if r.inward.is_some() => r.collect_inbound(),
                Some(r) if r.bridge.is_some() => r.collect_bridge(),
                _ => Vec::new(),
            }
        };
        for (link, parcel) in outbound {
            self.transmit_parcel(&link, parcel)?;
        }
        for (link, parcel) in inbound {
            self.transmit_parcel(&link, parcel)?;
        }

        // Finish decays (releasing the decaying links) and the bridge decay.
        let (out_decayed, in_decayed) = {
            let (out_link, out_decayed, in_link, in_decayed) = {
                let router = self
                    .routers
                    .get_mut(&rid)
                    .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
                // The bridge decay completion is driven by the flush; the
                // released bridge edge's sublink ownership is swept by
                // `remove_router`.
                let _ = router.finish_bridge_decay();
                router.finish_decays()
            };
            // Release the shared `RouterLinkState` refs of the fully decayed
            // links (the official drops the `RemoteRouterLink`s, whose
            // `FragmentRef`s release; the last ref frees the block).
            if let Some(link) = out_link {
                self.release_link_state(&link)?;
            }
            if let Some(link) = in_link {
                self.release_link_state(&link)?;
            }
            (out_decayed, in_decayed)
        };

        // Deliver contiguous inbound parcels to the terminal portal; drop a
        // proxy with no links left.
        let mut drop = false;
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
            if router.inward.is_none() && router.portal.is_some() {
                let mut delivered = Vec::new();
                while let Some(parcel) = router.inbound.pop() {
                    delivered.push((parcel.data, parcel.objects));
                }
                if let Some(portal) = &mut router.portal {
                    for (data, objects) in delivered {
                        portal.messages.push_back((data, objects));
                    }
                }
            }
            if router.inward.is_some()
                && router.outward.primary.is_none()
                && router.outward.decaying.is_none()
            {
                drop = true;
            }
        }
        if drop {
            self.remove_router(rid);
            return Ok(());
        }

        // Mark the central link stable once a decay completes and both edges
        // are stable (the official `Flush`'s `MarkSideStable` after
        // `MaybeFinishDecay`). This is what unblocks the peer's own bypass and
        // lets the waiting-bit wakeup fire.
        let either_decayed = out_decayed || in_decayed;
        let both_stable = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
            let outward_stable =
                router.outward.primary.is_some() && (!had_decaying_outward || out_decayed);
            let inward_stable = if router.inward.is_some() {
                !had_decaying_inward || in_decayed
            } else {
                true
            };
            outward_stable && inward_stable
        };
        let mut dropped_last_decaying_link = false;
        if on_central && either_decayed && both_stable {
            if let Some(link) = self
                .routers
                .get(&rid)
                .and_then(|r| r.outward.primary.clone())
            {
                self.mark_side_stable_on_link(&link)?;
                dropped_last_decaying_link = true;
            }
        }

        // Closure propagation.
        let mut route_closed: Option<(Link, u64)> = None;
        let mut forward_closed: Option<(Link, u64)> = None;
        let mut try_lock: Option<(Link, u64)> = None;
        let mut dead_outward = false;
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
            let outbound_done = router.outbound.is_sequence_fully_consumed();
            let inbound_expects_more = router.inbound.expects_more_elements();
            let inbound_consumed = router.inbound.is_sequence_fully_consumed();

            if on_central && outbound_done {
                if let (Some(primary), Some(final_len)) = (
                    router.outward.primary.clone(),
                    router.outbound.final_sequence_length(),
                ) {
                    try_lock = Some((primary, final_len));
                }
            } else if !inbound_expects_more {
                if router.outward.primary.is_some() {
                    router.outward.release_primary_link();
                    dead_outward = true;
                }
            }
            if inbound_consumed {
                if let Some(final_len) = router.inbound.final_sequence_length() {
                    if let Some(inward) = &mut router.inward {
                        if let Some(link) = inward.release_primary_link() {
                            forward_closed = Some((link, final_len));
                        }
                    } else if router.bridge.is_some() {
                        if let Some(link) = router.bridge_link() {
                            router.bridge = None;
                            forward_closed = Some((link, final_len));
                        }
                    }
                }
            }
        }
        if let Some((link, final_len)) = try_lock {
            if self.try_lock_link_for_closure(&link)? {
                dead_outward = true;
                self.routers
                    .get_mut(&rid)
                    .map(|r| r.outward.release_primary_link());
                route_closed = Some((link, final_len));
            }
        }

        // Bridge bypass: possible only with a bridge edge, a stable outward
        // link, and no inward links.
        let (bridge_present, has_stable_outward, has_no_inward) = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
            let has_stable_outward =
                router.outward.primary.is_some() && (!had_decaying_outward || out_decayed);
            let has_no_inward = router.inward.is_none() && (!had_decaying_inward || in_decayed);
            (router.bridge.is_some(), has_stable_outward, has_no_inward)
        };
        if bridge_present && has_stable_outward && has_no_inward {
            self.maybe_start_bridge_bypass(rid)?;
        }

        // Deliver closures over the released links (local peers receive the
        // closure directly; remote links get a RouteClosed message).
        if let Some((link, len)) = route_closed {
            self.deliver_route_closed(&link, len)?;
        }
        if let Some((link, len)) = forward_closed {
            self.deliver_route_closed(&link, len)?;
        }

        // The flush tail: no further work when the outward link is gone or
        // the router is not on a central link; otherwise flush the other side
        // if it is waiting on our stability.
        if dead_outward || !on_central {
            return Ok(());
        }
        if !dropped_last_decaying_link && !force {
            return Ok(());
        }
        if let Some(link) = self
            .routers
            .get(&rid)
            .and_then(|r| r.outward.primary.clone())
        {
            self.flush_other_side_if_waiting(&link)?;
        }
        Ok(())
    }

    /// `Router::CloseRoute`: close a terminal router's route locally.
    fn close_route(&mut self, rid: u64) -> Result<(), RoutingError> {
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("close for unknown router"))?;
            if router.inward.is_some() {
                return Err(RoutingError::BadParcel("closing a proxy"));
            }
            let current = router.outbound.current_sequence_number();
            if !router.outbound.set_final_sequence_length(current) {
                return Err(RoutingError::BadParcel("close sequence regression"));
            }
        }
        self.router_flush(rid, false)
    }

    /// `Router::AcceptRouteClosureFrom`: the far end closed after sending
    /// `sequence_length` parcels.
    fn router_route_closed(
        &mut self,
        rid: u64,
        sublink: u64,
        sequence_length: u64,
    ) -> Result<(), RoutingError> {
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("route closed for unknown router"))?;
            let on_outward = router
                .outward
                .primary
                .as_ref()
                .is_some_and(|l| l.sublink == sublink)
                || router
                    .outward
                    .decaying_link()
                    .is_some_and(|l| l.sublink == sublink);
            if on_outward {
                if !router.inbound.set_final_sequence_length(sequence_length) {
                    return Err(RoutingError::BadParcel("closure sequence regression"));
                }
                router.peer_closed = true;
            } else if !router.outbound.set_final_sequence_length(sequence_length) {
                return Err(RoutingError::BadParcel("closure sequence regression"));
            }
        }
        self.router_flush(rid, false)
    }

    /// `Router::AcceptRouteClosureFrom` for a closure delivered over a local
    /// link (`LocalRouterLink::AcceptRouteClosure`): the link type selects the
    /// queue the final length applies to; a bridge closure also releases the
    /// bridge edge.
    fn router_route_closed_local(
        &mut self,
        rid: u64,
        kind: LinkKind,
        sequence_length: u64,
    ) -> Result<(), RoutingError> {
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("route closed for unknown router"))?;
            if kind.is_outward() {
                if !router.inbound.set_final_sequence_length(sequence_length) {
                    return Err(RoutingError::BadParcel("closure sequence regression"));
                }
                if router.inward.is_none() && router.bridge.is_none() {
                    router.peer_closed = true;
                }
            } else if kind == LinkKind::PeripheralInward {
                if !router.outbound.set_final_sequence_length(sequence_length) {
                    return Err(RoutingError::BadParcel("closure sequence regression"));
                }
            } else if kind.is_bridge() {
                if !router.outbound.set_final_sequence_length(sequence_length) {
                    return Err(RoutingError::BadParcel("closure sequence regression"));
                }
                router.bridge = None;
            }
        }
        self.router_flush(rid, false)
    }

    /// `Router::AcceptRouteDisconnectedFrom`: a node on the route was lost.
    fn router_disconnected(&mut self, rid: u64) -> Result<(), RoutingError> {
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("disconnect for unknown router"))?;
            router.disconnected = true;
            router.outward.release_primary_link();
            router.outward.release_decaying_link();
            if let Some(inward) = &mut router.inward {
                inward.release_primary_link();
                inward.release_decaying_link();
            } else if let Some(bridge) = &mut router.bridge {
                // Bridges forward disconnection over their link like any other
                // edge (`AcceptRouteDisconnectedFrom`'s `forwarding_links`).
                bridge.release_primary_link();
                bridge.release_decaying_link();
            }
            router.peer_closed = true;
            let _ = router
                .inbound
                .set_final_sequence_length(router.inbound.current_sequence_number());
        }
        self.remove_router(rid);
        Ok(())
    }

    /// `Router::StopProxying`: the final sequence lengths for this proxy.
    fn router_stop_proxying(
        &mut self,
        rid: u64,
        inbound_sequence_length: u64,
        outbound_sequence_length: u64,
    ) -> Result<(), RoutingError> {
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("stop proxying for unknown router"))?;
            if router.outward.is_stable() {
                return Err(RoutingError::BadParcel("StopProxying on a non-proxy"));
            }
            let Some(inward) = &mut router.inward else {
                return Err(RoutingError::BadParcel("StopProxying on a terminal router"));
            };
            if inward.is_stable() {
                return Err(RoutingError::BadParcel(
                    "StopProxying with stable inward edge",
                ));
            }
            if inward.length_to_decaying_link().is_some()
                || inward.length_from_decaying_link().is_some()
                || router.outward.length_to_decaying_link().is_some()
                || router.outward.length_from_decaying_link().is_some()
            {
                return Err(RoutingError::BadParcel("StopProxying with set lengths"));
            }
            inward.set_length_to_decaying_link(inbound_sequence_length);
            inward.set_length_from_decaying_link(outbound_sequence_length);
            router
                .outward
                .set_length_to_decaying_link(outbound_sequence_length);
            router
                .outward
                .set_length_from_decaying_link(inbound_sequence_length);
        }
        self.router_flush(rid, false)
    }

    /// `Router::StopProxyingToLocalPeer`: the peer bypassed a proxy whose
    /// route includes this router; finalize the decay lengths. Handles both
    /// the plain local-peer case and the bridge-peer case (three local
    /// routers: this router, its bridge peer, and the bridge peer's outward
    /// peer).
    fn router_stop_proxying_to_local_peer(
        &mut self,
        rid: u64,
        outbound_sequence_length: u64,
    ) -> Result<(), RoutingError> {
        // Disambiguate the recipient: a decaying bridge link's local peer, or
        // a decaying outward link's local peer.
        let (bridge_peer, local_peer) = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("stop proxying for unknown router"))?;
            match &router.bridge {
                Some(b) => (b.get_decaying_local_peer(), None),
                None => (
                    None,
                    router.outward.decaying_link().and_then(|l| l.local_peer),
                ),
            }
        };

        // The common case: this router, its decaying local peer, and the
        // proxy on the other end of the decaying links.
        if let Some(local_peer) = local_peer {
            if bridge_peer.is_some() {
                return Err(RoutingError::BadParcel("ambiguous local peers"));
            }
            let valid = {
                let this_router = self
                    .routers
                    .get(&rid)
                    .ok_or(RoutingError::BadParcel("stop proxying for unknown router"))?;
                let Some(peer_router) = self.routers.get(&local_peer) else {
                    // The peer was disconnected; ignore the request.
                    return Ok(());
                };
                let our_link = this_router.outward.decaying_link();
                let peer_link = peer_router.outward.decaying_link();
                let (Some(our_link), Some(peer_link)) = (our_link, peer_link) else {
                    return Ok(());
                };
                our_link.is_local()
                    && peer_link.is_local()
                    && our_link.local_peer == Some(local_peer)
                    && peer_link.local_peer == Some(rid)
                    && this_router.inward.is_some()
                    && peer_router.outward.length_from_decaying_link().is_none()
                    && this_router.outward.length_to_decaying_link().is_none()
                    && this_router
                        .inward
                        .as_ref()
                        .is_none_or(|e| e.length_from_decaying_link().is_none())
            };
            if !valid {
                return Err(RoutingError::BadParcel("invalid proxy"));
            }
            if let Some(peer_router) = self.routers.get_mut(&local_peer) {
                peer_router
                    .outward
                    .set_length_from_decaying_link(outbound_sequence_length);
            }
            if let Some(this_router) = self.routers.get_mut(&rid) {
                this_router
                    .outward
                    .set_length_to_decaying_link(outbound_sequence_length);
                if let Some(inward) = &mut this_router.inward {
                    inward.set_length_from_decaying_link(outbound_sequence_length);
                }
            }
            self.router_flush(rid, false)?;
            self.router_flush(local_peer, false)?;
            return Ok(());
        }

        // The bridge case: three local routers are involved. Both this router
        // and its bridge peer serve as "the" proxy being bypassed.
        if let Some(bridge_peer) = bridge_peer {
            let local_peer2 = {
                let bp = self
                    .routers
                    .get(&bridge_peer)
                    .ok_or(RoutingError::BadParcel("bridge peer missing"))?;
                if bp.outward.is_stable() {
                    return Err(RoutingError::BadParcel("invalid bridge peer"));
                }
                bp.outward
                    .get_decaying_local_peer()
                    .ok_or(RoutingError::BadParcel(
                        "bridge peer has no decaying local peer",
                    ))?
            };
            let valid = {
                let this_router = self
                    .routers
                    .get(&rid)
                    .ok_or(RoutingError::BadParcel("stop proxying for unknown router"))?;
                let (Some(peer_router), Some(bp)) = (
                    self.routers.get(&local_peer2),
                    self.routers.get(&bridge_peer),
                ) else {
                    return Ok(());
                };
                !this_router.outward.is_stable()
                    && !peer_router.outward.is_stable()
                    && !bp.outward.is_stable()
                    && peer_router.outward.length_from_decaying_link().is_none()
                    && this_router.outward.length_from_decaying_link().is_none()
                    && this_router
                        .bridge
                        .as_ref()
                        .is_none_or(|e| e.length_to_decaying_link().is_none())
                    && bp.outward.length_to_decaying_link().is_none()
                    && bp
                        .bridge
                        .as_ref()
                        .is_none_or(|e| e.length_from_decaying_link().is_none())
            };
            if !valid {
                return Err(RoutingError::BadParcel("invalid bridge proxy"));
            }
            if let Some(peer_router) = self.routers.get_mut(&local_peer2) {
                peer_router
                    .outward
                    .set_length_from_decaying_link(outbound_sequence_length);
            }
            if let Some(this_router) = self.routers.get_mut(&rid) {
                this_router
                    .outward
                    .set_length_from_decaying_link(outbound_sequence_length);
                if let Some(b) = &mut this_router.bridge {
                    b.set_length_to_decaying_link(outbound_sequence_length);
                }
            }
            if let Some(bp) = self.routers.get_mut(&bridge_peer) {
                bp.outward
                    .set_length_to_decaying_link(outbound_sequence_length);
                if let Some(b) = &mut bp.bridge {
                    b.set_length_from_decaying_link(outbound_sequence_length);
                }
            }
            self.router_flush(rid, false)?;
            self.router_flush(local_peer2, false)?;
            self.router_flush(bridge_peer, false)?;
            return Ok(());
        }

        // No local peer and no bridge peer: the request is invalid (or the
        // router was disconnected, in which case it is silently ignored).
        if self.routers.get(&rid).is_some_and(|r| r.disconnected) {
            Ok(())
        } else {
            Err(RoutingError::BadParcel(
                "no local peer for StopProxyingToLocalPeer",
            ))
        }
    }

    /// `Router::AcceptBypassLink` (the receive side of `BypassPeerWithLink`):
    /// adopt the new central link on `b.new_sublink`, begin decaying the old
    /// link, and tell the peer to stop proxying on the old sublink. `link_id`
    /// is the NodeLink the message arrived on (the new sublink is scoped to
    /// it).
    fn on_bypass_peer_with_link(
        &mut self,
        b: messages::BypassPeerWithLink,
        link_id: u64,
    ) -> Result<(), RoutingError> {
        if b.new_link_state_fragment.is_null() {
            return Err(RoutingError::BadParcel("bypass with null link state"));
        }
        self.memory()?.fragment(b.new_link_state_fragment)?;

        // The message targets the router owning the old sublink; a deactivated
        // sublink is silently ignored (the official `GetRouter` returns null).
        let rid = match self.owners.get(&(link_id, b.sublink)).copied() {
            Some(rid) => rid,
            None => return Ok(()),
        };
        let old_sublink;
        let length_to_proxy_from_us;
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("bypass for unknown router"))?;
            if router.disconnected || router.outward.primary.is_none() {
                // The route is already dysfunctional; discard the bypass link.
                return Ok(());
            }
            let old = router
                .outward
                .primary
                .clone()
                .ok_or(RoutingError::BadParcel("bypass without outward link"))?;
            if old.is_local() {
                // Bypass links only make sense at a remote outward link.
                return Err(RoutingError::BadParcel("unexpected bypass at a local link"));
            }
            // The new link goes to the same node as the old one (the native
            // acceptor has a single NodeLink, so the official same-node
            // shortcut always applies; `CanNodeRequestBypass` is never needed).
            length_to_proxy_from_us = router.outbound.current_sequence_number();
            if !router.outward.begin_primary_link_decay() {
                return Err(RoutingError::BadParcel("failure to decay link"));
            }
            router
                .outward
                .set_length_to_decaying_link(length_to_proxy_from_us);
            router
                .outward
                .set_length_from_decaying_link(b.inbound_sequence_length);
            old_sublink = old.sublink;
            router.outward.set_primary_link(Link::remote(
                link_id,
                b.new_sublink,
                LinkKind::Central,
                LinkSide::B,
                Some(b.new_link_state_fragment),
            ));
            self.owners.insert((link_id, b.new_sublink), rid);
        }

        // The new link goes to the same node as the old one: tell the peer to
        // stop proxying on the old sublink.
        self.send_link_message(messages::encode_stop_proxying_to_local_peer(
            old_sublink,
            length_to_proxy_from_us,
        ))?;

        // Drain early parcels for the new sublink.
        if let Some(queued) = self.early_parcels.remove(&(link_id, b.new_sublink)) {
            for p in queued {
                self.process_accept_parcel(p, Vec::new(), link_id)?;
            }
        }
        self.router_flush(rid, false)
    }

    /// Deserialize the new routers described by an AcceptParcel's
    /// `new_routers` array; returns their identity sublinks in order.
    fn deserialize_portals(
        &mut self,
        p: &AcceptParcel,
        link_id: u64,
    ) -> Result<Vec<u64>, RoutingError> {
        if p.new_routers.len() % RouterDescriptor::SIZE != 0 {
            return Err(RoutingError::BadParcel("malformed new_routers array"));
        }
        let num = p.new_routers.len() / RouterDescriptor::SIZE;
        if num
            != p.handle_types
                .iter()
                .filter(|&&t| t == handle_type::PORTAL)
                .count()
        {
            return Err(RoutingError::BadParcel("portal count mismatch"));
        }
        let mut out = Vec::with_capacity(num);
        for i in 0..num {
            let bytes =
                &p.new_routers[i * RouterDescriptor::SIZE..(i + 1) * RouterDescriptor::SIZE];
            let d = RouterDescriptor::decode(bytes)?;
            let rid = self.router_deserialize(d, link_id)?;
            out.push(rid);
        }
        Ok(out)
    }

    /// Transmit one parcel on a link, serializing any attached portals. Local
    /// links deliver the parcel directly to the peer router
    /// (`LocalRouterLink::AcceptParcel`: central links deliver as inbound,
    /// bridge links as outbound); remote links transmit over the NodeLink.
    fn transmit_parcel(&mut self, link: &Link, parcel: Parcel) -> Result<(), RoutingError> {
        if let Some(peer_rid) = link.local_peer {
            let accepted = match link.kind {
                LinkKind::Central => self
                    .routers
                    .get_mut(&peer_rid)
                    .map(|r| r.accept_inbound(parcel))
                    .unwrap_or(false),
                LinkKind::Bridge => self
                    .routers
                    .get_mut(&peer_rid)
                    .map(|r| r.accept_outbound(parcel))
                    .unwrap_or(false),
                LinkKind::PeripheralOutward | LinkKind::PeripheralInward => {
                    // Local links are only ever central or bridge.
                    return Err(RoutingError::BadParcel("peripheral local link"));
                }
            };
            if !accepted {
                return Err(RoutingError::BadParcel("local delivery sequence gap"));
            }
            // `AcceptInboundParcel` / `AcceptOutboundParcel` flush the
            // receiver.
            return self.router_flush(peer_rid, false);
        }

        let sublink = link.sublink;
        let mut handle_types: Vec<u32> = Vec::new();
        let mut serialized: Vec<(u64, RouterDescriptor)> = Vec::new();
        for obj in &parcel.objects {
            match obj {
                Object::Fd(_) => {
                    return Err(RoutingError::Unsupported(
                        MSG_ID_ACCEPT_PARCEL,
                        "fd transmission not exercised by the routing court",
                    ));
                }
                Object::Router(rid) => {
                    handle_types.push(handle_type::PORTAL);
                    serialized.push(self.serialize_router(*rid, link.link_id)?);
                }
            }
        }
        let new_routers: Vec<Vec<u8>> = serialized.iter().map(|(_, d)| d.encode()).collect();
        let msg = messages::encode_accept_parcel_full(
            sublink,
            parcel.sequence_number,
            parcel.fragment,
            &parcel.data,
            &handle_types,
            &new_routers,
            &[],
        );
        self.send_link_message(msg)?;
        for (rid, d) in serialized {
            self.begin_proxying(rid, &d, link.link_id)?;
        }
        Ok(())
    }

    /// `Router::Put` from a portal: allocate the parcel data (`AllocateOutboundParcel`),
    /// enqueue an outbound parcel, and flush.
    ///
    /// The data allocation happens at put time based on the outward primary
    /// link: a remote link allocates a shared-memory fragment (`Parcel::AllocateData`
    /// with the link memory), falling back to inline data when no block is
    /// available; a local link (or none) allocates inline.
    ///
    /// Returns the fragment descriptor backing the payload, if one was
    /// allocated (the caller can verify the block-capacity expansion).
    fn put(
        &mut self,
        rid: u64,
        data: Vec<u8>,
        objects: Vec<Object>,
    ) -> Result<Option<FragmentDescriptor>, RoutingError> {
        let (seq, use_fragment) = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("put for unknown router"))?;
            let seq = router.outbound.current_sequence_number();
            let use_fragment = !data.is_empty()
                && router
                    .outward
                    .primary
                    .as_ref()
                    .is_some_and(|l| !l.is_local());
            (seq, use_fragment)
        };
        let fragment = if use_fragment {
            self.write_parcel_fragment_or_inline(&data)?
        } else {
            None
        };
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("put for unknown router"))?;
            let parcel = Parcel {
                sequence_number: seq,
                data,
                objects,
                fragment,
            };
            if !router.push_outbound(parcel) {
                return Err(RoutingError::BadParcel("outbound sequence regression"));
            }
        }
        self.router_flush(rid, false)?;
        Ok(fragment)
    }

    /// `Parcel::AllocateData` for a remote outward link: try to allocate a
    /// shared-memory fragment for `data.len() + sizeof(FragmentHeader)` bytes,
    /// falling back to inline data when no block is available.
    ///
    /// NOTE on expansion: the pinned mojo embedder sets
    /// `IPCZ_MEMORY_FIXED_PARCEL_CAPACITY` (ipcz_api.cc, crbug.com/40876289),
    /// so `allow_memory_expansion_for_parcel_data_` is false and
    /// `AllocateFragment` does NOT lobby for parcel data — the inline fallback
    /// is the official behavior in this epoch. The only expansion trigger is
    /// `RouterLinkState` pool exhaustion (the unconditional lobby in
    /// `TryAllocateRouterLinkState`), handled by the link-state path.
    fn write_parcel_fragment_or_inline(
        &mut self,
        data: &[u8],
    ) -> Result<Option<FragmentDescriptor>, RoutingError> {
        match self.memory_mut()?.write_parcel_fragment(data) {
            Ok(f) => Ok(Some(f)),
            Err(crate::ipcz::link_memory::LinkMemoryError::OutOfBounds) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Send a NodeLink message on the given link, assigning the per-link
    /// sequence number.
    fn send_link_message_on(
        &mut self,
        link_id: u64,
        mut payload: Vec<u8>,
    ) -> Result<(), RoutingError> {
        if link_id == LINK_ID_DIRECT {
            let direct = self
                .direct
                .as_mut()
                .ok_or(RoutingError::Unexpected("direct link not established"))?;
            set_message_sequence_number(&mut payload, direct.next_link_seq);
            direct.next_link_seq += 1;
            direct.channel.send(&payload, &[])?;
            return Ok(());
        }
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, &[])?;
        Ok(())
    }

    /// Send a NodeLink message on the given link with attached descriptors
    /// (e.g. an `AddBlockBuffer` carrying the shared buffer).
    fn send_link_message_with_fds_on(
        &mut self,
        link_id: u64,
        mut payload: Vec<u8>,
        fds: &[RawFd],
    ) -> Result<(), RoutingError> {
        if link_id == LINK_ID_DIRECT {
            let direct = self
                .direct
                .as_mut()
                .ok_or(RoutingError::Unexpected("direct link not established"))?;
            set_message_sequence_number(&mut payload, direct.next_link_seq);
            direct.next_link_seq += 1;
            direct.channel.send(&payload, fds)?;
            return Ok(());
        }
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, fds)?;
        Ok(())
    }

    /// Send a NodeLink message, assigning the per-link sequence number.
    fn send_link_message(&mut self, mut payload: Vec<u8>) -> Result<(), RoutingError> {
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, &[])?;
        Ok(())
    }

    /// Send a NodeLink message with attached descriptors (e.g. an
    /// `AddBlockBuffer` carrying the shared buffer).
    fn send_link_message_with_fds(
        &mut self,
        mut payload: Vec<u8>,
        fds: &[RawFd],
    ) -> Result<(), RoutingError> {
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, fds)?;
        Ok(())
    }

    /// `NodeLinkMemory::RequestBlockCapacity`: lobby for a new block buffer of
    /// `block_size`-byte blocks. Exactly one in-flight request per block size
    /// (further requests while one is pending are folded into it, matching
    /// `capacity_callbacks_`'s `need_new_request`); the request is routed
    /// through `Node::AllocateSharedMemory`, which — because this node
    /// connected as the allocation delegate (`IPCZ_CONNECT_NODE_TO_ALLOCATION_DELEGATE`
    /// set by `Invitation::Accept` when local allocation is disabled) — sends
    /// `RequestMemory` to the broker.
    fn request_block_capacity(&mut self, block_size: u32) -> Result<(), RoutingError> {
        if self.capacity_pending.contains(&block_size) {
            return Ok(());
        }
        self.capacity_pending.insert(block_size);
        // `kMinBlockAllocatorCapacity` blocks per page, rounded up to whole
        // pages (`RequestBlockCapacity`'s `num_pages` computation).
        let min_buffer_size = (block_size as usize)
            .saturating_mul(crate::ipcz::link_memory::MIN_BLOCK_ALLOCATOR_CAPACITY);
        let num_pages = min_buffer_size
            .div_ceil(crate::ipcz::link_memory::BLOCK_ALLOCATOR_PAGE_SIZE)
            .max(1);
        let buffer_size = num_pages * crate::ipcz::link_memory::BLOCK_ALLOCATOR_PAGE_SIZE;
        let buffer_size32 = u32::try_from(buffer_size)
            .map_err(|_| RoutingError::BadParcel("block buffer request exceeds u32 size"))?;
        self.pending_memory_requests
            .entry(buffer_size32)
            .or_default()
            .push_back(PendingMemoryRequest {
                buffer_size: buffer_size32,
                block_size,
            });
        self.send_link_message(messages::encode_request_memory(buffer_size32))
    }

    /// `NodeLink::OnProvideMemory`: adopt the provided buffer, initialize its
    /// block allocator region, share it with the peer via `AddBlockBuffer`
    /// (transmitted BEFORE the local registration, matching the official
    /// share-then-register order), and complete the pending capacity request.
    fn on_provide_memory(&mut self, size: u32, fd: OwnedFd) -> Result<(), RoutingError> {
        let pending = self
            .pending_memory_requests
            .get_mut(&size)
            .and_then(VecDeque::pop_front)
            .ok_or(RoutingError::BadParcel(
                "ProvideMemory without a pending request",
            ))?;
        if self
            .pending_memory_requests
            .get(&size)
            .is_some_and(VecDeque::is_empty)
        {
            self.pending_memory_requests.remove(&size);
        }
        let block_size = pending.block_size;
        // Allocate the buffer id from the shared primary header, then share the
        // buffer with the peer before registering it locally.
        let id = self.memory()?.allocate_new_buffer_id()?;
        let dup = fd.try_dup()?;
        let payload = messages::encode_add_block_buffer(id, block_size, 0, size);
        self.send_link_message_with_fds(payload, &[dup.as_raw_fd()])?;
        self.memory_mut()?
            .register_block_buffer(id, fd.into_raw_fd(), block_size)?;
        // The capacity request for this block size has completed; any further
        // requests may now start a fresh round trip.
        self.capacity_pending.remove(&block_size);
        Ok(())
    }

    /// `NodeLink::OnRequestMemory`: this node is the broker for its link and
    /// must allocate and provide a buffer. The routing acceptor is the
    /// allocation *delegate* (not the provider), so the official broker never
    /// sends it `RequestMemory`; multi-node courts with a native broker will
    /// exercise this path.
    fn on_request_memory(&mut self, size: u32) -> Result<(), RoutingError> {
        let _ = size;
        Err(RoutingError::Unsupported(
            MSG_ID_REQUEST_MEMORY,
            "native broker role not exercised by a sealed court",
        ))
    }

    /// Remove a router and all its sublinks. The owners map is swept for any
    /// entry referencing the router, so a deactivated router's sublinks behave
    /// like the official `RemoteRouterLink::Deactivate` (inbound messages on
    /// them are dropped, not errors). The router's shared `RouterLinkState`
    /// references are released (the last ref frees the block).
    fn remove_router(&mut self, rid: u64) {
        let (sublinks, state_links): (Vec<(u64, u64)>, Vec<Link>) = {
            let Some(router) = self.routers.get(&rid) else {
                return;
            };
            let mut out = Vec::new();
            let mut states = Vec::new();
            if let Some(l) = &router.outward.primary {
                out.push((l.link_id, l.sublink));
                if l.link_state.is_some() {
                    states.push(l.clone());
                }
            }
            if let Some(l) = router.outward.decaying_link() {
                out.push((l.link_id, l.sublink));
                if l.link_state.is_some() {
                    states.push(l.clone());
                }
            }
            if let Some(inward) = &router.inward {
                if let Some(l) = &inward.primary {
                    out.push((l.link_id, l.sublink));
                    if l.link_state.is_some() {
                        states.push(l.clone());
                    }
                }
                if let Some(l) = inward.decaying_link() {
                    out.push((l.link_id, l.sublink));
                    if l.link_state.is_some() {
                        states.push(l.clone());
                    }
                }
            }
            (out, states)
        };
        for s in sublinks {
            self.owners.remove(&s);
        }
        // Sweep stale owners entries (released links, e.g. a fully decayed
        // peripheral link whose sublink was already released from the edge).
        let stale: Vec<(u64, u64)> = self
            .owners
            .iter()
            .filter(|(_, owner)| **owner == rid)
            .map(|(s, _)| *s)
            .collect();
        for s in stale {
            self.owners.remove(&s);
        }
        for link in state_links {
            let _ = self.release_link_state(&link);
        }
        self.routers.remove(&rid);
    }

    /// Release a shared `RouterLinkState` reference held by a link; the last
    /// reference frees the block back to the shared pool
    /// (`GenericFragmentRef::reset` -> `NodeLinkMemory::FreeFragment`). Local
    /// links carry in-process state and hold no shared reference; the fixed
    /// initial-portal states are unmanaged (never refcounted or freed).
    fn release_link_state(&mut self, link: &Link) -> Result<(), RoutingError> {
        let Some(state) = link.link_state else {
            return Ok(());
        };
        // The initial portals' fixed `RouterLinkState`s (the unmanaged
        // `GetInitialRouterLinkState` refs) are never released.
        if crate::ipcz::link_memory::LinkMemory::is_initial_link_state(state) {
            return Ok(());
        }
        let last = self
            .memory_for(link.link_id)?
            .release_link_state_ref(state)?;
        if last {
            // The official `FreeFragment` asserts the fragment is addressable
            // and within a block allocator; a failure here is a protocol error.
            if !self.memory_for(link.link_id)?.free_block(state)? {
                return Err(RoutingError::BadParcel("failed to free link state block"));
            }
        }
        Ok(())
    }

    /// Drain all currently available messages on the broker link (used before
    /// sending, so in-flight routing messages are processed first).
    fn drain_available(&mut self) -> Result<(), RoutingError> {
        loop {
            match self.channel.recv_available()? {
                RecvResult::Message(m) => {
                    let decoded = messages::decode_message(&m.payload, m.fds.len())?;
                    self.dispatch(decoded, m.fds, LINK_ID_BROKER)?;
                }
                RecvResult::WouldBlock => return Ok(()),
                RecvResult::PeerClosed => {
                    return Err(RoutingError::Unexpected("peer closed"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::ipcz::link_memory::{LinkMemory, PRIMARY_BUFFER_SIZE};
    use crate::ipcz::messages::MSG_ID_REQUEST_MEMORY;
    use crate::ipcz::router::{Link, LinkKind, LinkSide};
    use crate::ipcz::wire::parse_stream;
    use mojo_rs_platform::shm::{Access, SharedMemory};
    use mojo_rs_platform::socket::socketpair;
    use std::os::unix::io::{AsRawFd, IntoRawFd};

    /// A fresh memfd-backed primary buffer initialized like the broker's
    /// `AllocateMemory`: header counters, initialized allocator regions.
    fn fresh_buffer() -> (SharedMemory, RawFd) {
        let mem = SharedMemory::create("test-routing-mem", PRIMARY_BUFFER_SIZE).unwrap();
        {
            let mut map = mem.map(0, PRIMARY_BUFFER_SIZE, Access::ReadWrite).unwrap();
            for b in map.iter_mut() {
                *b = 0;
            }
            map[0..8].copy_from_slice(&1u64.to_le_bytes());
            map[8..16].copy_from_slice(
                &(crate::ipcz::link_memory::MAX_INITIAL_PORTALS as u64).to_le_bytes(),
            );
            for &(block_size, off, count) in crate::ipcz::link_memory::PRIMARY_BLOCK_REGIONS {
                let last = off + (count - 1) * block_size as usize;
                let rel = -(count as i16);
                map[last + 2..last + 4].copy_from_slice(&rel.to_le_bytes());
            }
        }
        // SAFETY: dup is a plain syscall on a valid descriptor.
        let fd = unsafe { libc::dup(mem.as_raw_fd()) };
        assert!(fd >= 0);
        (mem, fd)
    }

    /// An acceptor with the link memory injected (no Connect handshake):
    /// enough for the serialization state machines under test.
    fn acceptor_with_memory() -> (RoutingAcceptor, mojo_rs_platform::socket::SocketPair) {
        let (_keep, fd) = fresh_buffer();
        let pair = socketpair().unwrap();
        let mut a = RoutingAcceptor::new(pair.a.as_raw_fd()).unwrap();
        a.link_memory = Some(LinkMemory::adopt_primary(fd).unwrap());
        a.broker_name = NodeName {
            high: 0x1111,
            low: 0x2222,
        };
        (a, pair)
    }

    /// `OpenPortals` creates a stable local central link pair.
    #[test]
    fn open_portals_creates_stable_local_pair() {
        let (mut a, _pair) = acceptor_with_memory();
        let (p1, p2) = a.open_portals().unwrap();
        let r1 = a.routers.get(&p1).unwrap();
        let r2 = a.routers.get(&p2).unwrap();
        assert!(r1.outward.primary.is_some());
        assert!(r2.outward.primary.is_some());
        let l1 = r1.outward.primary.as_ref().unwrap();
        assert_eq!(l1.local_peer, Some(p2));
        assert!(l1.kind.is_central());
        let state = l1.local_state.as_ref().unwrap().borrow();
        assert_eq!(
            state.status,
            crate::ipcz::link_memory::RouterLinkStatus::STABLE
        );
    }

    /// The WithLocalPeer serialization: a new central link with a pool
    /// `RouterLinkState`, an adjacent decaying peripheral sublink, and the
    /// proxy's inward edge armed with a deferred decay.
    #[test]
    fn serialize_with_local_peer_splits_the_pair() {
        let (mut a, pair) = acceptor_with_memory();
        let mut peer = crate::ipcz::channel::Channel::adopt(pair.b.into_raw_fd()).unwrap();
        let (p1, p2) = a.open_portals().unwrap();
        let d = a
            .serialize_router_with_local_peer(p2, p1, LINK_ID_BROKER)
            .unwrap()
            .expect("link state available");

        assert!(d.proxy_already_bypassed);
        assert_eq!(d.new_decaying_sublink, d.new_sublink + 1);
        assert!(!d.new_link_state_fragment.is_null());
        assert_eq!(
            d.new_link_state_fragment.buffer_id,
            crate::ipcz::link_memory::PRIMARY_BUFFER_ID
        );
        // The local peer's outward link was released at serialization.
        assert!(a.routers.get(&p1).unwrap().outward.primary.is_none());
        // The proxy's inward edge waits for a link with a deferred decay.
        let proxy = a.routers.get(&p2).unwrap();
        assert!(proxy.inward.is_some());
        assert!(proxy.inward.as_ref().unwrap().is_decay_deferred());
        // The tentative sublinks are registered on the NodeLink.
        assert_eq!(a.owners.get(&(LINK_ID_BROKER, d.new_sublink)), Some(&p1));
        assert_eq!(
            a.owners.get(&(LINK_ID_BROKER, d.new_decaying_sublink)),
            Some(&p2)
        );
        // The link state was allocated from the shared pool (ref count 2:
        // allocation + the central link's copy).
        let view = a
            .memory()
            .unwrap()
            .fragment(d.new_link_state_fragment)
            .unwrap();
        let ref_count = u32::from_le_bytes(view[0..4].try_into().unwrap());
        assert_eq!(ref_count, 2);

        // Begin proxying: the proxy adopts the decaying peripheral link as its
        // inward edge; the local peer adopts the new central link (side A
        // stable marked). The proxy's decay bounds are 0/0 (a fresh pair with
        // no queued parcels), so the flush completes the decay and the proxy
        // drops immediately.
        a.begin_proxying(p2, &d, LINK_ID_BROKER).unwrap();
        assert!(
            a.routers.get(&p2).is_none(),
            "the fresh-pair proxy decays and drops in the same flush"
        );
        assert_eq!(
            a.owners.get(&(LINK_ID_BROKER, d.new_decaying_sublink)),
            None
        );
        let local = a.routers.get(&p1).unwrap();
        let central = local
            .outward
            .primary
            .as_ref()
            .expect("local peer adopts the central link");
        assert_eq!(central.sublink, d.new_sublink);
        assert!(central.kind.is_central());
        assert_eq!(central.link_state, Some(d.new_link_state_fragment));
        // Side A stable was set by `SetOutwardLink`.
        let status_view = a
            .memory()
            .unwrap()
            .fragment(d.new_link_state_fragment)
            .unwrap();
        let status = u32::from_le_bytes(
            status_view[LinkMemory::LINK_STATUS_OFFSET..LinkMemory::LINK_STATUS_OFFSET + 4]
                .try_into()
                .unwrap(),
        );
        let status = crate::ipcz::link_memory::RouterLinkStatus(status);
        assert!(status.side_a_stable());
        // The flush after begin_proxying transmitted nothing unexpected to the
        // peer (the proxy dropped and the local peer has no parcels).
        assert!(matches!(
            peer.recv_available().unwrap(),
            RecvResult::WouldBlock
        ));
        drop(peer);
    }

    /// The exhaustion fallback: with the 64-byte pool exhausted, the
    /// WithLocalPeer path returns `None`, fires the `RequestBlockCapacity`
    /// lobby (a `RequestMemory` appears on the wire), and the caller can
    /// proceed with the plain proxy path.
    #[test]
    fn with_local_peer_exhaustion_lobbies_request_memory() {
        let (mut a, pair) = acceptor_with_memory();
        let mut peer = crate::ipcz::channel::Channel::adopt(pair.b.into_raw_fd()).unwrap();
        // Exhaust the entire 64-byte pool (all blocks, one at a time).
        let mut allocated = Vec::new();
        while let Some(desc) = a.memory_mut().unwrap().try_allocate_link_state().unwrap() {
            allocated.push(desc);
        }
        assert!(allocated.len() >= 1400, "pool should be large");
        let (p1, p2) = a.open_portals().unwrap();
        let d = a
            .serialize_router_with_local_peer(p2, p1, LINK_ID_BROKER)
            .unwrap();
        assert!(d.is_none(), "exhausted pool must fall back");
        // The unconditional lobby fired: a RequestMemory{65536} went out.
        let msg = peer.recv().unwrap().expect("RequestMemory message");
        let decoded = messages::decode_message(&msg.payload, msg.fds.len()).unwrap();
        match &decoded {
            DecodedMessage::RequestMemory(r) => {
                assert_eq!(r.size, 65536);
            }
            other_msg => panic!("expected RequestMemory, got {other_msg:?}"),
        }
        // The fallback plain proxy path (local outward link: unlocked, no
        // proxy fields) still serializes.
        let (rid, d) = a.serialize_router(p2, LINK_ID_BROKER).unwrap();
        assert_eq!(rid, p2);
        assert!(!d.proxy_already_bypassed);
        assert!(!d.proxy_peer_node_name.is_valid());
        assert!(d.new_link_state_fragment.is_null());
        assert!(matches!(
            peer.recv_available().unwrap(),
            RecvResult::WouldBlock
        ));
        drop(peer);
        drop(allocated);
    }
}
