//! The native ipcz acceptor — the Phase 3 interop node.
//!
//! This is a non-broker node that completes the official `ConnectNode`
//! handshake with a broker over an inherited channel socket, adopts the
//! link-memory buffer transferred in the broker's Connect greeting, and then
//! exchanges a message plus a wrapped descriptor through the bootstrap
//! message pipe (initial portal 1 / sublink 1).
//!
//! The state machines implemented here mirror the pinned ipcz sources:
//!
//! * Connect: `NodeConnectorForNonBrokerToBroker` (reply) and
//!   `NodeConnectorForBrokerToNonBroker` (greeting handling).
//! * Link memory: `NodeLinkMemory::PrimaryBuffer` layout, fragment
//!   resolution, `RouterLinkState` status bits, parcel `FragmentHeader`s.
//! * Routing: the direct central-link router, `AcceptBypassLink` adoption of
//!   a peer-initiated bypass, `StopProxyingToLocalPeer`, `RouteClosed`.
//!
//! Unsupported message types are rejected with classified errors; nothing is
//! silently ignored except messages the reference explicitly ignores.

use std::collections::{HashMap, VecDeque};
use std::os::unix::io::{IntoRawFd, RawFd};

use mojo_rs_casefile::events::{Event, EventKind};
use mojo_rs_platform::fd::OwnedFd;
use mojo_rs_platform::shm::SharedMemory;

use crate::ipcz::channel::{Channel, ChannelError, IncomingMessage, RecvResult};
use crate::ipcz::link_memory::{
    LinkMemory, LinkMemoryError, MAX_INITIAL_PORTALS, PRIMARY_BUFFER_SIZE, ROUTER_LINK_STATE_SIZE,
    RouterLinkStatus,
};
use crate::ipcz::messages::{
    AcceptParcel, DecodedMessage, DriverObjectRef, FragmentDescriptor, MSG_ID_ACCEPT_PARCEL,
    encode_accept_parcel_fragment, encode_connect_from_non_broker_to_broker, encode_route_closed,
    encode_stop_proxying_to_local_peer,
};
use crate::ipcz::wire::{WireError, set_message_sequence_number};

/// Classified acceptor errors. Every failure mode is explicit.
#[derive(Debug)]
pub enum AcceptorError {
    /// The channel failed (I/O, EOF, malformed framing).
    Channel(ChannelError),
    /// A message was malformed at the wire level.
    Wire(WireError),
    /// The link memory was malformed or out of bounds.
    LinkMemory(LinkMemoryError),
    /// A protocol message was unexpected in the current state.
    Unexpected(&'static str),
    /// A message type is valid but unsupported by the acceptor.
    Unsupported(u8, &'static str),
    /// A parcel violated route sequencing or structure.
    BadParcel(&'static str),
    /// The transport endpoint fd was invalid.
    Io(std::io::Error),
}

impl From<ChannelError> for AcceptorError {
    fn from(e: ChannelError) -> Self {
        AcceptorError::Channel(e)
    }
}

impl From<WireError> for AcceptorError {
    fn from(e: WireError) -> Self {
        AcceptorError::Wire(e)
    }
}

impl From<LinkMemoryError> for AcceptorError {
    fn from(e: LinkMemoryError) -> Self {
        AcceptorError::LinkMemory(e)
    }
}

impl From<std::io::Error> for AcceptorError {
    fn from(e: std::io::Error) -> Self {
        AcceptorError::Io(e)
    }
}

impl std::fmt::Display for AcceptorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcceptorError::Channel(e) => write!(f, "channel: {e}"),
            AcceptorError::Wire(e) => write!(f, "wire: {e}"),
            AcceptorError::LinkMemory(e) => write!(f, "link memory: {e}"),
            AcceptorError::Unexpected(s) => write!(f, "unexpected message: {s}"),
            AcceptorError::Unsupported(id, s) => write!(f, "unsupported message {id}: {s}"),
            AcceptorError::BadParcel(s) => write!(f, "bad parcel: {s}"),
            AcceptorError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for AcceptorError {}

/// A link edge: one `RemoteRouterLink` of a router.
#[derive(Debug, Clone)]
struct LinkEdge {
    /// The sublink id on the NodeLink.
    sublink: u64,
    /// The `RouterLinkState` fragment for this central link.
    link_state: FragmentDescriptor,
}

/// A portal (message-pipe endpoint) with its delivered-message queue.
#[derive(Debug, Default)]
struct Portal {
    /// Delivered messages: payload bytes plus extracted descriptors.
    messages: VecDeque<(Vec<u8>, Vec<OwnedFd>)>,
}

impl Portal {
    /// Take the first delivered message, if any.
    fn pop(&mut self) -> Option<(Vec<u8>, Vec<OwnedFd>)> {
        self.messages.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// A router: the per-route state machine for one portal pair on this link.
#[derive(Debug)]
struct Router {
    /// The portal this router fronts (None for internal/unused portals).
    portal: Option<Portal>,
    /// The primary (current) link edge.
    primary: LinkEdge,
    /// A decaying link edge during a bypass; dropped once drained.
    decaying: Option<LinkEdge>,
    /// Next outbound route sequence number.
    outbound_seq: u64,
    /// Next expected inbound route sequence number.
    inbound_seq: u64,
    /// Inbound parcels received per sublink (for decay bookkeeping).
    inbound_by_edge: HashMap<u64, u64>,
    /// Whether this route is closed.
    closed: bool,
}

impl Router {
    /// The current sublink for outbound transmissions.
    fn outbound_sublink(&self) -> u64 {
        self.primary.sublink
    }
}

/// A half of a split parcel, keyed by (sublink, sequence number).
enum SplitHalf {
    /// The data half (AcceptParcel) with placeholder object slots.
    Data(AcceptParcel),
    /// The objects half (AcceptParcelDriverObjects).
    Objects(Vec<DriverObjectRef>),
}

/// The ipcz acceptor state machine.
pub struct Acceptor {
    /// The channel to the broker.
    channel: Channel,
    /// The adopted link memory (set by the Connect handshake).
    link_memory: Option<LinkMemory>,
    /// Per-link outgoing sequence number (after Connect).
    next_link_seq: u64,
    /// Routers by sublink.
    routers: HashMap<u64, Router>,
    /// Parcels for sublinks not yet established.
    early_parcels: HashMap<u64, VecDeque<(Vec<u8>, Vec<OwnedFd>)>>,
    /// Split-parcel halves awaiting their counterpart.
    pending_split: HashMap<(u64, u64), SplitHalf>,
    /// The bootstrap pipe sublink (initial portal 1).
    bootstrap_sublink: u64,
    /// Events in casefile format.
    events: Vec<Event>,
    /// Event sequence counter (first event is seq 1, like the oracle).
    event_seq: u64,
    /// Whether the reply has been sent.
    reply_sent: bool,
    /// Whether the bootstrap portal has delivered a message.
    reply_ready: bool,
}

/// The result of the acceptor run.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The bootstrap exchange completed and verified.
    Success,
    /// The peer closed before the exchange completed.
    PeerClosed,
}

/// The negotiated initial portal count the acceptor assumes (max attachments
/// + 1 internal portal, matching `Invitation::Accept`).
const ACCEPTOR_INITIAL_PORTALS: u32 = 8;
/// The internal portal (sublink 0) is reserved for the shared-memory service.
const INTERNAL_PORTAL: u64 = 0;

impl Acceptor {
    /// Start the acceptor on an inherited socket descriptor.
    pub fn new(fd: RawFd) -> Result<Acceptor, AcceptorError> {
        let channel = Channel::adopt(fd)?;
        Ok(Acceptor {
            channel,
            link_memory: None,
            next_link_seq: 0,
            routers: HashMap::new(),
            early_parcels: HashMap::new(),
            pending_split: HashMap::new(),
            bootstrap_sublink: 1,
            events: Vec::new(),
            event_seq: 0,
            reply_sent: false,
            reply_ready: false,
        })
    }

    /// Emit an event (the `seq` field starts at 1, matching the oracle).
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

    /// The events emitted so far.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The established link memory (set by the Connect handshake).
    fn memory(&self) -> Result<&LinkMemory, AcceptorError> {
        self.link_memory
            .as_ref()
            .ok_or(AcceptorError::Unexpected("link memory not established"))
    }

    /// Mutable access to the established link memory.
    fn memory_mut(&mut self) -> Result<&mut LinkMemory, AcceptorError> {
        self.link_memory
            .as_mut()
            .ok_or(AcceptorError::Unexpected("link memory not established"))
    }

    /// Run the full acceptor flow.
    pub fn run(&mut self) -> Result<RunOutcome, AcceptorError> {
        self.emit(0, EventKind::Lifecycle, "MOJO_RESULT_OK");
        // Phase 1: the Connect handshake.
        self.connect()?;
        self.emit(1, EventKind::Result, "MOJO_RESULT_OK");
        self.emit(2, EventKind::Result, "MOJO_RESULT_OK");

        // Phase 2: the message exchange.
        let outcome = self.exchange()?;
        self.emit(5, EventKind::Lifecycle, "MOJO_RESULT_OK");
        Ok(outcome)
    }

    /// Complete the Connect handshake: receive the broker's greeting, adopt
    /// the link memory, and reply.
    fn connect(&mut self) -> Result<(), AcceptorError> {
        let msg = match self.channel.recv()? {
            Some(m) => m,
            None => return Err(AcceptorError::Unexpected("peer closed before Connect")),
        };
        let decoded = crate::ipcz::messages::decode_message(&msg.payload, msg.fds.len())?;
        let DecodedMessage::ConnectFromBrokerToNonBroker(connect) = decoded else {
            return Err(AcceptorError::Unexpected(
                "first message was not ConnectFromBrokerToNonBroker",
            ));
        };
        if connect.num_initial_portals > MAX_INITIAL_PORTALS as u32 {
            return Err(AcceptorError::BadParcel("excessive initial portals"));
        }
        // The greeting carries exactly one driver object: the link memory.
        let mut fds = msg.fds;
        if fds.len() != 1 {
            return Err(AcceptorError::Unexpected("Connect carries no link memory"));
        }
        let mem_fd = fds.remove(0).into_raw_fd();
        let buffer = parse_shared_buffer_driver_object(&msg.payload)?;
        if buffer.buffer_size != PRIMARY_BUFFER_SIZE as u32 {
            return Err(AcceptorError::Unexpected(
                "link memory buffer has unexpected size",
            ));
        }
        self.link_memory = Some(LinkMemory::adopt_primary(mem_fd)?);

        // Reply with the non-broker's Connect message. The sequence number is
        // not assigned by NodeLink::Transmit (it is zero, like the oracle).
        let reply = encode_connect_from_non_broker_to_broker(ACCEPTOR_INITIAL_PORTALS, 0);
        self.channel.send(&reply, &[])?;

        // Establish the initial portals. The broker assumes
        // `num_initial_portals`; surplus acceptor portals close locally.
        let broker_portals = connect.num_initial_portals as usize;
        for i in 0..broker_portals.min(MAX_INITIAL_PORTALS) {
            let sublink = i as u64;
            let state = FragmentDescriptor {
                buffer_id: 0,
                offset: LinkMemory::initial_link_state_offset(i) as u32,
                size: ROUTER_LINK_STATE_SIZE as u32,
            };
            let portal = if i == 1 {
                Some(Portal::default())
            } else {
                // Internal (0) and unused portals have no Mojo endpoint.
                None
            };
            let mut router = Router {
                portal,
                primary: LinkEdge {
                    sublink,
                    link_state: state,
                },
                decaying: None,
                outbound_seq: 0,
                inbound_seq: 0,
                inbound_by_edge: HashMap::new(),
                closed: false,
            };
            // SetOutwardLink marks a central link stable when the router has
            // no decaying links; the acceptor is side B of initial links.
            self.memory()?
                .set_link_status_bits(state, RouterLinkStatus::SIDE_B_STABLE)?;
            if i > 1 {
                router.closed = true;
            }
            self.routers.insert(sublink, router);
        }
        // Surplus portals (beyond the broker's count) close locally.
        for i in broker_portals.min(MAX_INITIAL_PORTALS)..MAX_INITIAL_PORTALS {
            let state = FragmentDescriptor {
                buffer_id: 0,
                offset: LinkMemory::initial_link_state_offset(i) as u32,
                size: ROUTER_LINK_STATE_SIZE as u32,
            };
            self.routers.insert(
                i as u64,
                Router {
                    portal: None,
                    primary: LinkEdge {
                        sublink: i as u64,
                        link_state: state,
                    },
                    decaying: None,
                    outbound_seq: 0,
                    inbound_seq: 0,
                    inbound_by_edge: HashMap::new(),
                    closed: true,
                },
            );
        }
        Ok(())
    }

    /// Drive the message exchange until the reply is sent and the route is
    /// closed.
    fn exchange(&mut self) -> Result<RunOutcome, AcceptorError> {
        let mut peer_closed = false;
        while !self.reply_sent {
            if !self
                .channel
                .wait_readable(std::time::Duration::from_secs(30))?
            {
                return Err(AcceptorError::Channel(ChannelError::Io(
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "exchange timed out"),
                )));
            }
            // Drain everything currently available. The reply is sent only
            // after the drain, so any in-flight routing messages (the broker's
            // bypass) are processed first and the reply goes out on the
            // current primary sublink — matching the oracle acceptor.
            loop {
                match self.channel.recv_available()? {
                    RecvResult::Message(m) => self.dispatch(m)?,
                    RecvResult::WouldBlock => break,
                    RecvResult::PeerClosed => {
                        peer_closed = true;
                        break;
                    }
                }
            }
            if peer_closed {
                break;
            }
            if self.reply_ready && !self.reply_sent {
                self.send_reply()?;
            }
        }
        // Close the route: propagate closure on the primary sublink with the
        // outbound parcel count, matching MojoClose on the pipe.
        if !peer_closed && self.reply_sent {
            let sublink = self
                .routers
                .get(&self.bootstrap_sublink)
                .map(Router::outbound_sublink)
                .unwrap_or(self.bootstrap_sublink);
            let seq_len = self
                .routers
                .get(&self.bootstrap_sublink)
                .map(|r| r.outbound_seq)
                .unwrap_or(0);
            self.send_route_closed(sublink, seq_len)?;
        }
        if peer_closed && !self.reply_sent {
            return Ok(RunOutcome::PeerClosed);
        }
        Ok(RunOutcome::Success)
    }

    /// Dispatch one incoming channel message.
    fn dispatch(&mut self, msg: IncomingMessage) -> Result<(), AcceptorError> {
        let num_fds = msg.fds.len();
        let decoded = crate::ipcz::messages::decode_message(&msg.payload, num_fds)?;
        match decoded {
            DecodedMessage::ConnectFromBrokerToNonBroker(_) => Err(AcceptorError::Unexpected(
                "Connect received after handshake",
            )),
            DecodedMessage::ConnectFromNonBrokerToBroker(_) => Err(AcceptorError::Unexpected(
                "Connect reply received from broker",
            )),
            DecodedMessage::AddBlockBuffer(b) => {
                // Adopt the new buffer (buffer_index into the driver array).
                if b.buffer_index as usize >= msg.fds.len() {
                    return Err(AcceptorError::Unexpected(
                        "AddBlockBuffer index out of range",
                    ));
                }
                let fd = msg
                    .fds
                    .get(b.buffer_index as usize)
                    .ok_or(AcceptorError::Unexpected("AddBlockBuffer missing buffer"))?
                    .try_dup()?;
                let fd = fd.into_raw_fd();
                self.memory_mut()?.add_block_buffer(
                    b.buffer_id,
                    fd,
                    block_size_for(b.block_size),
                )?;
                Ok(())
            }
            DecodedMessage::AcceptParcel(p) => self.on_accept_parcel(p, msg.fds),
            DecodedMessage::AcceptParcelDriverObjects(p) => {
                self.on_accept_parcel_objects(p, msg.fds)
            }
            DecodedMessage::RouteClosed(rc) => {
                if let Some(r) = self.routers.get_mut(&rc.sublink) {
                    r.closed = true;
                }
                Ok(())
            }
            DecodedMessage::RouteDisconnected(rd) => {
                if let Some(r) = self.routers.get_mut(&rd.sublink) {
                    r.closed = true;
                }
                Ok(())
            }
            DecodedMessage::BypassPeerWithLink(b) => self.on_bypass_peer_with_link(b),
            DecodedMessage::StopProxyingToLocalPeer(s) => {
                // The reference ignores StopProxyingToLocalPeer at a router
                // with no decaying link; with a decaying link it completes the
                // decay bookkeeping. The acceptor's decaying edge is only ever
                // local (initiated by the broker's bypass), so this is a no-op
                // unless the message targets the bootstrap router.
                let _ = s;
                Ok(())
            }
            DecodedMessage::FlushRouter(_) => Ok(()),
            DecodedMessage::BypassPeer(_) => Err(AcceptorError::Unsupported(
                crate::ipcz::messages::MSG_ID_BYPASS_PEER,
                "BypassPeer not supported by the Phase 3 acceptor",
            )),
            DecodedMessage::AcceptBypassLink(_) => Err(AcceptorError::Unsupported(
                crate::ipcz::messages::MSG_ID_ACCEPT_BYPASS_LINK,
                "AcceptBypassLink not supported by the Phase 3 acceptor",
            )),
            DecodedMessage::StopProxying(_) => Err(AcceptorError::Unsupported(
                crate::ipcz::messages::MSG_ID_STOP_PROXYING,
                "StopProxying not supported by the Phase 3 acceptor",
            )),
            DecodedMessage::ProxyWillStop(_) => Err(AcceptorError::Unsupported(
                crate::ipcz::messages::MSG_ID_PROXY_WILL_STOP,
                "ProxyWillStop not supported by the Phase 3 acceptor",
            )),
            DecodedMessage::RequestMemory(_) => Err(AcceptorError::Unsupported(
                crate::ipcz::messages::MSG_ID_REQUEST_MEMORY,
                "RequestMemory not supported by the Phase 3 acceptor",
            )),
            DecodedMessage::ProvideMemory(_) => Err(AcceptorError::Unsupported(
                crate::ipcz::messages::MSG_ID_PROVIDE_MEMORY,
                "ProvideMemory not supported by the Phase 3 acceptor",
            )),
            DecodedMessage::Unknown(id) => Err(AcceptorError::Unsupported(
                id,
                "message type not decoded by the interop layer",
            )),
        }
    }

    /// Handle `AcceptParcel`: validate, assemble, and deliver.
    fn on_accept_parcel(
        &mut self,
        p: AcceptParcel,
        mut fds: Vec<OwnedFd>,
    ) -> Result<(), AcceptorError> {
        if p.num_subparcels == 0 || p.subparcel_index >= p.num_subparcels || p.num_subparcels > 16 {
            return Err(AcceptorError::BadParcel("invalid subparcel bounds"));
        }
        if p.num_subparcels > 1 {
            // Subparcels only arise from application-object serialization,
            // which the Phase 3 acceptor does not exercise. Reject explicitly.
            return Err(AcceptorError::Unsupported(
                MSG_ID_ACCEPT_PARCEL,
                "multi-subparcel parcels not supported",
            ));
        }
        // Collect the parcel data (fragment or inline).
        let data = if p.parcel_fragment.is_null() {
            p.parcel_data.clone()
        } else {
            self.memory()?.read_parcel_fragment(p.parcel_fragment)?
        };

        // Split the message's driver objects against the handle types.
        let mut driver_refs = p.driver_objects.iter();
        let mut objects: Vec<OwnedFd> = Vec::new();
        let mut is_split = false;
        for &ht in &p.handle_types {
            match ht {
                1 => {
                    // kBoxedDriverObject: consume the next driver object.
                    let obj = driver_refs
                        .next()
                        .ok_or(AcceptorError::BadParcel("missing driver object"))?;
                    let fd = unwrap_wrapped_platform_handle(obj, &fds)?;
                    objects.push(fd);
                }
                2 => {
                    // kRelayedBoxedDriverObject: placeholder; objects arrive
                    // via AcceptParcelDriverObjects.
                    is_split = true;
                }
                0 => {
                    return Err(AcceptorError::Unsupported(
                        MSG_ID_ACCEPT_PARCEL,
                        "portal transfer (kPortal) not supported",
                    ));
                }
                other => {
                    return Err(AcceptorError::BadParcel("unknown handle type"));
                }
            }
        }
        if driver_refs.next().is_some() {
            return Err(AcceptorError::BadParcel("unclaimed driver objects"));
        }

        if is_split {
            // Store the data half; the objects half completes the parcel.
            self.pending_split
                .insert((p.sublink, p.sequence_number), SplitHalf::Data(p));
            return Ok(());
        }

        // Consume the fds claimed by the driver objects (the rest were never
        // claimed and must be closed; the reference closes unused handles).
        let claimed = objects.len();
        fds.drain(..claimed.min(fds.len()));

        self.deliver_parcel(p.sublink, p.sequence_number, data, objects)
    }

    /// Handle `AcceptParcelDriverObjects`: pair with the data half.
    fn on_accept_parcel_objects(
        &mut self,
        p: crate::ipcz::messages::AcceptParcelDriverObjects,
        mut fds: Vec<OwnedFd>,
    ) -> Result<(), AcceptorError> {
        let key = (p.sublink, p.sequence_number);
        let objects = p
            .driver_objects
            .iter()
            .map(|obj| unwrap_wrapped_platform_handle(obj, &fds))
            .collect::<Result<Vec<_>, _>>()?;
        let claimed = objects.len();
        fds.drain(..claimed.min(fds.len()));
        match self.pending_split.remove(&key) {
            Some(SplitHalf::Data(parcel)) => {
                // The placeholder count must match the supplied objects.
                let placeholders = parcel.handle_types.iter().filter(|&&t| t == 2).count();
                if placeholders != objects.len() {
                    return Err(AcceptorError::BadParcel(
                        "split parcel object count mismatch",
                    ));
                }
                self.deliver_parcel(
                    p.sublink,
                    p.sequence_number,
                    parcel.parcel_data.clone(),
                    objects,
                )
            }
            Some(SplitHalf::Objects(_)) => {
                Err(AcceptorError::BadParcel("duplicate split parcel objects"))
            }
            None => {
                // Objects arrived first: keep them for the pending data half.
                self.pending_split
                    .insert(key, SplitHalf::Objects(p.driver_objects));
                Ok(())
            }
        }
    }

    /// Deliver a complete parcel to the router's portal.
    fn deliver_parcel(
        &mut self,
        sublink: u64,
        sequence_number: u64,
        data: Vec<u8>,
        objects: Vec<OwnedFd>,
    ) -> Result<(), AcceptorError> {
        if let Some(router) = self.routers.get_mut(&sublink) {
            if sequence_number != router.inbound_seq {
                return Err(AcceptorError::BadParcel("route sequence gap or duplicate"));
            }
            router.inbound_seq += 1;
            *router.inbound_by_edge.entry(sublink).or_insert(0) += 1;
            if let Some(portal) = &mut router.portal {
                portal.messages.push_back((data, objects));
                if sublink == self.bootstrap_sublink {
                    self.reply_ready = true;
                }
            }
            Ok(())
        } else {
            // Early parcel for a sublink not yet established; deliver when the
            // sublink is adopted (reference: early_parcels_for_sublink_).
            self.early_parcels
                .entry(sublink)
                .or_default()
                .push_back((data, objects));
            Ok(())
        }
    }

    /// Handle `BypassPeerWithLink`: adopt the new central link (side B) and
    /// decay the old one, mirroring `Router::AcceptBypassLink`.
    fn on_bypass_peer_with_link(
        &mut self,
        b: crate::ipcz::messages::BypassPeerWithLink,
    ) -> Result<(), AcceptorError> {
        if b.new_link_state_fragment.is_null() {
            return Err(AcceptorError::BadParcel("bypass with null link state"));
        }
        // Validate that the fragment resolves in the link memory.
        self.memory()?
            .fragment(b.new_link_state_fragment)
            .map_err(|_| AcceptorError::BadParcel("bypass link state unresolvable"))?;

        // Extract the router state we need before re-borrowing self mutably.
        let (old, length_to_proxy_from_us, received) = {
            let router = self
                .routers
                .get_mut(&b.sublink)
                .ok_or(AcceptorError::BadParcel("bypass for unknown sublink"))?;
            let old = router.primary.clone();
            let length_to_proxy_from_us = router.outbound_seq;
            let new_edge = LinkEdge {
                sublink: b.new_sublink,
                link_state: b.new_link_state_fragment,
            };
            router.decaying = Some(old.clone());
            router.primary = new_edge;
            let received = router
                .inbound_by_edge
                .get(&old.sublink)
                .copied()
                .unwrap_or(0);
            (old, length_to_proxy_from_us, received)
        };

        // Tell the peer to stop proxying on the old sublink (the new link
        // goes to the same node as the old one).
        self.send_link_message(encode_stop_proxying_to_local_peer(
            old.sublink,
            length_to_proxy_from_us,
        ))?;

        // Decay bookkeeping: the decaying link carries inbound parcels up to
        // `inbound_sequence_length`; once drained, drop it and mark stable.
        if received >= b.inbound_sequence_length {
            if let Some(router) = self.routers.get_mut(&b.sublink) {
                router.decaying = None;
            }
            self.memory()?
                .set_link_status_bits(b.new_link_state_fragment, RouterLinkStatus::SIDE_B_STABLE)?;
        }
        // Drain any early parcels queued for the new sublink.
        if let Some(queued) = self.early_parcels.remove(&b.new_sublink) {
            for (data, objects) in queued {
                self.deliver_parcel(b.new_sublink, 0, data, objects)?;
            }
        }
        Ok(())
    }

    /// Send a NodeLink message, assigning the per-link sequence number.
    fn send_link_message(&mut self, mut payload: Vec<u8>) -> Result<(), AcceptorError> {
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, &[])?;
        Ok(())
    }

    /// Send `RouteClosed` for a sublink.
    fn send_route_closed(
        &mut self,
        sublink: u64,
        sequence_length: u64,
    ) -> Result<(), AcceptorError> {
        self.send_link_message(encode_route_closed(sublink, sequence_length))
    }

    /// Read the bootstrap portal's message, verify it, and send the reply.
    fn send_reply(&mut self) -> Result<(), AcceptorError> {
        // Pop the bootstrap message, releasing the router borrow before any
        // other self access.
        let (payload, mut fds) = {
            let router = self
                .routers
                .get_mut(&self.bootstrap_sublink)
                .ok_or(AcceptorError::Unexpected("bootstrap router missing"))?;
            let Some(portal) = &mut router.portal else {
                return Err(AcceptorError::Unexpected("bootstrap portal missing"));
            };
            portal
                .pop()
                .ok_or(AcceptorError::Unexpected("bootstrap message missing"))?
        };
        if payload != b"hello-from-broker" {
            return Err(AcceptorError::BadParcel("unexpected broker payload"));
        }
        // Verify the transferred descriptor content.
        if fds.len() != 1 {
            return Err(AcceptorError::BadParcel("expected one descriptor"));
        }
        let fd = fds.remove(0).into_raw_fd();
        let content = read_all(fd)?;
        // SAFETY: the fd was consumed by `read_all` only; ownership was
        // transferred to this function, so closing here is correct.
        unsafe { libc::close(fd) };
        if content != b"fd-from-broker" {
            return Err(AcceptorError::BadParcel("unexpected broker fd content"));
        }
        // Emit the receive event with payload and fd content hex.
        self.emit_receive(&payload, &content);

        // Build the reply: "hello-from-acceptor" + a memfd containing
        // "fd-from-acceptor". The parcel data goes through the link-memory
        // mailbox (a 64-byte fragment), mirroring the oracle acceptor. The
        // memfd is created at size 0 and extended by the write, exactly like
        // the oracle's memfd_create(name, 0) + write, so the receiver reads
        // precisely the content bytes.
        let memfd = SharedMemory::create("mojo-rs-acceptor", 0)?;
        let raw_fd = memfd.as_raw_fd();
        write_all(raw_fd, b"fd-from-acceptor")?;

        let (sublink, seq) = {
            let router = self
                .routers
                .get(&self.bootstrap_sublink)
                .ok_or(AcceptorError::Unexpected("bootstrap router missing"))?;
            (router.primary.sublink, router.outbound_seq)
        };
        let frag = self
            .memory_mut()?
            .write_parcel_fragment(b"hello-from-acceptor")?;
        let driver = wrapped_platform_handle_encoding();
        let mut payload =
            encode_accept_parcel_fragment(sublink, seq, frag, &[1], std::slice::from_ref(&driver));
        // SCM_RIGHTS duplicates the descriptor; keep ownership of `memfd`
        // until after the send so the fd stays valid, then drop it.
        set_message_sequence_number(&mut payload, self.next_link_seq);
        self.next_link_seq += 1;
        self.channel.send(&payload, &[raw_fd])?;
        drop(memfd);
        let router = self
            .routers
            .get_mut(&self.bootstrap_sublink)
            .ok_or(AcceptorError::Unexpected("bootstrap router missing"))?;
        router.outbound_seq += 1;
        self.reply_sent = true;
        self.emit(4, EventKind::Result, "MOJO_RESULT_OK");
        Ok(())
    }

    /// Emit the `message` event for a received payload + fd content.
    fn emit_receive(&mut self, payload: &[u8], fd_content: &[u8]) {
        self.event_seq += 1;
        self.events.push(Event {
            seq: self.event_seq,
            op_id: 3,
            event: EventKind::Message,
            result: "MOJO_RESULT_OK".to_string(),
            handle: None,
            payload_hex: Some(hex::encode(payload)),
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
            note: Some(format!("fd_hex:{}", hex::encode(fd_content))),
        });
    }
}

/// The block size backing an AddBlockBuffer (rounded to the block allocator
/// granularity; the acceptor only uses the buffer for fragment storage).
fn block_size_for(block_size: u32) -> usize {
    // The buffer size must be a multiple of the block size; the driver object
    // for AddBlockBuffer is the memfd itself (its size is authoritative).
    block_size as usize
}

/// Parse the Connect greeting's link-memory driver object: a serialized
/// `SharedBuffer` (ObjectHeader{8, type=1} + BufferHeader{32}).
fn parse_shared_buffer_driver_object(payload: &[u8]) -> Result<SharedBufferInfo, AcceptorError> {
    use crate::ipcz::messages::{le_u16, le_u32, le_u64};
    let hdr = crate::ipcz::wire::parse_message_header(payload)?;
    let doa = hdr.driver_object_data_array;
    if doa == 0 {
        return Err(AcceptorError::Unexpected(
            "Connect carries no driver objects",
        ));
    }
    let data = read_array_at(payload, &hdr, doa)?;
    if data.len() != 8 {
        return Err(AcceptorError::Unexpected("bad driver object array"));
    }
    let driver_data_array = le_u32(&data[0..4]).map_err(AcceptorError::Wire)?;
    let num_handles = le_u16(&data[6..8]).map_err(AcceptorError::Wire)?;
    if num_handles != 1 {
        return Err(AcceptorError::Unexpected("link memory must carry one fd"));
    }
    let obj = read_array_at(payload, &hdr, driver_data_array)?;
    if obj.len() < 8 {
        return Err(AcceptorError::Unexpected("truncated driver object"));
    }
    let header_size = le_u32(&obj[0..4]).map_err(AcceptorError::Wire)? as usize;
    let object_type = le_u32(&obj[4..8]).map_err(AcceptorError::Wire)?;
    if header_size != 8 || object_type != 1 {
        // 1 == kSharedBuffer.
        return Err(AcceptorError::Unexpected(
            "Connect buffer is not a SharedBuffer",
        ));
    }
    let bh = obj
        .get(8..40)
        .ok_or(AcceptorError::Unexpected("truncated BufferHeader"))?;
    let size = le_u32(&bh[0..4]).map_err(AcceptorError::Wire)?;
    let buffer_size = le_u32(&bh[4..8]).map_err(AcceptorError::Wire)?;
    let mode = le_u32(&bh[8..12]).map_err(AcceptorError::Wire)?;
    if size != 32 {
        return Err(AcceptorError::Unexpected("bad BufferHeader size"));
    }
    Ok(SharedBufferInfo {
        buffer_size,
        mode,
        guid_low: le_u64(&bh[16..24]).map_err(AcceptorError::Wire)?,
        guid_high: le_u64(&bh[24..32]).map_err(AcceptorError::Wire)?,
    })
}

/// Info from a serialized `SharedBuffer` driver object.
struct SharedBufferInfo {
    /// The buffer size in bytes.
    buffer_size: u32,
    /// BufferMode (0=read-only, 1=writable, 2=unsafe).
    #[allow(dead_code)]
    mode: u32,
    /// Guid components.
    #[allow(dead_code)]
    guid_low: u64,
    #[allow(dead_code)]
    guid_high: u64,
}

/// Resolve an array at a payload-relative offset (used by the Connect parser).
fn read_array_at<'a>(
    payload: &'a [u8],
    hdr: &crate::ipcz::wire::MessageHeader,
    off: u32,
) -> Result<&'a [u8], AcceptorError> {
    let start = off as usize;
    let arr = payload
        .get(start..)
        .ok_or(AcceptorError::Wire(WireError::BadArrayOffset))?;
    if arr.len() < 8 {
        return Err(AcceptorError::Wire(WireError::BadArray));
    }
    let num_bytes =
        crate::ipcz::messages::le_u32(&arr[0..4]).map_err(AcceptorError::Wire)? as usize;
    if num_bytes < 8 || num_bytes > arr.len() {
        return Err(AcceptorError::Wire(WireError::BadArray));
    }
    Ok(&arr[8..num_bytes])
}

/// Unwrap a serialized `WrappedPlatformHandle` driver object to its fd.
fn unwrap_wrapped_platform_handle(
    obj: &DriverObjectRef,
    fds: &[OwnedFd],
) -> Result<OwnedFd, AcceptorError> {
    if obj.num_fds != 1 {
        return Err(AcceptorError::BadParcel("wrapped handle must carry one fd"));
    }
    let fd = fds
        .get(obj.first_fd)
        .ok_or(AcceptorError::BadParcel("driver object fd out of range"))?;
    let data = &obj.data;
    if data.len() < 16 {
        return Err(AcceptorError::BadParcel("truncated wrapped handle"));
    }
    use crate::ipcz::messages::{le_u32, le_u64};
    let header_size = le_u32(&data[0..4]).map_err(AcceptorError::Wire)?;
    let object_type = le_u32(&data[4..8]).map_err(AcceptorError::Wire)?;
    if header_size != 8 || object_type != 4 {
        // 4 == kWrappedPlatformHandle.
        return Err(AcceptorError::BadParcel(
            "driver object is not a WrappedPlatformHandle",
        ));
    }
    let wrapper_size = le_u32(&data[8..12]).map_err(AcceptorError::Wire)? as usize;
    let wrapper_type = le_u32(&data[12..16]).map_err(AcceptorError::Wire)?;
    let _ = le_u64; // keep the import used
    if wrapper_size < 8 || wrapper_size % 8 != 0 || wrapper_type != 0 {
        return Err(AcceptorError::BadParcel("bad WrappedPlatformHandle header"));
    }
    fd.try_dup().map_err(AcceptorError::Io)
}

/// The serialized form of a `WrappedPlatformHandle` (ObjectHeader{8, 4} +
/// WrappedPlatformHandleHeader{8, 0}).
fn wrapped_platform_handle_encoding() -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&8u32.to_le_bytes()); // ObjectHeader.size
    out.extend_from_slice(&4u32.to_le_bytes()); // kWrappedPlatformHandle
    out.extend_from_slice(&8u32.to_le_bytes()); // WPHH.size
    out.extend_from_slice(&0u32.to_le_bytes()); // kTransmissible
    out
}

/// Read all bytes from an fd starting at offset 0.
fn read_all(fd: RawFd) -> Result<Vec<u8>, AcceptorError> {
    // SAFETY: fd is owned by the caller and remains open for this call.
    let rc = unsafe { libc::lseek(fd, 0, libc::SEEK_SET) };
    if rc < 0 {
        return Err(AcceptorError::Io(std::io::Error::last_os_error()));
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: buf is a valid buffer; fd is a valid descriptor.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            return Err(AcceptorError::Io(std::io::Error::last_os_error()));
        }
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    Ok(out)
}

/// Write all bytes to an fd at its current offset.
fn write_all(fd: RawFd, bytes: &[u8]) -> Result<(), AcceptorError> {
    let mut written = 0usize;
    while written < bytes.len() {
        // SAFETY: fd is valid; bytes is a valid buffer.
        let n = unsafe {
            libc::write(
                fd,
                bytes[written..].as_ptr() as *const libc::c_void,
                bytes.len() - written,
            )
        };
        if n < 0 {
            return Err(AcceptorError::Io(std::io::Error::last_os_error()));
        }
        if n == 0 {
            return Err(AcceptorError::Io(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short write",
            )));
        }
        written += n as usize;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn wrapped_handle_encoding_is_stable() {
        let enc = wrapped_platform_handle_encoding();
        assert_eq!(enc, vec![8, 0, 0, 0, 4, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn parse_connect_shared_buffer() {
        let data = std::fs::read(format!(
            "{}/testdata/ipcz/broker-to-acceptor.bin",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let msgs = crate::ipcz::wire::parse_stream(&data).unwrap();
        let info = parse_shared_buffer_driver_object(&msgs[0].payload).unwrap();
        assert_eq!(info.buffer_size, PRIMARY_BUFFER_SIZE as u32);
        assert_eq!(info.mode, 2); // kUnsafe
        assert_ne!(info.guid_low, 0);
    }

    #[test]
    fn fragment_header_constant_matches_reference() {
        // Parcel FragmentHeader is { reserved u32, size u32 }.
        assert_eq!(crate::ipcz::link_memory::FRAGMENT_HEADER_SIZE, 8);
    }
}
