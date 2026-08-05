//! The ipcz Router state machine (Phase 5).
//!
//! Mirrors the pinned epoch's `ipcz/router.{h,cc}` (Chromium 151.0.7922.105)
//! for the paths a non-broker node exercises: terminal routers on central and
//! peripheral links, proxy routers created by portal serialization, decaying
//! links with sequence-length bounds, parcel forwarding, route closure, and
//! proxy bypass completion (`StopProxying`).
//!
//! The candidate acceptor is single-threaded (one poll loop), so a Router
//! carries no internal lock: the owning `Acceptor` serializes every operation,
//! matching the official observable state machine. Each `Router` owns two
//! `RouteEdge`s at most (outward, and inward when proxying), each with a
//! primary link and optionally one decaying link, plus two `ParcelQueue`s
//! (inbound from the outward side; outbound toward it) with sequence numbers
//! and optional final lengths.

use std::collections::{BTreeMap, VecDeque};

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

/// A single link on an edge: identified by its sublink on the NodeLink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The sublink id on the NodeLink.
    pub sublink: u64,
    /// The link's role.
    pub kind: LinkKind,
    /// This router's side of the link.
    pub side: LinkSide,
    /// The `RouterLinkState` fragment (central links only).
    pub link_state: Option<FragmentDescriptor>,
}

impl Link {
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
    /// The application payload.
    pub data: Vec<u8>,
    /// Attached objects (handles).
    pub objects: Vec<Object>,
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
    /// Parcels received from the outward side: delivered (terminal) or
    /// forwarded inward (proxy).
    pub inbound: ParcelQueue,
    /// Parcels to transmit outward: from the portal (terminal) or the inward
    /// side (proxy).
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
            inbound: ParcelQueue::new(),
            outbound: ParcelQueue::new(),
            peer_closed: false,
            disconnected: false,
        }
    }

    /// A bare router with no links (used for deserialization setup).
    pub fn bare() -> Router {
        Router {
            portal: Some(Portal::default()),
            outward: Edge::default(),
            inward: None,
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
    /// whose link is unavailable (official `CollectParcelsToFlush`).
    pub fn collect_outbound(&mut self) -> Vec<(u64, Parcel)> {
        let mut out = Vec::new();
        while self.outbound.has_next_element() {
            let n = self.outbound.current_sequence_number();
            let link = if self.outward.should_transmit_on_decaying_link(n) {
                self.outward.decaying_link().map(|l| l.sublink)
            } else if self.outward.primary.is_some() {
                self.outward.primary.as_ref().map(|l| l.sublink)
            } else {
                None
            };
            let Some(sublink) = link else {
                break;
            };
            let Some(parcel) = self.outbound.pop() else {
                break;
            };
            out.push((sublink, parcel));
        }
        out
    }

    /// Collect parcels ready to forward inward (proxy), over the inward
    /// edge's decaying or primary link.
    pub fn collect_inbound(&mut self) -> Vec<(u64, Parcel)> {
        let mut out = Vec::new();
        let Some(inward) = &mut self.inward else {
            return out;
        };
        while self.inbound.has_next_element() {
            let n = self.inbound.current_sequence_number();
            let link = if inward.should_transmit_on_decaying_link(n) {
                inward.decaying_link().map(|l| l.sublink)
            } else if inward.primary.is_some() {
                inward.primary.as_ref().map(|l| l.sublink)
            } else {
                None
            };
            let Some(sublink) = link else {
                break;
            };
            let Some(parcel) = self.inbound.pop() else {
                break;
            };
            out.push((sublink, parcel));
        }
        out
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
    /// Returns the released decaying links' sublinks.
    pub fn finish_decays(&mut self) -> (Option<u64>, Option<u64>) {
        // Snapshot the sequence lengths first: the inward-edge borrow below
        // must not overlap reads of the router's queues.
        let outbound_len = self.outbound_length();
        let inbound_len = self.inbound_length();
        let out = if self.outward.maybe_finish_decay(outbound_len, inbound_len) {
            self.outward.release_decaying_link().map(|l| l.sublink)
        } else {
            None
        };
        let inc = if let Some(inward) = &mut self.inward {
            if inward.maybe_finish_decay(inbound_len, outbound_len) {
                inward.release_decaying_link().map(|l| l.sublink)
            } else {
                None
            }
        } else {
            None
        };
        (out, inc)
    }
}
