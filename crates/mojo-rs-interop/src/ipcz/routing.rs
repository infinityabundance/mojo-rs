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
//! * shared `RouterLinkState` coordination (`TryLock`, `SetSideStable`,
//!   `allowed_bypass_request_source`) and the shared sublink allocator.
//!
//! The node is single-threaded (one poll loop), so routers carry no internal
//! locks: the acceptor serializes every operation, matching the official
//! observable state machine. Unsupported inbound messages (`BypassPeer`,
//! `AcceptBypassLink`, `ProxyWillStop`) are rejected explicitly rather than
//! silently ignored.

use std::collections::{HashMap, VecDeque};
use std::os::unix::io::IntoRawFd;

use mojo_rs_casefile::events::{Event, EventKind};
use mojo_rs_platform::fd::OwnedFd;
use mojo_rs_platform::socket::socketpair;

use crate::ipcz::channel::{Channel, ChannelError, RecvResult};
use crate::ipcz::link_memory::{
    LinkMemory, LinkMemoryError, MAX_INITIAL_PORTALS, ROUTER_LINK_STATE_SIZE,
};
use crate::ipcz::messages::{
    self, AcceptParcel, DecodedMessage, FragmentDescriptor, MSG_ID_ACCEPT_BYPASS_LINK,
    MSG_ID_ACCEPT_PARCEL, MSG_ID_BYPASS_PEER, MSG_ID_PROXY_WILL_STOP, NodeName, RouterDescriptor,
    handle_type,
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

/// A processed parcel: its payload and any deserialized portal identities.
struct ProcessedParcel {
    /// The application payload.
    payload: Vec<u8>,
    /// Identity sublinks of deserialized portals, in handle order.
    identities: Vec<u64>,
}

/// The Phase 5 routing acceptor state machine.
pub struct RoutingAcceptor {
    /// The channel to the broker.
    channel: Channel,
    /// The adopted link memory (set by the Connect handshake).
    link_memory: Option<LinkMemory>,
    /// Per-link outgoing message sequence number (after Connect).
    next_link_seq: u64,
    /// Routers by identity sublink (their first primary sublink).
    routers: HashMap<u64, Router>,
    /// Sublink -> owning router identity.
    owners: HashMap<u64, u64>,
    /// Parcels for sublinks whose router is not yet established.
    early_parcels: HashMap<u64, VecDeque<AcceptParcel>>,
    /// The broker's node name (from the Connect greeting).
    broker_name: NodeName,
    /// Events in casefile format.
    events: Vec<Event>,
    /// Event sequence counter.
    event_seq: u64,
}

impl RoutingAcceptor {
    /// Start the routing acceptor on an inherited socket descriptor.
    pub fn new(fd: std::os::unix::io::RawFd) -> Result<RoutingAcceptor, RoutingError> {
        let channel = Channel::adopt(fd)?;
        Ok(RoutingAcceptor {
            channel,
            link_memory: None,
            next_link_seq: 0,
            routers: HashMap::new(),
            owners: HashMap::new(),
            early_parcels: HashMap::new(),
            broker_name: NodeName::invalid(),
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
        let transfer_payload;
        let b1_identity;
        match transfer {
            DecodedMessage::AcceptParcel(p) => {
                let processed = self.process_accept_parcel(p, transfer_fds)?;
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
            DecodedMessage::AcceptParcel(p) => self.process_accept_parcel(p, w1_fds)?.payload,
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

        // Step 3: send r1 over the wire on the b1 route.
        self.put(b1_identity, b"r1".to_vec(), Vec::new())?;
        self.emit(5, EventKind::Result, "MOJO_RESULT_OK");

        // Step 4: send the transfer-back on the bootstrap with the b1 handle.
        // The bootstrap router transmits on its current primary sublink (it
        // may have migrated during the broker's bypass above).
        self.put(
            BOOTSTRAP_SUBLINK,
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
            self.dispatch(DecodedMessage::StopProxying(s), Vec::new())?;
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
            self.dispatch(DecodedMessage::RouteClosed(r), rc_fds)?;
        }
        self.emit(8, EventKind::Message, "MOJO_RESULT_FAILED_PRECONDITION");

        // Step 7: close the bootstrap portal locally. The broker already
        // closed its end, so no closure message is transmitted (the primary
        // link was released when the peer's RouteClosed arrived).
        self.close_route(BOOTSTRAP_SUBLINK)?;
        self.emit(9, EventKind::Lifecycle, "MOJO_RESULT_OK");
        Ok(())
    }

    /// The bootstrap router's current primary sublink (its identity stays
    /// `BOOTSTRAP_SUBLINK`; the primary migrates on bypass).
    fn bootstrap_sublink(&self) -> Result<u64, RoutingError> {
        self.routers
            .get(&BOOTSTRAP_SUBLINK)
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
            let link = Link {
                sublink,
                kind: LinkKind::Central,
                side: LinkSide::B,
                link_state: Some(state),
            };
            let router = if i == 1 {
                // The bootstrap portal (side B of the initial central link).
                Router::new_terminal(link)
            } else {
                let mut r = Router::bare();
                r.outward.set_primary_link(link);
                r
            };
            // SetOutwardLink marks a central link stable when the router has
            // no decaying links; the acceptor is side B of initial links.
            self.memory()?.set_side_stable(state, false)?;
            self.owners.insert(sublink, sublink);
            self.routers.insert(sublink, router);
        }
        // The internal portal 0 carries the shared-memory-service client
        // handshake (`BaseSharedMemoryService::CreateClient`): a boxed
        // Transport endpoint is `Put` on portal 0, then the portal closes.
        // Reproduced byte-exactly (golden fixture `acceptor-to-broker.bin`
        // messages 1-2) so the routing wire capture matches the baseline.
        self.send_shared_memory_client()?;
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
        loop {
            let msg = self
                .channel
                .recv()?
                .ok_or(RoutingError::Unexpected("peer closed during exchange"))?;
            let decoded = messages::decode_message(&msg.payload, msg.fds.len())?;
            if pred(&decoded) {
                return Ok((decoded, msg.fds));
            }
            self.dispatch(decoded, msg.fds)?;
        }
    }

    /// Dispatch one message to the routers.
    fn dispatch(&mut self, decoded: DecodedMessage, fds: Vec<OwnedFd>) -> Result<(), RoutingError> {
        match decoded {
            DecodedMessage::ConnectFromBrokerToNonBroker(_) => {
                Err(RoutingError::Unexpected("Connect received after handshake"))
            }
            DecodedMessage::ConnectFromNonBrokerToBroker(_) => Err(RoutingError::Unexpected(
                "Connect reply received from broker",
            )),
            DecodedMessage::AddBlockBuffer(b) => {
                if b.buffer_index as usize >= fds.len() {
                    return Err(RoutingError::BadParcel("AddBlockBuffer index out of range"));
                }
                let fd = fds[b.buffer_index as usize].try_dup()?;
                self.memory_mut()?.add_block_buffer(
                    b.buffer_id,
                    fd.into_raw_fd(),
                    b.block_size as usize,
                )?;
                Ok(())
            }
            DecodedMessage::AcceptParcel(p) => self.process_accept_parcel(p, fds).map(|_| ()),
            DecodedMessage::AcceptParcelDriverObjects(_) => Err(RoutingError::Unsupported(
                MSG_ID_ACCEPT_PARCEL,
                "split parcels not exercised by the routing court",
            )),
            DecodedMessage::RouteClosed(rc) => {
                let Some(rid) = self.owners.get(&rc.sublink).copied() else {
                    // Deactivated sublink: the official `GetRouter` returns
                    // null and the message is silently ignored.
                    return Ok(());
                };
                self.router_route_closed(rid, rc.sublink, rc.sequence_length)
            }
            DecodedMessage::RouteDisconnected(rd) => {
                let Some(rid) = self.owners.get(&rd.sublink).copied() else {
                    return Ok(());
                };
                self.router_disconnected(rid)
            }
            DecodedMessage::BypassPeerWithLink(b) => self.on_bypass_peer_with_link(b),
            DecodedMessage::StopProxying(s) => {
                let Some(rid) = self.owners.get(&s.sublink).copied() else {
                    return Ok(());
                };
                self.router_stop_proxying(
                    rid,
                    s.inbound_sequence_length,
                    s.outbound_sequence_length,
                )
            }
            DecodedMessage::StopProxyingToLocalPeer(_) => {
                // The official router ignores this when its decaying link has
                // no local peer (always the case here); a disconnected router
                // silently accepts it.
                Ok(())
            }
            DecodedMessage::FlushRouter(f) => {
                if let Some(&rid) = self.owners.get(&f.sublink) {
                    self.router_flush(rid)?;
                }
                Ok(())
            }
            DecodedMessage::RequestMemory(_) => Err(RoutingError::Unsupported(
                64,
                "RequestMemory not supported by the routing acceptor",
            )),
            DecodedMessage::ProvideMemory(_) => Err(RoutingError::Unsupported(
                65,
                "ProvideMemory not supported by the routing acceptor",
            )),
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
    fn process_accept_parcel(
        &mut self,
        p: AcceptParcel,
        fds: Vec<OwnedFd>,
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
        let payload = self.parcel_data(&p)?;
        let identities = self.deserialize_portals(&p)?;
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

        let rid = match self.owners.get(&p.sublink) {
            Some(&rid) => rid,
            None => {
                self.early_parcels
                    .entry(p.sublink)
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
                .is_some_and(|l| l.sublink == p.sublink)
                || router
                    .outward
                    .decaying_link()
                    .is_some_and(|l| l.sublink == p.sublink);
            if !on_outward {
                let on_inward = router.inward.as_ref().is_some_and(|e| {
                    e.primary.as_ref().is_some_and(|l| l.sublink == p.sublink)
                        || e.decaying_link().is_some_and(|l| l.sublink == p.sublink)
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
        self.router_flush(rid)?;
        Ok(ProcessedParcel {
            payload,
            identities,
        })
    }

    /// `Router::Deserialize`: create a new router from a descriptor received
    /// over the NodeLink. Returns the new router's identity sublink.
    fn router_deserialize(&mut self, d: RouterDescriptor) -> Result<u64, RoutingError> {
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
            self.memory()?.fragment(d.new_link_state_fragment)?;
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
            router.outward.set_primary_link(Link {
                sublink: decaying_sublink,
                kind: LinkKind::PeripheralOutward,
                side: LinkSide::B,
                link_state: None,
            });
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
            router.outward.set_primary_link(Link {
                sublink: d.new_sublink,
                kind: LinkKind::Central,
                side: LinkSide::B,
                link_state,
            });
        } else {
            router.outward.set_primary_link(Link {
                sublink: d.new_sublink,
                kind: LinkKind::PeripheralOutward,
                side: LinkSide::B,
                link_state: None,
            });
        }

        self.owners.insert(d.new_sublink, d.new_sublink);
        if let Some(decaying) = new_decaying_sublink {
            self.owners.insert(decaying, d.new_sublink);
        }
        self.routers.insert(d.new_sublink, router);

        // Accept early parcels for the new sublink.
        if let Some(queued) = self.early_parcels.remove(&d.new_sublink) {
            for p in queued {
                self.process_accept_parcel(p, Vec::new())?;
            }
        }

        // If the source router rolled peer-bypass details into the descriptor,
        // begin bypassing the proxy now.
        if d.proxy_peer_node_name.is_valid() {
            let rid = d.new_sublink;
            let target_node = d.proxy_peer_node_name;
            let target_sublink = d.proxy_peer_sublink;
            self.router_bypass_peer(rid, target_node, target_sublink)?;
        }

        self.router_flush(d.new_sublink)?;
        Ok(d.new_sublink)
    }

    /// `Router::BypassPeer` (the immediate-bypass path from a descriptor): the
    /// requestor is our peripheral outward peer; its outward peer lives on our
    /// own node's link target — here, always the broker — so the bypass is
    /// completed by the broker's side and we only validate the request source.
    fn router_bypass_peer(
        &mut self,
        rid: u64,
        _bypass_target_node: NodeName,
        _bypass_target_sublink: u64,
    ) -> Result<(), RoutingError> {
        // On this node, a descriptor's `proxy_peer_*` fields identify the
        // broker (the only peer), and the broker completes the bypass with a
        // BypassPeerWithNewLocalLink on its side, ending with a StopProxying
        // message to our proxy. There is nothing for us to send; we wait for
        // the StopProxying that follows.
        let router = self
            .routers
            .get(&rid)
            .ok_or(RoutingError::BadParcel("bypass for unknown router"))?;
        if router.outward.primary.is_none() {
            return Err(RoutingError::BadParcel("bypass requestor is not our peer"));
        }
        Ok(())
    }

    /// `Router::SerializeNewRouterAndConfigureProxy`: serialize this router so
    /// a new router can back its portal on the remote node; this router stays
    /// behind as a proxy.
    fn serialize_router(&mut self, rid: u64) -> Result<(u64, RouterDescriptor), RoutingError> {
        // Lock the central link for proxy bypass, if possible. The lock also
        // records the allowed bypass request source (the remote peer on the
        // locked link), matching `RemoteRouterLink::TryLockForBypass`.
        let locked_state = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("serialize for unknown router"))?;
            let mut out = None;
            if let Some(primary) = &router.outward.primary {
                if primary.kind.is_central() {
                    if let Some(state) = primary.link_state {
                        if self
                            .memory()?
                            .try_lock_link_state(state, primary.side.is_a())?
                        {
                            out = Some(state);
                        }
                    }
                }
            }
            out
        };
        if let Some(state) = locked_state {
            // The bypass request source is the remote node of the locked link
            // (the only peer of this NodeLink).
            let source = self.broker_name.low;
            self.memory_mut()?
                .write_allowed_bypass_source(state, source)?;
        }
        let initiate_proxy_bypass = locked_state.is_some();

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
            if router.inward.is_some() {
                return Err(RoutingError::BadParcel("serializing a proxy"));
            }
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
                let primary = router
                    .outward
                    .primary
                    .clone()
                    .ok_or(RoutingError::BadParcel("no outward primary"))?;
                descriptor.proxy_peer_node_name = self.broker_name;
                descriptor.proxy_peer_sublink = primary.sublink;
                if let Some(inward) = router.inward.as_mut() {
                    inward.begin_primary_link_decay();
                }
                router.outward.begin_primary_link_decay();
            }
        }
        // Register the new peripheral inward link on the NodeLink; the router
        // adopts it in begin_proxying after the descriptor is transmitted.
        self.owners.insert(new_sublink, rid);
        Ok((rid, descriptor))
    }

    /// `Router::BeginProxyingToNewRouter`: after the descriptor was
    /// transmitted, adopt the peripheral inward link.
    fn begin_proxying(&mut self, rid: u64, d: &RouterDescriptor) -> Result<(), RoutingError> {
        let new_sublink = d.new_sublink;
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
                inward.set_primary_link(Link {
                    sublink: new_sublink,
                    kind: LinkKind::PeripheralInward,
                    side: LinkSide::A,
                    link_state: None,
                });
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
            self.router_flush(rid)?;
        }
        Ok(())
    }

    /// `Router::Flush`: forward parcels, finish decays, deliver to the portal,
    /// propagate closure, and drop dead proxies.
    fn router_flush(&mut self, rid: u64) -> Result<(), RoutingError> {
        // Collect parcels to transmit (outward then inward forwarding).
        let outbound = self
            .routers
            .get_mut(&rid)
            .map(|r| r.collect_outbound())
            .unwrap_or_default();
        for (sublink, parcel) in outbound {
            self.transmit_parcel(sublink, parcel)?;
        }
        let inward = self
            .routers
            .get_mut(&rid)
            .map(|r| r.collect_inbound())
            .unwrap_or_default();
        for (sublink, parcel) in inward {
            self.transmit_parcel(sublink, parcel)?;
        }

        // Finish decays (releasing the decaying links).
        let _ = self.routers.get_mut(&rid).map(|r| r.finish_decays());

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

        // Mark the central link stable when both edges are stable.
        let mark_stable = {
            let router = self
                .routers
                .get(&rid)
                .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
            if router.on_central_link() && router.outward.is_stable() {
                let inward_stable = match &router.inward {
                    Some(e) => e.is_stable(),
                    None => true,
                };
                if inward_stable {
                    router.outward.primary.as_ref().and_then(|l| l.link_state)
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(state) = mark_stable {
            let side_a = self
                .routers
                .get(&rid)
                .and_then(|r| r.outward.primary.as_ref())
                .is_some_and(|l| l.side.is_a());
            self.memory()?.set_side_stable(state, side_a)?;
        }

        // Closure propagation.
        let mut route_closed: Option<(u64, u64)> = None;
        let mut forward_closed: Option<(u64, u64)> = None;
        let mut try_lock: Option<(u64, bool, FragmentDescriptor, u64)> = None;
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("flush for unknown router"))?;
            let on_central = router.on_central_link();
            let outbound_done = router.outbound.is_sequence_fully_consumed();
            let inbound_expects_more = router.inbound.expects_more_elements();
            let inbound_consumed = router.inbound.is_sequence_fully_consumed();

            if on_central && outbound_done {
                if let Some(primary) = &router.outward.primary {
                    if let Some(state) = primary.link_state {
                        if let Some(final_len) = router.outbound.final_sequence_length() {
                            try_lock =
                                Some((primary.sublink, primary.side.is_a(), state, final_len));
                        }
                    } else if let Some(final_len) = router.outbound.final_sequence_length() {
                        route_closed = Some((primary.sublink, final_len));
                        router.outward.release_primary_link();
                    }
                }
            } else if !inbound_expects_more {
                router.outward.release_primary_link();
            }
            if inbound_consumed {
                if let Some(final_len) = router.inbound.final_sequence_length() {
                    if let Some(inward) = &mut router.inward {
                        if let Some(link) = inward.release_primary_link() {
                            forward_closed = Some((link.sublink, final_len));
                        }
                    }
                }
            }
        }
        if let Some((sublink, side_a, state, final_len)) = try_lock {
            if self.memory()?.try_lock_link_state(state, side_a)? {
                route_closed = Some((sublink, final_len));
                self.routers
                    .get_mut(&rid)
                    .map(|r| r.outward.release_primary_link());
            }
        }
        if let Some((sublink, len)) = route_closed {
            self.send_link_message(messages::encode_route_closed(sublink, len))?;
        }
        if let Some((sublink, len)) = forward_closed {
            self.send_link_message(messages::encode_route_closed(sublink, len))?;
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
        self.router_flush(rid)
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
        self.router_flush(rid)
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
        self.router_flush(rid)
    }

    /// `Router::AcceptBypassLink`-style handling of `BypassPeerWithLink`
    /// (the broker's bootstrap-route bypass): adopt the new central link and
    /// begin decaying the old one.
    fn on_bypass_peer_with_link(
        &mut self,
        b: messages::BypassPeerWithLink,
    ) -> Result<(), RoutingError> {
        if b.new_link_state_fragment.is_null() {
            return Err(RoutingError::BadParcel("bypass with null link state"));
        }
        self.memory()?.fragment(b.new_link_state_fragment)?;

        let length_to_proxy_from_us;
        let received_on_old;
        {
            let router = self
                .routers
                .get_mut(&b.sublink)
                .ok_or(RoutingError::BadParcel("bypass for unknown sublink"))?;
            let _old = router
                .outward
                .primary
                .clone()
                .ok_or(RoutingError::BadParcel("bypass without outward link"))?;
            length_to_proxy_from_us = router.outbound.current_sequence_number();
            received_on_old = router.inbound.current_sequence_number();
            router.outward.begin_primary_link_decay();
            router
                .outward
                .set_length_to_decaying_link(length_to_proxy_from_us);
            router
                .outward
                .set_length_from_decaying_link(b.inbound_sequence_length);
            router.outward.set_primary_link(Link {
                sublink: b.new_sublink,
                kind: LinkKind::Central,
                side: LinkSide::B,
                link_state: Some(b.new_link_state_fragment),
            });
            self.owners.insert(b.new_sublink, b.sublink);
        }

        // The new link goes to the same node as the old one: tell the peer to
        // stop proxying on the old sublink.
        self.send_link_message(messages::encode_stop_proxying_to_local_peer(
            b.sublink,
            length_to_proxy_from_us,
        ))?;

        // If the decaying link already received its full sequence, drop it and
        // mark our side stable.
        if received_on_old >= b.inbound_sequence_length {
            if let Some(router) = self.routers.get_mut(&b.sublink) {
                router.outward.release_decaying_link();
            }
            self.memory()?
                .set_side_stable(b.new_link_state_fragment, false)?;
        }
        // Drain early parcels for the new sublink.
        if let Some(queued) = self.early_parcels.remove(&b.new_sublink) {
            for p in queued {
                self.process_accept_parcel(p, Vec::new())?;
            }
        }
        Ok(())
    }

    /// Deserialize the new routers described by an AcceptParcel's
    /// `new_routers` array; returns their identity sublinks in order.
    fn deserialize_portals(&mut self, p: &AcceptParcel) -> Result<Vec<u64>, RoutingError> {
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
            let rid = self.router_deserialize(d)?;
            out.push(rid);
        }
        Ok(out)
    }

    /// Transmit one parcel on a sublink, serializing any attached portals.
    fn transmit_parcel(&mut self, sublink: u64, parcel: Parcel) -> Result<(), RoutingError> {
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
                    serialized.push(self.serialize_router(*rid)?);
                }
            }
        }
        let new_routers: Vec<Vec<u8>> = serialized.iter().map(|(_, d)| d.encode()).collect();
        let msg = messages::encode_accept_parcel_with_portals(
            sublink,
            parcel.sequence_number,
            &parcel.data,
            &handle_types,
            &new_routers,
            &[],
        );
        self.send_link_message(msg)?;
        for (rid, d) in serialized {
            self.begin_proxying(rid, &d)?;
        }
        Ok(())
    }

    /// `Router::Put` from a portal: enqueue an outbound parcel and flush.
    fn put(&mut self, rid: u64, data: Vec<u8>, objects: Vec<Object>) -> Result<(), RoutingError> {
        {
            let router = self
                .routers
                .get_mut(&rid)
                .ok_or(RoutingError::BadParcel("put for unknown router"))?;
            let seq = router.outbound.current_sequence_number();
            let parcel = Parcel {
                sequence_number: seq,
                data,
                objects,
            };
            if !router.push_outbound(parcel) {
                return Err(RoutingError::BadParcel("outbound sequence regression"));
            }
        }
        self.router_flush(rid)
    }

    /// Send a NodeLink message, assigning the per-link sequence number.
    fn send_link_message(&mut self, mut payload: Vec<u8>) -> Result<(), RoutingError> {
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, &[])?;
        Ok(())
    }

    /// Remove a router and all its sublinks. The owners map is swept for any
    /// entry referencing the router, so a deactivated router's sublinks behave
    /// like the official `RemoteRouterLink::Deactivate` (inbound messages on
    /// them are dropped, not errors).
    fn remove_router(&mut self, rid: u64) {
        let sublinks: Vec<u64> = {
            let Some(router) = self.routers.get(&rid) else {
                return;
            };
            let mut out = Vec::new();
            if let Some(l) = &router.outward.primary {
                out.push(l.sublink);
            }
            if let Some(l) = router.outward.decaying_link() {
                out.push(l.sublink);
            }
            if let Some(inward) = &router.inward {
                if let Some(l) = &inward.primary {
                    out.push(l.sublink);
                }
                if let Some(l) = inward.decaying_link() {
                    out.push(l.sublink);
                }
            }
            out
        };
        for s in sublinks {
            self.owners.remove(&s);
        }
        // Sweep stale owners entries (released links, e.g. a fully decayed
        // peripheral link whose sublink was already released from the edge).
        let stale: Vec<u64> = self
            .owners
            .iter()
            .filter(|(_, owner)| **owner == rid)
            .map(|(s, _)| *s)
            .collect();
        for s in stale {
            self.owners.remove(&s);
        }
        self.routers.remove(&rid);
    }

    /// Drain all currently available messages (used before sending, so
    /// in-flight routing messages are processed first).
    fn drain_available(&mut self) -> Result<(), RoutingError> {
        loop {
            match self.channel.recv_available()? {
                RecvResult::Message(m) => {
                    let decoded = messages::decode_message(&m.payload, m.fds.len())?;
                    self.dispatch(decoded, m.fds)?;
                }
                RecvResult::WouldBlock => return Ok(()),
                RecvResult::PeerClosed => {
                    return Err(RoutingError::Unexpected("peer closed"));
                }
            }
        }
    }
}
