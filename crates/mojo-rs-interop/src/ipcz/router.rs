//! The ipcz Router state machine (Phase 5).
//!
//! Mirrors the pinned epoch's `ipcz/router.{h,cc}` (Chromium 151.0.7922.105)
//! for the paths a non-broker node exercises: terminal routers on central and
//! peripheral links, proxy routers created by portal serialization, decaying
//! links with sequence-length bounds, parcel forwarding, route closure, proxy
//! bypass completion (`StopProxying`), and the bridge chains formed by
//! `MergeRoute` (`MergePortals`) with their bridge bypass
//! (`MaybeStartBridgeBypass` / `StartBridgeBypassFromLocalPeer`).
//!
//! The candidate acceptor is single-threaded (one poll loop), so a Router
//! carries no internal lock: the owning `Acceptor` serializes every operation,
//! matching the official observable state machine. Each `Router` owns at most
//! three `RouteEdge`s (outward, inward when proxying, and bridge when merged),
//! each with a primary link and optionally one decaying link, plus two
//! `ParcelQueue`s (inbound from the outward side; outbound toward it) with
//! sequence numbers and optional final lengths.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use crate::ipcz::link_memory::RouterLinkStatus;
use crate::ipcz::messages::FragmentDescriptor;

/// A link's role along a route (ipcz `LinkType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// The link connecting one side of the route to the other; the only link
    /// at which decay can be initiated by a router.
    Central,
    /// A link extending the route toward the far portal; forwards messages
    /// outward.
    PeripheralOutward,
    /// A link extending the route toward the local portal; forwards messages
    /// inward.
    PeripheralInward,
    /// A bridge link formed by `MergePortals`: links two terminal routers of
    /// two different routes. Bridge links decay only when both routes are
    /// adjacent to decayable central links, replacing the bridge and both
    /// routes' outer links with a single new central link.
    Bridge,
}

impl LinkKind {
    /// Whether messages arriving on this link are inbound (from the far side).
    pub fn is_outward(self) -> bool {
        matches!(self, LinkKind::Central | LinkKind::PeripheralOutward)
    }

    /// Whether this is a central link.
    pub fn is_central(self) -> bool {
        matches!(self, LinkKind::Central)
    }

    /// Whether this is a bridge link.
    pub fn is_bridge(self) -> bool {
        matches!(self, LinkKind::Bridge)
    }
}

/// Which side of a link this router occupies (ipcz `LinkSide`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSide {
    /// Side A.
    A,
    /// Side B.
    B,
}

impl LinkSide {
    /// The opposite side.
    pub fn opposite(self) -> LinkSide {
        match self {
            LinkSide::A => LinkSide::B,
            LinkSide::B => LinkSide::A,
        }
    }

    /// Whether this is side A.
    pub fn is_a(self) -> bool {
        matches!(self, LinkSide::A)
    }
}

/// The in-process shared state of a local link (the official
/// `LocalRouterLink::SharedState`, which owns a `RouterLinkState`). Both
/// routers on either end of the link share one of these; the acceptor is
/// single-threaded, so the state is a plain (non-atomic) struct guarded by the
/// `RefCell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLinkState {
    /// The `RouterLinkStatus` bits.
    pub status: u32,
    /// The `allowed_bypass_request_source` NodeName (low 64 bits).
    pub allowed_bypass_source: u64,
}

impl LocalLinkState {
    /// A fresh state; `initial_stable` mirrors
    /// `LocalRouterLink::CreatePair`'s `InitialState`: local central links
    /// created by `OpenPortals` are `kStable`; bridge links created by
    /// `MergeRoute` are `kUnstable`.
    pub fn new(initial_stable: bool) -> LocalLinkState {
        LocalLinkState {
            status: if initial_stable {
                RouterLinkStatus::STABLE
            } else {
                RouterLinkStatus::UNSTABLE
            },
            allowed_bypass_source: 0,
        }
    }

    /// The official `RouterLinkState::TryLock`: lock the link when both sides
    /// are stable and unlocked; otherwise set the waiting bit when the other
    /// side is still unstable. This is a direct simulation of the atomic
    /// compare-exchange loop (single-threaded: the status cannot change between
    /// iterations, so the loop terminates after at most two iterations).
    pub fn try_lock(&mut self, side_a: bool) -> bool {
        use RouterLinkStatus as S;
        let (this_stable, other_stable, locked_by_this, locked_either, this_waiting) = if side_a {
            (
                S::SIDE_A_STABLE,
                S::SIDE_B_STABLE,
                S::LOCKED_BY_SIDE_A,
                S::LOCKED_BY_SIDE_A | S::LOCKED_BY_SIDE_B,
                S::SIDE_A_WAITING,
            )
        } else {
            (
                S::SIDE_B_STABLE,
                S::SIDE_A_STABLE,
                S::LOCKED_BY_SIDE_B,
                S::LOCKED_BY_SIDE_A | S::LOCKED_BY_SIDE_B,
                S::SIDE_B_WAITING,
            )
        };
        let mut expected = S::STABLE;
        let mut desired_bit = locked_by_this;
        loop {
            let actual = self.status;
            if actual == expected {
                self.status = actual | desired_bit;
                break;
            }
            expected = actual;
            if (expected & locked_either) != 0 || (expected & this_stable) == 0 {
                return false;
            }
            if desired_bit == locked_by_this && (expected & other_stable) == 0 {
                desired_bit = this_waiting;
            } else if desired_bit == this_waiting && (expected & S::STABLE) == S::STABLE {
                desired_bit = locked_by_this;
            }
        }
        desired_bit == locked_by_this
    }

    /// The official `RouterLinkState::SetSideStable`.
    pub fn set_side_stable(&mut self, side_a: bool) {
        use RouterLinkStatus as S;
        let bit = if side_a {
            S::SIDE_A_STABLE
        } else {
            S::SIDE_B_STABLE
        };
        if self.status & bit == 0 {
            self.status |= bit;
        }
    }

    /// The official `RouterLinkState::Unlock`.
    pub fn unlock(&mut self, side_a: bool) {
        use RouterLinkStatus as S;
        let locked_by_this = if side_a {
            S::LOCKED_BY_SIDE_A
        } else {
            S::LOCKED_BY_SIDE_B
        };
        self.status &= !locked_by_this;
    }

    /// The official `RouterLinkState::ResetWaitingBit`.
    pub fn reset_waiting_bit(&mut self, side_a: bool) -> bool {
        use RouterLinkStatus as S;
        let this_waiting = if side_a {
            S::SIDE_A_WAITING
        } else {
            S::SIDE_B_WAITING
        };
        if (self.status & S::STABLE) != S::STABLE
            || (self.status & this_waiting) == 0
            || (self.status & (S::LOCKED_BY_SIDE_A | S::LOCKED_BY_SIDE_B)) != 0
        {
            return false;
        }
        self.status &= !this_waiting;
        true
    }
}

/// A single link on an edge: identified by its sublink on the NodeLink, or by
/// its local peer router for local links (`LocalRouterLink`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The NodeLink this link belongs to (0 = the broker link; the direct
    /// peer link in the multi-node courts = 1). Local links carry 0.
    pub link_id: u64,
    /// The sublink id on the NodeLink (meaningless for local links).
    pub sublink: u64,
    /// The link's role.
    pub kind: LinkKind,
    /// This router's side of the link.
    pub side: LinkSide,
    /// The `RouterLinkState` fragment (central links only).
    pub link_state: Option<FragmentDescriptor>,
    /// The local peer router identity, for local links (`LocalRouterLink`);
    /// None for remote (sublink-based) links.
    pub local_peer: Option<u64>,
    /// The shared in-process link state for local links; None for remote
    /// links.
    pub local_state: Option<Rc<RefCell<LocalLinkState>>>,
}

impl Link {
    /// Whether this link connects to a local peer router.
    pub fn is_local(&self) -> bool {
        self.local_peer.is_some()
    }

    /// A remote (sublink-based) link on the given NodeLink.
    pub fn remote(
        link_id: u64,
        sublink: u64,
        kind: LinkKind,
        side: LinkSide,
        link_state: Option<FragmentDescriptor>,
    ) -> Link {
        Link {
            link_id,
            sublink,
            kind,
            side,
            link_state,
            local_peer: None,
            local_state: None,
        }
    }

    /// Create one side of a local link pair, mirroring
    /// `LocalRouterLink::CreatePair`: the two returned links share one
    /// `LocalLinkState`. `side_for_first` is the side of the first router
    /// (`rid_a`); the second link takes the opposite side and peers back to
    /// `rid_a`.
    pub fn local_pair(
        kind: LinkKind,
        side_for_first: LinkSide,
        rid_a: u64,
        rid_b: u64,
        initial_stable: bool,
    ) -> (Link, Link) {
        let state = Rc::new(RefCell::new(LocalLinkState::new(initial_stable)));
        let first = Link {
            link_id: 0,
            sublink: 0,
            kind,
            side: side_for_first,
            link_state: None,
            local_peer: Some(rid_b),
            local_state: Some(Rc::clone(&state)),
        };
        let second = Link {
            link_id: 0,
            sublink: 0,
            kind,
            side: side_for_first.opposite(),
            link_state: None,
            local_peer: Some(rid_a),
            local_state: Some(state),
        };
        (first, second)
    }

    /// Whether this link belongs to the outward direction.
    pub fn is_outward(&self) -> bool {
        self.kind.is_outward()
    }
}

/// A decaying link: a former primary link being phased out, restricted to a
/// bounded range of parcel sequence numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecayingLink {
    /// The decaying link (None = decay deferred until a primary is adopted).
    pub link: Option<Link>,
    /// The length of the parcel sequence after which this edge must stop
    /// transmitting on the decaying link.
    pub outgoing_length: Option<u64>,
    /// The length of the parcel sequence after which this edge can stop
    /// expecting to receive parcels on the decaying link.
    pub incoming_length: Option<u64>,
}

/// A `RouteEdge`: the state of one (inward- or outward-facing) side of a
/// router, with at most one primary and one decaying link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edge {
    /// The primary link.
    pub primary: Option<Link>,
    /// The decaying link (or a deferred-decay marker).
    pub decaying: Option<DecayingLink>,
}

impl Edge {
    /// Whether the edge has no decaying link and is not set to decay its next
    /// primary link.
    pub fn is_stable(&self) -> bool {
        self.decaying.is_none()
    }

    /// Whether the edge is marked for deferred decay (a decaying slot with no
    /// link yet).
    pub fn is_decay_deferred(&self) -> bool {
        matches!(&self.decaying, Some(d) if d.link.is_none())
    }

    /// The decaying link, if any.
    pub fn decaying_link(&self) -> Option<&Link> {
        self.decaying.as_ref().and_then(|d| d.link.as_ref())
    }

    /// Set the primary link. Only valid when there is no primary; if the edge
    /// was marked for deferred decay, the new link immediately begins decay.
    pub fn set_primary_link(&mut self, link: Link) {
        debug_assert!(self.primary.is_none());
        match &mut self.decaying {
            // A deferred-decay slot (decaying link not yet adopted) takes the
            // new link as its decaying link immediately.
            Some(d) if d.link.is_none() => d.link = Some(link),
            _ => self.primary = Some(link),
        }
    }

    /// Release and return the primary link.
    pub fn release_primary_link(&mut self) -> Option<Link> {
        self.primary.take()
    }

    /// Release and return the decaying link (resetting the decaying slot).
    pub fn release_decaying_link(&mut self) -> Option<Link> {
        let link = self.decaying.take().and_then(|d| d.link);
        self.decaying = None;
        link
    }

    /// Set the current primary link to begin decay; or, with no primary,
    /// mark the edge for deferred decay. Fails if a decaying link exists.
    pub fn begin_primary_link_decay(&mut self) -> bool {
        if self.decaying.is_some() {
            return false;
        }
        let link = self.primary.take();
        self.decaying = Some(DecayingLink {
            link,
            outgoing_length: None,
            incoming_length: None,
        });
        true
    }

    /// Whether a parcel with the given sequence number should be transmitted
    /// on the decaying link (the official `ShouldTransmitOnDecayingLink`).
    pub fn should_transmit_on_decaying_link(&self, n: u64) -> bool {
        match &self.decaying {
            Some(d) => d.link.is_some() && d.outgoing_length.map_or(true, |len| n < len),
            None => false,
        }
    }

    /// Attempt to drop the decaying link once `length_sent` and
    /// `length_received` have both been reached.
    pub fn maybe_finish_decay(&mut self, length_sent: u64, length_received: u64) -> bool {
        let Some(d) = &self.decaying else {
            return false;
        };
        if d.link.is_none() {
            return false;
        }
        let Some(out) = d.outgoing_length else {
            return false;
        };
        let Some(inc) = d.incoming_length else {
            return false;
        };
        if length_sent < out || length_received < inc {
            return false;
        }
        self.decaying = None;
        true
    }

    /// Set the final length of the sequence transmitted on the decaying link.
    pub fn set_length_to_decaying_link(&mut self, length: u64) {
        if let Some(d) = self.decaying.as_mut() {
            debug_assert!(d.outgoing_length.is_none());
            d.outgoing_length = Some(length);
        }
        // Unreachable in the state machine: callers only set lengths on an
        // edge whose decay has already begun.
    }

    /// The final length of the sequence transmitted on the decaying link, if
    /// set.
    pub fn length_to_decaying_link(&self) -> Option<u64> {
        self.decaying.as_ref().and_then(|d| d.outgoing_length)
    }

    /// Set the final length of the sequence received on the decaying link.
    pub fn set_length_from_decaying_link(&mut self, length: u64) {
        if let Some(d) = self.decaying.as_mut() {
            debug_assert!(d.incoming_length.is_none());
            d.incoming_length = Some(length);
        }
        // Unreachable in the state machine: callers only set lengths on an
        // edge whose decay has already begun.
    }

    /// The final length of the sequence received on the decaying link, if
    /// set.
    pub fn length_from_decaying_link(&self) -> Option<u64> {
        self.decaying.as_ref().and_then(|d| d.incoming_length)
    }

    /// The local peer of the primary link, if it is a local link
    /// (`RouteEdge::GetLocalPeer`).
    pub fn get_local_peer(&self) -> Option<u64> {
        self.primary.as_ref().and_then(|l| l.local_peer)
    }

    /// The local peer of the decaying link, if it is a local link
    /// (`RouteEdge::GetDecayingLocalPeer`).
    pub fn get_decaying_local_peer(&self) -> Option<u64> {
        self.decaying
            .as_ref()
            .and_then(|d| d.link.as_ref())
            .and_then(|l| l.local_peer)
    }
}

/// An object attached to a parcel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Object {
    /// A file descriptor (from a boxed driver object).
    Fd(i32),
    /// A router (portal) — inbound: the deserialized router's identity
    /// sublink; outbound: the router to serialize.
    Router(u64),
}

/// A parcel in flight on a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parcel {
    /// The parcel's sequence number on its route.
    pub sequence_number: u64,
    /// The application payload. When `fragment` is set the payload lives in
    /// the shared-memory fragment (the `data` copy is kept for verification
    /// but is not transmitted); otherwise it is transmitted inline.
    pub data: Vec<u8>,
    /// Attached objects (handles).
    pub objects: Vec<Object>,
    /// The shared-memory fragment holding the payload, when the parcel data
    /// was allocated from the link memory at `Put` time (`Parcel::AllocateData`
    /// with a remote outward link).
    pub fragment: Option<crate::ipcz::messages::FragmentDescriptor>,
}

/// A sequenced queue with out-of-order buffering and an optional final length
/// (mirrors the official `SequencedQueue` for the parcel queues).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParcelQueue {
    /// The next sequence number to emit (outbound) or deliver (inbound).
    current: u64,
    /// The final sequence length (set when the peer end closes).
    final_length: Option<u64>,
    /// Out-of-order arrivals keyed by sequence number.
    buffer: BTreeMap<u64, Parcel>,
}

impl Default for ParcelQueue {
    fn default() -> ParcelQueue {
        ParcelQueue::new()
    }
}

impl ParcelQueue {
    /// A fresh queue starting at sequence 0.
    pub fn new() -> ParcelQueue {
        ParcelQueue {
            current: 0,
            final_length: None,
            buffer: BTreeMap::new(),
        }
    }

    /// Reset the next sequence number (used on deserialization).
    pub fn reset_sequence(&mut self, n: u64) {
        self.current = n;
        self.final_length = None;
        self.buffer.clear();
    }

    /// The next sequence number to emit or deliver.
    pub fn current_sequence_number(&self) -> u64 {
        self.current
    }

    /// The number of contiguous elements received (== current for an ordered
    /// stream).
    pub fn get_current_sequence_length(&self) -> u64 {
        self.current
    }

    /// The final sequence length, if set.
    pub fn final_sequence_length(&self) -> Option<u64> {
        self.final_length
    }

    /// Push a parcel at its sequence number. Returns false if the parcel is
    /// stale (below the current sequence).
    pub fn push(&mut self, parcel: Parcel) -> bool {
        if parcel.sequence_number < self.current {
            return false;
        }
        self.buffer.insert(parcel.sequence_number, parcel);
        true
    }

    /// Pop the next parcel in sequence order, advancing the current sequence.
    pub fn pop(&mut self) -> Option<Parcel> {
        let parcel = self.buffer.remove(&self.current)?;
        self.current += 1;
        Some(parcel)
    }

    /// Whether the next element in sequence is available.
    pub fn has_next_element(&self) -> bool {
        self.buffer.contains_key(&self.current)
    }

    /// Set the final sequence length. Returns false if it is below the
    /// current sequence.
    pub fn set_final_sequence_length(&mut self, n: u64) -> bool {
        if n < self.current {
            return false;
        }
        self.final_length = Some(n);
        true
    }

    /// Whether the sequence is fully consumed (final length reached).
    pub fn is_sequence_fully_consumed(&self) -> bool {
        self.final_length.is_some_and(|len| self.current >= len)
    }

    /// Whether more elements are still expected.
    pub fn expects_more_elements(&self) -> bool {
        self.final_length.is_none_or(|len| self.current < len)
    }

    /// Whether the queue holds no pending elements and nothing is in flight.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty() && self.current == 0
    }
}

/// A portal: the user-facing endpoint of a terminal router, with its delivered
/// message queue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Portal {
    /// Delivered messages: payload plus extracted objects.
    pub messages: VecDeque<(Vec<u8>, Vec<Object>)>,
}

/// A router: the per-route state machine for one portal (or proxy) on a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Router {
    /// The portal this router fronts (None for proxies and bare routers).
    pub portal: Option<Portal>,
    /// The outward edge, toward the far portal.
    pub outward: Edge,
    /// The inward edge, toward the local portal's new location (proxies only).
    pub inward: Option<Edge>,
    /// The bridge edge to the other route's router, formed by `MergeRoute`
    /// (None for non-merged routers). A bridge edge holds at most one link
    /// (primary or decaying, never both) at a time.
    pub bridge: Option<Edge>,
    /// Parcels received from the outward side: delivered (terminal) or
    /// forwarded inward (proxy) or over the bridge (merged routers).
    pub inbound: ParcelQueue,
    /// Parcels to transmit outward: from the portal (terminal) or the inward
    /// side (proxy) or the bridge (merged routers).
    pub outbound: ParcelQueue,
    /// Whether the far end of the route is closed.
    pub peer_closed: bool,
    /// Whether this router was unexpectedly disconnected.
    pub disconnected: bool,
}

impl Router {
    /// A new terminal router with the given outward link.
    pub fn new_terminal(outward: Link) -> Router {
        Router {
            portal: Some(Portal::default()),
            outward: Edge {
                primary: Some(outward),
                decaying: None,
            },
            inward: None,
            bridge: None,
            inbound: ParcelQueue::new(),
            outbound: ParcelQueue::new(),
            peer_closed: false,
            disconnected: false,
        }
    }

    /// A bare router with no links (used for deserialization setup and local
    /// bridge-chain routers).
    pub fn bare() -> Router {
        Router {
            portal: Some(Portal::default()),
            outward: Edge::default(),
            inward: None,
            bridge: None,
            inbound: ParcelQueue::new(),
            outbound: ParcelQueue::new(),
            peer_closed: false,
            disconnected: false,
        }
    }

    /// Push an outbound parcel from the portal (assigns the next sequence
    /// number) or from the inward side (parcel already sequenced).
    pub fn push_outbound(&mut self, mut parcel: Parcel) -> bool {
        if parcel.sequence_number == u64::MAX {
            parcel.sequence_number = self.outbound.current_sequence_number();
        }
        self.outbound.push(parcel)
    }

    /// Accept an inbound parcel (from the outward side).
    pub fn accept_inbound(&mut self, parcel: Parcel) -> bool {
        self.inbound.push(parcel)
    }

    /// Accept an outbound parcel (from the inward side).
    pub fn accept_outbound(&mut self, parcel: Parcel) -> bool {
        self.outbound.push(parcel)
    }

    /// Collect parcels ready to transmit on the outward edge, choosing the
    /// decaying vs primary link by sequence number. Stops at the first parcel
    /// whose link is unavailable (official `CollectParcelsToFlush`). Returns
    /// each parcel with the link it should be transmitted on (remote or
    /// local).
    pub fn collect_outbound(&mut self) -> Vec<(Link, Parcel)> {
        let mut out = Vec::new();
        while self.outbound.has_next_element() {
            let n = self.outbound.current_sequence_number();
            let link = if self.outward.should_transmit_on_decaying_link(n) {
                self.outward.decaying_link().cloned()
            } else {
                self.outward.primary.clone()
            };
            let Some(link) = link else {
                break;
            };
            let Some(parcel) = self.outbound.pop() else {
                break;
            };
            out.push((link, parcel));
        }
        out
    }

    /// Collect parcels ready to forward inward (proxy), over the inward
    /// edge's decaying or primary link.
    pub fn collect_inbound(&mut self) -> Vec<(Link, Parcel)> {
        let mut out = Vec::new();
        let Some(inward) = &mut self.inward else {
            return out;
        };
        while self.inbound.has_next_element() {
            let n = self.inbound.current_sequence_number();
            let link = if inward.should_transmit_on_decaying_link(n) {
                inward.decaying_link().cloned()
            } else {
                inward.primary.clone()
            };
            let Some(link) = link else {
                break;
            };
            let Some(parcel) = self.inbound.pop() else {
                break;
            };
            out.push((link, parcel));
        }
        out
    }

    /// Collect parcels ready to forward over the bridge edge (merged routers
    /// with no inward edge): the official `CollectParcelsToFlush(inbound,
    /// *bridge_)`.
    pub fn collect_bridge(&mut self) -> Vec<(Link, Parcel)> {
        let mut out = Vec::new();
        let Some(bridge) = &mut self.bridge else {
            return out;
        };
        while self.inbound.has_next_element() {
            let n = self.inbound.current_sequence_number();
            let link = if bridge.should_transmit_on_decaying_link(n) {
                bridge.decaying_link().cloned()
            } else {
                bridge.primary.clone()
            };
            let Some(link) = link else {
                break;
            };
            let Some(parcel) = self.inbound.pop() else {
                break;
            };
            out.push((link, parcel));
        }
        out
    }

    /// The bridge edge's link (primary, or decaying when the primary is
    /// gone), mirroring the official `Flush`'s `bridge_link` capture. Bridges
    /// hold at most one link at a time.
    #[must_use]
    pub fn bridge_link(&self) -> Option<Link> {
        match &self.bridge {
            Some(b) => b.primary.clone().or_else(|| b.decaying_link().cloned()),
            None => None,
        }
    }

    /// Whether the bridge edge is stable (no decaying link).
    #[must_use]
    pub fn bridge_stable(&self) -> bool {
        self.bridge.as_ref().is_some_and(Edge::is_stable)
    }

    /// Attempt to finish the bridge decay; resets the bridge when its
    /// sequence bounds are met (the official `Flush`'s `bridge_->
    /// MaybeFinishDecay(inbound.current, outbound.current)`). Returns whether
    /// the bridge was reset.
    #[must_use]
    pub fn finish_bridge_decay(&mut self) -> bool {
        let inbound_len = self.inbound.current_sequence_number();
        let outbound_len = self.outbound.current_sequence_number();
        let done = match &mut self.bridge {
            Some(b) => b.maybe_finish_decay(inbound_len, outbound_len),
            None => false,
        };
        if done {
            self.bridge = None;
        }
        done
    }

    /// Whether this router is on a central link.
    pub fn on_central_link(&self) -> bool {
        matches!(
            &self.outward.primary,
            Some(l) if l.kind.is_central()
        )
    }

    /// The current outbound and inbound sequence lengths (for decay checks).
    pub fn outbound_length(&self) -> u64 {
        self.outbound.current_sequence_number()
    }

    /// The current inbound sequence length (contiguous parcels received).
    pub fn inbound_length(&self) -> u64 {
        self.inbound.get_current_sequence_length()
    }

    /// Finish the outward and inward decays if their sequence bounds are met.
    /// Returns the released decaying links and whether each edge's decay
    /// finished (the official `Flush`'s `outward_link_decayed` /
    /// `inward_link_decayed`). The released links are captured before
    /// `maybe_finish_decay` (which resets the decaying slot), mirroring the
    /// official `Flush`'s capture of `decaying_outward_link` before
    /// `MaybeFinishDecay`; the caller releases any shared `RouterLinkState`
    /// references the links hold.
    pub fn finish_decays(&mut self) -> (Option<Link>, bool, Option<Link>, bool) {
        // Snapshot the sequence lengths first: the inward-edge borrow below
        // must not overlap reads of the router's queues.
        let outbound_len = self.outbound_length();
        let inbound_len = self.inbound_length();
        let out_link = self.outward.decaying_link().cloned();
        let out = if self.outward.maybe_finish_decay(outbound_len, inbound_len) {
            (out_link, true)
        } else {
            (None, false)
        };
        let in_link = self
            .inward
            .as_ref()
            .and_then(|e| e.decaying_link())
            .cloned();
        let inc = if let Some(inward) = &mut self.inward {
            if inward.maybe_finish_decay(inbound_len, outbound_len) {
                (in_link, true)
            } else {
                (None, false)
            }
        } else {
            (None, false)
        };
        (out.0, out.1, inc.0, inc.1)
    }

    /// Whether the outward edge has a decaying link (before this flush's
    /// decays).
    #[must_use]
    pub fn outward_has_decaying(&self) -> bool {
        self.outward.decaying.is_some()
    }

    /// Whether the inward edge has a decaying link (before this flush's
    /// decays).
    #[must_use]
    pub fn inward_has_decaying(&self) -> bool {
        self.inward.as_ref().is_some_and(|e| e.decaying.is_some())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Local central links are born stable (OpenPortals); bridge links are
    /// born unstable (MergeRoute).
    #[test]
    fn local_link_state_initial_values() {
        let stable = LocalLinkState::new(true);
        assert_eq!(stable.status, RouterLinkStatus::STABLE);
        let unstable = LocalLinkState::new(false);
        assert_eq!(unstable.status, RouterLinkStatus::UNSTABLE);
    }

    /// TryLock on a stable link from either side succeeds; the lock bit is
    /// visible to the other side (shared state).
    #[test]
    fn local_link_try_lock_stable() {
        let (a, b) = Link::local_pair(LinkKind::Central, LinkSide::A, 1, 2, true);
        let state = a.local_state.clone().unwrap();
        // Side A locks.
        assert!(state.borrow_mut().try_lock(true));
        assert_ne!(
            state.borrow().status & RouterLinkStatus::LOCKED_BY_SIDE_A,
            0
        );
        // Side B cannot lock while A holds it.
        assert!(!state.borrow_mut().try_lock(false));
        // Unlock; B can now lock.
        state.borrow_mut().unlock(true);
        assert!(state.borrow_mut().try_lock(false));
        // Both sides see the same shared state: b's handle is the same Rc.
        let state_b = b.local_state.clone().unwrap();
        assert!(std::rc::Rc::ptr_eq(&state, &state_b));
        assert_ne!(
            state.borrow().status & RouterLinkStatus::LOCKED_BY_SIDE_B,
            0
        );
    }

    /// TryLock on an unstable (bridge) link fails from either side without
    /// touching the status (the official `TryLock` returns false when the
    /// requesting side is not stable).
    #[test]
    fn local_link_try_lock_unstable() {
        let (a, _b) = Link::local_pair(LinkKind::Bridge, LinkSide::A, 1, 2, false);
        let state = a.local_state.clone().unwrap();
        assert!(!state.borrow_mut().try_lock(true));
        assert_eq!(state.borrow().status, RouterLinkStatus::UNSTABLE);
        // Once both sides are stable, the link can be locked.
        state.borrow_mut().set_side_stable(true);
        state.borrow_mut().set_side_stable(false);
        assert!(state.borrow_mut().try_lock(true));
    }

    /// A stable side that cannot lock because the peer is not yet stable sets
    /// its own waiting bit (`TryLock`'s waiting fallback).
    #[test]
    fn local_link_try_lock_sets_waiting_when_peer_unstable() {
        let (a, _b) = Link::local_pair(LinkKind::Central, LinkSide::A, 1, 2, true);
        let state = a.local_state.clone().unwrap();
        // Make side B unstable, then have side A try to lock: the lock fails
        // and A's waiting bit is set.
        state.borrow_mut().status = RouterLinkStatus::SIDE_A_STABLE;
        assert!(!state.borrow_mut().try_lock(true));
        assert_ne!(state.borrow().status & RouterLinkStatus::SIDE_A_WAITING, 0);
        assert_eq!(
            state.borrow().status & RouterLinkStatus::LOCKED_BY_SIDE_A,
            0
        );
        // Side B becomes stable; the peer's `ResetWaitingBit` clears A's
        // waiting bit, and the next attempt locks.
        state.borrow_mut().set_side_stable(false);
        assert!(state.borrow_mut().reset_waiting_bit(true));
        assert!(state.borrow_mut().try_lock(true));
        assert_eq!(state.borrow().status & RouterLinkStatus::SIDE_A_WAITING, 0);
    }

    /// ResetWaitingBit refuses while the link is not fully stable, while not
    /// waiting, or while locked.
    #[test]
    fn local_link_reset_waiting_bit() {
        let (a, _b) = Link::local_pair(LinkKind::Central, LinkSide::A, 1, 2, true);
        let state = a.local_state.clone().unwrap();
        // Not waiting: no reset.
        assert!(!state.borrow_mut().reset_waiting_bit(true));
        // A waiting A-side with a not-yet-stable B side: reset refuses because
        // the link is not fully stable.
        state.borrow_mut().status =
            RouterLinkStatus::SIDE_A_STABLE | RouterLinkStatus::SIDE_A_WAITING;
        assert!(!state.borrow_mut().reset_waiting_bit(true));
        // B becomes stable: the waiting bit can now be reset.
        state.borrow_mut().set_side_stable(false);
        assert!(state.borrow_mut().reset_waiting_bit(true));
        assert_eq!(state.borrow().status, RouterLinkStatus::STABLE);
    }

    /// `Link::local_pair` wires each side to the other and shares one state.
    #[test]
    fn local_pair_wiring() {
        let (a, b) = Link::local_pair(LinkKind::Bridge, LinkSide::A, 7, 42, false);
        assert!(a.is_local());
        assert_eq!(a.local_peer, Some(42));
        assert_eq!(a.side, LinkSide::A);
        assert_eq!(b.local_peer, Some(7));
        assert_eq!(b.side, LinkSide::B);
        assert_eq!(a.kind, LinkKind::Bridge);
        assert!(a.local_state.is_some());
    }

    /// `Edge::get_local_peer` / `get_decaying_local_peer` resolve only local
    /// links.
    #[test]
    fn edge_local_peer_helpers() {
        let (local_a, local_b) = Link::local_pair(LinkKind::Central, LinkSide::A, 1, 2, true);
        let mut edge = Edge::default();
        assert_eq!(edge.get_local_peer(), None);
        edge.set_primary_link(local_a);
        assert_eq!(edge.get_local_peer(), Some(2));
        assert_eq!(edge.get_decaying_local_peer(), None);
        // Begin decay moves the primary into the decaying slot.
        assert!(edge.begin_primary_link_decay());
        assert_eq!(edge.get_decaying_local_peer(), Some(2));
        assert_eq!(edge.get_local_peer(), None);
        // A remote link has no local peer.
        edge.set_primary_link(Link::remote(0, 5, LinkKind::Central, LinkSide::B, None));
        assert_eq!(edge.get_local_peer(), None);
        // Sanity: local_b peers back to 1.
        assert_eq!(local_b.local_peer, Some(1));
    }

    /// `collect_bridge` forwards inbound parcels over the bridge edge's link;
    /// `finish_bridge_decay` resets the bridge once both sequence bounds are
    /// met.
    #[test]
    fn bridge_collect_and_decay() {
        let (bridge_a, _bridge_b) = Link::local_pair(LinkKind::Bridge, LinkSide::A, 1, 2, false);
        let mut router = Router::bare();
        router.bridge = Some(Edge {
            primary: Some(bridge_a),
            decaying: None,
        });
        let parcel = Parcel {
            sequence_number: 0,
            data: b"x".to_vec(),
            objects: Vec::new(),
            fragment: None,
        };
        assert!(router.accept_inbound(parcel));
        let collected = router.collect_bridge();
        assert_eq!(collected.len(), 1);
        assert!(collected[0].0.is_local());
        assert_eq!(collected[0].1.data, b"x");

        // No decay bounds yet: finish_bridge_decay is a no-op.
        assert!(!router.finish_bridge_decay());
        // Begin the decay, then set the bounds to the current lengths: the
        // parcel was forwarded, so the inbound sequence is fully sent and the
        // outbound sequence is empty (the bridge's `MaybeFinishDecay(inbound,
        // outbound)`).
        {
            let b = router.bridge.as_mut().unwrap();
            assert!(b.begin_primary_link_decay());
            b.set_length_to_decaying_link(1);
            b.set_length_from_decaying_link(0);
        }
        assert!(router.finish_bridge_decay());
        assert!(router.bridge.is_none());
    }

    /// A bridge router's `bridge_link` prefers the primary over the decaying
    /// link (bridges hold at most one at a time).
    #[test]
    fn bridge_link_prefers_primary() {
        let (bridge_a, _b) = Link::local_pair(LinkKind::Bridge, LinkSide::A, 1, 2, false);
        let mut edge = Edge {
            primary: Some(bridge_a),
            decaying: None,
        };
        let link = edge.primary.clone().unwrap();
        assert!(edge.begin_primary_link_decay());
        // Now decaying only.
        let mut router = Router::bare();
        router.bridge = Some(edge);
        let got = router.bridge_link().unwrap();
        assert_eq!(got, link);
    }

    /// `finish_decays` reports which edges actually decayed.
    #[test]
    fn finish_decays_reports_edges() {
        let mut router = Router::bare();
        router
            .outward
            .set_primary_link(Link::remote(0, 1, LinkKind::Central, LinkSide::B, None));
        assert!(router.outward.begin_primary_link_decay());
        router.outward.set_length_to_decaying_link(0);
        router.outward.set_length_from_decaying_link(0);
        let (out_sub, out_decayed, in_sub, in_decayed) = router.finish_decays();
        assert!(out_decayed);
        assert_eq!(out_sub.map(|l| l.sublink), Some(1));
        assert!(!in_decayed);
        assert_eq!(in_sub.map(|l| l.sublink), None);
    }
}
