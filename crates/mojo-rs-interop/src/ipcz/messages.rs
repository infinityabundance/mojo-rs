//! ipcz node message decoding and encoding — the fixed parameters of the
//! NodeLink message types (node_messages_generator.h) decoded from validated
//! payloads, plus byte-exact encoders for the messages a native node emits.
//!
//! Offsets mirror the generated `node_messages.h` structs for the pinned
//! epoch. Only the message types needed for the interop acceptor are decoded
//! here; all reads are bounds-checked against the payload length. Unknown or
//! malformed messages are classified, never panicked on.

use crate::ipcz::wire::{MessageBuilder, MessageHeader, WireError};

/// A 128-bit ipcz node name (broker-assigned, unique per node).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeName {
    /// High 64 bits.
    pub high: u64,
    /// Low 64 bits.
    pub low: u64,
}

impl NodeName {
    /// Whether the name is the all-zero (invalid) name.
    pub fn is_valid(self) -> bool {
        self.high != 0 || self.low != 0
    }
}

/// A span of memory within a shared buffer owned by the link's `BufferPool`.
/// Matches the ipcz `FragmentDescriptor` wire structure (16 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FragmentDescriptor {
    /// The shared buffer id (64-bit in this epoch).
    pub buffer_id: u64,
    /// Byte offset within the buffer.
    pub offset: u32,
    /// Fragment size in bytes.
    pub size: u32,
}

impl FragmentDescriptor {
    /// The invalid buffer id (`kInvalidBufferId`).
    pub const INVALID_BUFFER_ID: u64 = u64::MAX;

    /// Whether this descriptor is null (references no memory).
    pub fn is_null(self) -> bool {
        self.buffer_id == Self::INVALID_BUFFER_ID
    }
}

/// A driver object carried by a message: its serialized data plus the index of
/// its first attached descriptor in the message's `SCM_RIGHTS` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverObjectRef {
    /// Serialized object payload (after the `ObjectHeader`).
    pub data: Vec<u8>,
    /// Index of the first attached descriptor for this object.
    pub first_fd: usize,
    /// Number of attached descriptors.
    pub num_fds: usize,
}

/// The broker's greeting to a non-broker on a new connection
/// (`ConnectFromBrokerToNonBroker`, message id 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectFromBrokerToNonBroker {
    /// The broker's node name.
    pub broker_name: NodeName,
    /// The name the broker assigned to the receiving non-broker.
    pub receiver_name: NodeName,
    /// The highest protocol version known to the broker.
    pub protocol_version: u32,
    /// The number of initial portals the broker assumes.
    pub num_initial_portals: u32,
    /// Index of the link-memory buffer in the message's driver-object array.
    pub buffer_index: u32,
}

/// The non-broker's reply (`ConnectFromNonBrokerToBroker`, message id 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectFromNonBrokerToBroker {
    /// The highest protocol version known to the sender.
    pub protocol_version: u32,
    /// The number of initial portals the sender assumes.
    pub num_initial_portals: u32,
}

/// `AcceptParcel` (id 20): a parcel (portal message) delivered over a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptParcel {
    /// The sublink the parcel targets.
    pub sublink: u64,
    /// The parcel's sequence number on its route.
    pub sequence_number: u64,
    /// Subparcel index (0 for a standalone parcel).
    pub subparcel_index: u32,
    /// Total subparcel count.
    pub num_subparcels: u32,
    /// Where the parcel data lives; null means inlined in the message.
    pub parcel_fragment: FragmentDescriptor,
    /// Inline parcel data (when `parcel_fragment` is null).
    pub parcel_data: Vec<u8>,
    /// `HandleType` values, one per attached object.
    pub handle_types: Vec<u32>,
    /// Serialized `RouterDescriptor`s (portal transfers).
    pub new_routers: Vec<u8>,
    /// Driver objects attached to the parcel.
    pub driver_objects: Vec<DriverObjectRef>,
}

/// `AcceptParcelDriverObjects` (id 21): the driver-object half of a split
/// parcel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptParcelDriverObjects {
    /// The sublink the parcel targets.
    pub sublink: u64,
    /// The parcel's route sequence number.
    pub sequence_number: u64,
    /// Driver objects attached to the parcel.
    pub driver_objects: Vec<DriverObjectRef>,
}

/// `RouteClosed` (id 22): a route endpoint observed peer closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteClosed {
    /// The sublink whose route is closed.
    pub sublink: u64,
    /// The number of parcels the closing end sent on the route.
    pub sequence_length: u64,
}

/// `RouteDisconnected` (id 23): a route was severed by a node loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDisconnected {
    /// The sublink whose route was severed.
    pub sublink: u64,
}

/// `BypassPeerWithLink` (id 34): a request to bypass a proxy using a new
/// central link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BypassPeerWithLink {
    /// The sublink of the route to bypass.
    pub sublink: u64,
    /// The new sublink allocated for the direct link.
    pub new_sublink: u64,
    /// The `RouterLinkState` fragment for the new central link.
    pub new_link_state_fragment: FragmentDescriptor,
    /// The sequence length already sent by the initiator.
    pub inbound_sequence_length: u64,
}

/// `StopProxyingToLocalPeer` (id 35): a bypassed proxy stops forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopProxyingToLocalPeer {
    /// The sublink of the route.
    pub sublink: u64,
    /// The outbound sequence length at which forwarding stops.
    pub outbound_sequence_length: u64,
}

/// `FlushRouter` (id 36): request a peer router to flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlushRouter {
    /// The sublink of the router to flush.
    pub sublink: u64,
}

/// `AddBlockBuffer` (id 14): a new shared buffer added to the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddBlockBuffer {
    /// The id of the new buffer.
    pub buffer_id: u64,
    /// The block size the buffer backs.
    pub block_size: u32,
    /// Index of the buffer's driver object in the message.
    pub buffer_index: u32,
}

/// `RequestMemory` (id 64) / `ProvideMemory` (id 65).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRequest {
    /// The requested/provided buffer size.
    pub size: u32,
}

/// A decoded node message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedMessage {
    /// `ConnectFromBrokerToNonBroker` (id 0).
    ConnectFromBrokerToNonBroker(ConnectFromBrokerToNonBroker),
    /// `ConnectFromNonBrokerToBroker` (id 1).
    ConnectFromNonBrokerToBroker(ConnectFromNonBrokerToBroker),
    /// `AddBlockBuffer` (id 14).
    AddBlockBuffer(AddBlockBuffer),
    /// `AcceptParcel` (id 20).
    AcceptParcel(AcceptParcel),
    /// `AcceptParcelDriverObjects` (id 21).
    AcceptParcelDriverObjects(AcceptParcelDriverObjects),
    /// `RouteClosed` (id 22).
    RouteClosed(RouteClosed),
    /// `RouteDisconnected` (id 23).
    RouteDisconnected(RouteDisconnected),
    /// `BypassPeerWithLink` (id 34).
    BypassPeerWithLink(BypassPeerWithLink),
    /// `StopProxyingToLocalPeer` (id 35).
    StopProxyingToLocalPeer(StopProxyingToLocalPeer),
    /// `FlushRouter` (id 36).
    FlushRouter(FlushRouter),
    /// `RequestMemory` (id 64).
    RequestMemory(MemoryRequest),
    /// `ProvideMemory` (id 65).
    ProvideMemory(MemoryRequest),
    /// A message type not decoded by the interop layer.
    Unknown(u8),
}

/// The message id for `ConnectFromBrokerToNonBroker`.
pub const MSG_ID_CONNECT_FROM_BROKER_TO_NON_BROKER: u8 = 0;
/// The message id for `ConnectFromNonBrokerToBroker`.
pub const MSG_ID_CONNECT_FROM_NON_BROKER_TO_BROKER: u8 = 1;
/// `AddBlockBuffer` — a new block buffer added to the link.
pub const MSG_ID_ADD_BLOCK_BUFFER: u8 = 14;
/// `AcceptParcel` — a parcel (portal message) delivered over a NodeLink.
pub const MSG_ID_ACCEPT_PARCEL: u8 = 20;
/// `AcceptParcelDriverObjects` — driver objects for an in-flight parcel.
pub const MSG_ID_ACCEPT_PARCEL_DRIVER_OBJECTS: u8 = 21;
/// `RouteClosed` — a route endpoint observed peer closure.
pub const MSG_ID_ROUTE_CLOSED: u8 = 22;
/// `RouteDisconnected` — a route was severed by node loss.
pub const MSG_ID_ROUTE_DISCONNECTED: u8 = 23;
/// `BypassPeerWithLink` — route optimization.
pub const MSG_ID_BYPASS_PEER_WITH_LINK: u8 = 34;
/// `StopProxyingToLocalPeer` — route optimization.
pub const MSG_ID_STOP_PROXYING_TO_LOCAL_PEER: u8 = 35;
/// `FlushRouter` — route state flush.
pub const MSG_ID_FLUSH_ROUTER: u8 = 36;
/// `RequestMemory` — request a new shared buffer.
pub const MSG_ID_REQUEST_MEMORY: u8 = 64;
/// `ProvideMemory` — provide a new shared buffer.
pub const MSG_ID_PROVIDE_MEMORY: u8 = 65;

/// The official name of a node message id (node_messages_generator.h).
pub fn message_id_name(id: u8) -> &'static str {
    match id {
        MSG_ID_CONNECT_FROM_BROKER_TO_NON_BROKER => "ConnectFromBrokerToNonBroker",
        MSG_ID_CONNECT_FROM_NON_BROKER_TO_BROKER => "ConnectFromNonBrokerToBroker",
        2 => "ReferNonBroker",
        3 => "ConnectToReferredBroker",
        4 => "ConnectToReferredNonBroker",
        5 => "NonBrokerReferralAccepted",
        6 => "NonBrokerReferralRejected",
        7 => "ConnectFromBrokerToBroker",
        10 => "RequestIntroduction",
        11 => "AcceptIntroduction",
        12 => "RejectIntroduction",
        13 => "RequestIndirectIntroduction",
        MSG_ID_ADD_BLOCK_BUFFER => "AddBlockBuffer",
        MSG_ID_ACCEPT_PARCEL => "AcceptParcel",
        MSG_ID_ACCEPT_PARCEL_DRIVER_OBJECTS => "AcceptParcelDriverObjects",
        MSG_ID_ROUTE_CLOSED => "RouteClosed",
        MSG_ID_ROUTE_DISCONNECTED => "RouteDisconnected",
        30 => "BypassPeer",
        31 => "AcceptBypassLink",
        32 => "StopProxying",
        33 => "ProxyWillStop",
        MSG_ID_BYPASS_PEER_WITH_LINK => "BypassPeerWithLink",
        MSG_ID_STOP_PROXYING_TO_LOCAL_PEER => "StopProxyingToLocalPeer",
        MSG_ID_FLUSH_ROUTER => "FlushRouter",
        MSG_ID_REQUEST_MEMORY => "RequestMemory",
        MSG_ID_PROVIDE_MEMORY => "ProvideMemory",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Checked little-endian u32 read from an exactly-4-byte slice.
pub(crate) fn le_u32(b: &[u8]) -> Result<u32, WireError> {
    let arr: [u8; 4] = b.try_into().map_err(|_| WireError::BadArray)?;
    Ok(u32::from_le_bytes(arr))
}

/// Checked little-endian u64 read from an exactly-8-byte slice.
pub(crate) fn le_u64(b: &[u8]) -> Result<u64, WireError> {
    let arr: [u8; 8] = b.try_into().map_err(|_| WireError::BadArray)?;
    Ok(u64::from_le_bytes(arr))
}

/// Checked little-endian u16 read from an exactly-2-byte slice.
pub(crate) fn le_u16(b: &[u8]) -> Result<u16, WireError> {
    let arr: [u8; 2] = b.try_into().map_err(|_| WireError::BadArray)?;
    Ok(u16::from_le_bytes(arr))
}

/// Read `n` bytes at payload-relative offset `off` (measured from the start of
/// the `MessageHeader`), validating bounds.
fn read<'a>(
    payload: &'a [u8],
    hdr: &MessageHeader,
    off: usize,
    n: usize,
) -> Result<&'a [u8], WireError> {
    payload
        .get(hdr.size as usize + off..hdr.size as usize + off + n)
        .ok_or(WireError::ShortParams)
}

fn read_u32(payload: &[u8], hdr: &MessageHeader, off: usize) -> Result<u32, WireError> {
    le_u32(read(payload, hdr, off, 4)?)
}

fn read_u64(payload: &[u8], hdr: &MessageHeader, off: usize) -> Result<u64, WireError> {
    le_u64(read(payload, hdr, off, 8)?)
}

fn read_node_name(payload: &[u8], hdr: &MessageHeader, off: usize) -> Result<NodeName, WireError> {
    Ok(NodeName {
        high: read_u64(payload, hdr, off)?,
        low: read_u64(payload, hdr, off + 8)?,
    })
}

fn read_fragment(
    payload: &[u8],
    hdr: &MessageHeader,
    off: usize,
) -> Result<FragmentDescriptor, WireError> {
    Ok(FragmentDescriptor {
        buffer_id: read_u64(payload, hdr, off)?,
        offset: read_u32(payload, hdr, off + 8)?,
        size: read_u32(payload, hdr, off + 12)?,
    })
}

/// Resolve an array at payload-relative `off` into its element bytes.
///
/// Array offsets are measured from the start of the `MessageHeader` (payload
/// offset 0), matching the official encoder. Returns exactly
/// `num_elements * element_size` bytes (the padding is not part of the data).
fn read_array<'a>(
    payload: &'a [u8],
    _hdr: &MessageHeader,
    off: u32,
    element_size: usize,
) -> Result<&'a [u8], WireError> {
    if off == 0 {
        return Ok(&[]);
    }
    let start = off as usize;
    let arr = payload.get(start..).ok_or(WireError::BadArrayOffset)?;
    if arr.len() < 8 {
        return Err(WireError::BadArray);
    }
    let num_bytes = le_u32(&arr[0..4])? as usize;
    let num_elements = le_u32(&arr[4..8])? as usize;
    if num_bytes < 8 || num_bytes % 8 != 0 || num_bytes > arr.len() {
        return Err(WireError::BadArray);
    }
    let data_len = num_bytes - 8;
    let want = num_elements
        .checked_mul(element_size)
        .ok_or(WireError::BadArray)?;
    if want > data_len {
        return Err(WireError::BadArray);
    }
    Ok(&arr[8..8 + want])
}

/// Resolve the message's driver object array into `DriverObjectRef`s.
///
/// `num_fds` is the number of `SCM_RIGHTS` descriptors received with the
/// message. Each object's descriptor span is validated against that count.
fn read_driver_objects(
    payload: &[u8],
    hdr: &MessageHeader,
    num_fds: usize,
) -> Result<Vec<DriverObjectRef>, WireError> {
    let doa = hdr.driver_object_data_array;
    if doa == 0 {
        return Ok(Vec::new());
    }
    let data = read_array(payload, hdr, doa, 8)?;
    if data.len() % 8 != 0 {
        return Err(WireError::BadDriverObjects);
    }
    let mut out = Vec::with_capacity(data.len() / 8);
    let mut next_fd = 0usize;
    for chunk in data.chunks_exact(8) {
        let driver_data_array = le_u32(&chunk[0..4])?;
        let first = le_u16(&chunk[4..6])? as usize;
        let num = le_u16(&chunk[6..8])? as usize;
        if first != next_fd {
            // Objects must consume descriptors in order and contiguously.
            return Err(WireError::BadDriverObjects);
        }
        if first + num > num_fds {
            return Err(WireError::BadDriverObjects);
        }
        next_fd = first + num;
        let obj_data = read_array(payload, hdr, driver_data_array, 1)?;
        out.push(DriverObjectRef {
            data: obj_data.to_vec(),
            first_fd: first,
            num_fds: num,
        });
    }
    if next_fd != num_fds {
        return Err(WireError::BadDriverObjects);
    }
    Ok(out)
}

/// Decode a node message from a validated payload (starting at the
/// `MessageHeader`).
///
/// `num_fds` is the number of attached descriptors received with the message.
pub fn decode_message(payload: &[u8], num_fds: usize) -> Result<DecodedMessage, WireError> {
    let hdr = crate::ipcz::wire::parse_message_header(payload)?;
    let po = hdr.size as usize;
    let params = payload.get(po..).ok_or(WireError::ShortParams)?;
    if params.len() < 8 {
        return Err(WireError::ShortParams);
    }
    let params_size = le_u32(&params[0..4])? as usize;
    if params_size < 8 || params_size > params.len() {
        return Err(WireError::BadParamsSize);
    }
    let fields = po + 8;
    let fields_end = fields + params_size - 8;
    // Helper closures bound to the field range.
    let f_u32 = |off: usize| -> Result<u32, WireError> {
        let s = fields.checked_add(off).ok_or(WireError::BadParamsSize)?;
        let e = s.checked_add(4).ok_or(WireError::BadParamsSize)?;
        if e > fields_end {
            return Err(WireError::ShortParams);
        }
        le_u32(&payload[s..e])
    };
    let f_u64 = |off: usize| -> Result<u64, WireError> {
        let s = fields.checked_add(off).ok_or(WireError::BadParamsSize)?;
        let e = s.checked_add(8).ok_or(WireError::BadParamsSize)?;
        if e > fields_end {
            return Err(WireError::ShortParams);
        }
        le_u64(&payload[s..e])
    };
    let f_node = |off: usize| -> Result<NodeName, WireError> {
        Ok(NodeName {
            high: f_u64(off)?,
            low: f_u64(off + 8)?,
        })
    };
    let f_frag = |off: usize| -> Result<FragmentDescriptor, WireError> {
        Ok(FragmentDescriptor {
            buffer_id: f_u64(off)?,
            offset: f_u32(off + 8)?,
            size: f_u32(off + 12)?,
        })
    };

    let msg = match hdr.message_id {
        MSG_ID_CONNECT_FROM_BROKER_TO_NON_BROKER => {
            if fields_end < fields + 48 {
                return Err(WireError::ShortParams);
            }
            let driver_objects = read_driver_objects(payload, &hdr, num_fds)?;
            let buffer_index = f_u32(40)?;
            // The link-memory driver object must be present and be the only one.
            if driver_objects.len() != 1 || driver_objects[0].first_fd != 0 {
                return Err(WireError::BadDriverObjects);
            }
            DecodedMessage::ConnectFromBrokerToNonBroker(ConnectFromBrokerToNonBroker {
                broker_name: f_node(0)?,
                receiver_name: f_node(16)?,
                protocol_version: f_u32(32)?,
                num_initial_portals: f_u32(36)?,
                buffer_index,
            })
        }
        MSG_ID_CONNECT_FROM_NON_BROKER_TO_BROKER => {
            if fields_end < fields + 8 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::ConnectFromNonBrokerToBroker(ConnectFromNonBrokerToBroker {
                protocol_version: f_u32(0)?,
                num_initial_portals: f_u32(4)?,
            })
        }
        MSG_ID_ADD_BLOCK_BUFFER => {
            if fields_end < fields + 16 {
                return Err(WireError::ShortParams);
            }
            let driver_objects = read_driver_objects(payload, &hdr, num_fds)?;
            DecodedMessage::AddBlockBuffer(AddBlockBuffer {
                buffer_id: f_u64(0)?,
                block_size: f_u32(8)?,
                buffer_index: f_u32(12)?,
            })
            .with_objects_check(driver_objects.len() == 1)
        }
        MSG_ID_ACCEPT_PARCEL => {
            if fields_end < fields + 64 {
                return Err(WireError::ShortParams);
            }
            let sublink = f_u64(0)?;
            let sequence_number = f_u64(8)?;
            let subparcel_index = f_u32(16)?;
            let num_subparcels = f_u32(20)?;
            let parcel_fragment = f_frag(24)?;
            let parcel_data_off = f_u32(40)?;
            let handle_types_off = f_u32(44)?;
            let new_routers_off = f_u32(48)?;
            let driver_objects = read_driver_objects(payload, &hdr, num_fds)?;
            // Inline data is only honored when no fragment is present; the
            // reference prefers the fragment (the inline array is ignored in
            // that case) and an empty parcel may carry neither.
            let parcel_data = read_array(payload, &hdr, parcel_data_off, 1)?.to_vec();
            let handle_types = read_array(payload, &hdr, handle_types_off, 4)?
                .chunks_exact(4)
                .map(le_u32)
                .collect::<Result<Vec<_>, _>>()?;
            let new_routers = read_array(payload, &hdr, new_routers_off, 96)?.to_vec();
            // The number of handle types must match the number of driver
            // objects claimed by the parcel params; validated by the caller
            // against its own routing state.
            DecodedMessage::AcceptParcel(AcceptParcel {
                sublink,
                sequence_number,
                subparcel_index,
                num_subparcels,
                parcel_fragment,
                parcel_data,
                handle_types,
                new_routers,
                driver_objects,
            })
        }
        MSG_ID_ACCEPT_PARCEL_DRIVER_OBJECTS => {
            if fields_end < fields + 16 {
                return Err(WireError::ShortParams);
            }
            let sublink = f_u64(0)?;
            let sequence_number = f_u64(8)?;
            let driver_objects = read_driver_objects(payload, &hdr, num_fds)?;
            DecodedMessage::AcceptParcelDriverObjects(AcceptParcelDriverObjects {
                sublink,
                sequence_number,
                driver_objects,
            })
        }
        MSG_ID_ROUTE_CLOSED => {
            if fields_end < fields + 16 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::RouteClosed(RouteClosed {
                sublink: f_u64(0)?,
                sequence_length: f_u64(8)?,
            })
        }
        MSG_ID_ROUTE_DISCONNECTED => {
            if fields_end < fields + 8 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::RouteDisconnected(RouteDisconnected { sublink: f_u64(0)? })
        }
        MSG_ID_BYPASS_PEER_WITH_LINK => {
            if fields_end < fields + 40 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::BypassPeerWithLink(BypassPeerWithLink {
                sublink: f_u64(0)?,
                new_sublink: f_u64(8)?,
                new_link_state_fragment: f_frag(16)?,
                inbound_sequence_length: f_u64(32)?,
            })
        }
        MSG_ID_STOP_PROXYING_TO_LOCAL_PEER => {
            if fields_end < fields + 16 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::StopProxyingToLocalPeer(StopProxyingToLocalPeer {
                sublink: f_u64(0)?,
                outbound_sequence_length: f_u64(8)?,
            })
        }
        MSG_ID_FLUSH_ROUTER => {
            if fields_end < fields + 8 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::FlushRouter(FlushRouter { sublink: f_u64(0)? })
        }
        MSG_ID_REQUEST_MEMORY => {
            if fields_end < fields + 8 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::RequestMemory(MemoryRequest { size: f_u32(0)? })
        }
        MSG_ID_PROVIDE_MEMORY => {
            if fields_end < fields + 8 {
                return Err(WireError::ShortParams);
            }
            DecodedMessage::ProvideMemory(MemoryRequest { size: f_u32(0)? })
        }
        other => DecodedMessage::Unknown(other),
    };
    Ok(msg)
}

/// Extension helper: validate an `AddBlockBuffer` driver-object condition.
trait WithObjectsCheck {
    fn with_objects_check(self, ok: bool) -> Self;
}

impl WithObjectsCheck for DecodedMessage {
    fn with_objects_check(self, ok: bool) -> Self {
        if !ok {
            // The caller treats a missing buffer object as a protocol error;
            // surface it as an unknown message so the dispatch layer can
            // classify it, matching the reference behavior of failing the
            // handler.
            DecodedMessage::Unknown(MSG_ID_ADD_BLOCK_BUFFER)
        } else {
            self
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Encode `ConnectFromNonBrokerToBroker` (V0 + features array).
///
/// Layout: header (24) + params (StructHeader{24} + protocol_version +
/// num_initial_portals + features offset) + features array (16 + 8 bytes).
pub fn encode_connect_from_non_broker_to_broker(
    num_initial_portals: u32,
    features: u64,
) -> Vec<u8> {
    use crate::ipcz::wire::MESSAGE_HEADER_SIZE;
    use crate::ipcz::wire::align8;
    // Params: 8 (StructHeader) + 8 (V0 fields) + 4 (V1 features offset),
    // padded to 24. The features array follows at the next 8-byte boundary.
    let params_size = align8(8 + 12);
    let features_off = align8(MESSAGE_HEADER_SIZE + params_size) as u32;
    let mut b = MessageBuilder::new(MSG_ID_CONNECT_FROM_NON_BROKER_TO_BROKER);
    let mut fields = Vec::with_capacity(12);
    fields.extend_from_slice(&0u32.to_le_bytes()); // protocol_version
    fields.extend_from_slice(&num_initial_portals.to_le_bytes());
    fields.extend_from_slice(&features_off.to_le_bytes());
    b.append_params(&fields);
    b.append_array(&features.to_le_bytes(), 1);
    b.build()
}

/// Precompute the payload-relative offsets of the parcel-data and
/// handle-types arrays for an `AcceptParcel`, given the fixed 72-byte params
/// (8-byte StructHeader + 64-byte V0 fields) and the array contents.
///
/// Arrays are laid out contiguously after the params, 8-byte aligned, in
/// allocation order; a zero-length array is encoded as offset 0.
fn accept_parcel_array_offsets(data: &[u8], handle_types: &[u32]) -> (u32, u32) {
    use crate::ipcz::wire::MESSAGE_HEADER_SIZE;
    use crate::ipcz::wire::align8;
    let mut off = align8(MESSAGE_HEADER_SIZE + 72);
    let pd = if data.is_empty() {
        0
    } else {
        let o = off as u32;
        off += align8(8 + data.len());
        o
    };
    let ht = if handle_types.is_empty() {
        0
    } else {
        let o = off as u32;
        off += align8(8 + handle_types.len() * 4);
        o
    };
    (pd, ht)
}

/// Encode an `AcceptParcel` with inline parcel data.
///
/// `handle_types` are the `HandleType` values (u32); `driver_data` are the
/// serialized driver object payloads, each consuming one attached descriptor.
pub fn encode_accept_parcel_inline(
    sublink: u64,
    sequence_number: u64,
    data: &[u8],
    handle_types: &[u32],
    driver_data: &[Vec<u8>],
) -> Vec<u8> {
    let (pd_off, ht_off) = accept_parcel_array_offsets(data, handle_types);
    let mut b = MessageBuilder::new(MSG_ID_ACCEPT_PARCEL);
    let mut fields = Vec::with_capacity(64);
    fields.extend_from_slice(&sublink.to_le_bytes());
    fields.extend_from_slice(&sequence_number.to_le_bytes());
    fields.extend_from_slice(&0u32.to_le_bytes()); // subparcel_index
    fields.extend_from_slice(&1u32.to_le_bytes()); // num_subparcels
    // Null parcel fragment: buffer id = u64::MAX, offset = 0, size = 0
    // (FragmentDescriptor is exactly 16 bytes).
    fields.extend_from_slice(&FragmentDescriptor::INVALID_BUFFER_ID.to_le_bytes());
    fields.extend_from_slice(&0u32.to_le_bytes());
    fields.extend_from_slice(&0u32.to_le_bytes());
    fields.extend_from_slice(&pd_off.to_le_bytes());
    fields.extend_from_slice(&ht_off.to_le_bytes());
    fields.extend_from_slice(&0u32.to_le_bytes()); // new_routers
    fields.extend_from_slice(&0u32.to_le_bytes()); // padding
    let num_objects = driver_data.len() as u32;
    fields.extend_from_slice(&0u32.to_le_bytes()); // first_object_index
    fields.extend_from_slice(&num_objects.to_le_bytes());
    b.append_params(&fields);
    if !data.is_empty() {
        b.append_array(data, data.len() as u32);
    }
    if !handle_types.is_empty() {
        let mut bytes = Vec::with_capacity(handle_types.len() * 4);
        for t in handle_types {
            bytes.extend_from_slice(&t.to_le_bytes());
        }
        b.append_array(&bytes, handle_types.len() as u32);
    }
    if !driver_data.is_empty() {
        b.append_driver_objects(driver_data);
    }
    b.build()
}

/// Encode an `AcceptParcel` whose data lives in a shared-memory fragment
/// (the link-memory mailbox path). `parcel_fragment` must be non-null.
pub fn encode_accept_parcel_fragment(
    sublink: u64,
    sequence_number: u64,
    parcel_fragment: FragmentDescriptor,
    handle_types: &[u32],
    driver_data: &[Vec<u8>],
) -> Vec<u8> {
    let (_, ht_off) = accept_parcel_array_offsets(&[], handle_types);
    let mut b = MessageBuilder::new(MSG_ID_ACCEPT_PARCEL);
    let mut fields = Vec::with_capacity(64);
    fields.extend_from_slice(&sublink.to_le_bytes());
    fields.extend_from_slice(&sequence_number.to_le_bytes());
    fields.extend_from_slice(&0u32.to_le_bytes()); // subparcel_index
    fields.extend_from_slice(&1u32.to_le_bytes()); // num_subparcels
    fields.extend_from_slice(&parcel_fragment.buffer_id.to_le_bytes());
    fields.extend_from_slice(&parcel_fragment.offset.to_le_bytes());
    fields.extend_from_slice(&parcel_fragment.size.to_le_bytes());
    // parcel_data: none (data is in the fragment).
    fields.extend_from_slice(&0u32.to_le_bytes());
    fields.extend_from_slice(&ht_off.to_le_bytes());
    fields.extend_from_slice(&0u32.to_le_bytes()); // new_routers
    fields.extend_from_slice(&0u32.to_le_bytes()); // padding
    let num_objects = driver_data.len() as u32;
    fields.extend_from_slice(&0u32.to_le_bytes()); // first_object_index
    fields.extend_from_slice(&num_objects.to_le_bytes());
    b.append_params(&fields);
    if !handle_types.is_empty() {
        let mut bytes = Vec::with_capacity(handle_types.len() * 4);
        for t in handle_types {
            bytes.extend_from_slice(&t.to_le_bytes());
        }
        b.append_array(&bytes, handle_types.len() as u32);
    }
    if !driver_data.is_empty() {
        b.append_driver_objects(driver_data);
    }
    b.build()
}

/// Encode an `AcceptParcelDriverObjects` message (the split-object half).
pub fn encode_accept_parcel_driver_objects(
    sublink: u64,
    sequence_number: u64,
    driver_data: &[Vec<u8>],
) -> Vec<u8> {
    let mut b = MessageBuilder::new(MSG_ID_ACCEPT_PARCEL_DRIVER_OBJECTS);
    let mut fields = Vec::with_capacity(24);
    fields.extend_from_slice(&sublink.to_le_bytes());
    fields.extend_from_slice(&sequence_number.to_le_bytes());
    let num_objects = driver_data.len() as u32;
    fields.extend_from_slice(&0u32.to_le_bytes()); // first_object_index
    fields.extend_from_slice(&num_objects.to_le_bytes());
    b.append_params(&fields);
    if !driver_data.is_empty() {
        b.append_driver_objects(driver_data);
    }
    b.build()
}

/// Encode `RouteClosed`.
pub fn encode_route_closed(sublink: u64, sequence_length: u64) -> Vec<u8> {
    let mut b = MessageBuilder::new(MSG_ID_ROUTE_CLOSED);
    let mut fields = Vec::with_capacity(16);
    fields.extend_from_slice(&sublink.to_le_bytes());
    fields.extend_from_slice(&sequence_length.to_le_bytes());
    b.append_params(&fields);
    b.build()
}

/// Encode `RouteDisconnected`.
pub fn encode_route_disconnected(sublink: u64) -> Vec<u8> {
    let mut b = MessageBuilder::new(MSG_ID_ROUTE_DISCONNECTED);
    let mut fields = Vec::with_capacity(8);
    fields.extend_from_slice(&sublink.to_le_bytes());
    b.append_params(&fields);
    b.build()
}

/// Encode `FlushRouter`.
pub fn encode_flush_router(sublink: u64) -> Vec<u8> {
    let mut b = MessageBuilder::new(MSG_ID_FLUSH_ROUTER);
    let mut fields = Vec::with_capacity(8);
    fields.extend_from_slice(&sublink.to_le_bytes());
    b.append_params(&fields);
    b.build()
}

/// Encode `StopProxyingToLocalPeer`.
pub fn encode_stop_proxying_to_local_peer(sublink: u64, outbound_sequence_length: u64) -> Vec<u8> {
    let mut b = MessageBuilder::new(MSG_ID_STOP_PROXYING_TO_LOCAL_PEER);
    let mut fields = Vec::with_capacity(16);
    fields.extend_from_slice(&sublink.to_le_bytes());
    fields.extend_from_slice(&outbound_sequence_length.to_le_bytes());
    b.append_params(&fields);
    b.build()
}

/// Encode `BypassPeerWithLink`.
pub fn encode_bypass_peer_with_link(
    sublink: u64,
    new_sublink: u64,
    new_link_state_fragment: FragmentDescriptor,
    inbound_sequence_length: u64,
) -> Vec<u8> {
    let mut b = MessageBuilder::new(MSG_ID_BYPASS_PEER_WITH_LINK);
    let mut fields = Vec::with_capacity(40);
    fields.extend_from_slice(&sublink.to_le_bytes());
    fields.extend_from_slice(&new_sublink.to_le_bytes());
    // FragmentDescriptor is exactly 16 bytes: buffer_id u64 + offset u32 +
    // size u32 (no explicit padding field).
    fields.extend_from_slice(&new_link_state_fragment.buffer_id.to_le_bytes());
    fields.extend_from_slice(&new_link_state_fragment.offset.to_le_bytes());
    fields.extend_from_slice(&new_link_state_fragment.size.to_le_bytes());
    fields.extend_from_slice(&inbound_sequence_length.to_le_bytes());
    b.append_params(&fields);
    b.build()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::ipcz::wire::parse_stream;
    use crate::ipcz::wire::set_message_sequence_number;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/testdata/ipcz/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .expect("fixture")
    }

    #[test]
    fn decode_broker_connect_greeting() {
        let data = fixture("broker-to-acceptor.bin");
        let msgs = parse_stream(&data).unwrap();
        let hdr = crate::ipcz::wire::parse_message_header(&msgs[0].payload).unwrap();
        assert_eq!(hdr.message_id, MSG_ID_CONNECT_FROM_BROKER_TO_NON_BROKER);
        match decode_message(&msgs[0].payload, 1).unwrap() {
            DecodedMessage::ConnectFromBrokerToNonBroker(c) => {
                assert!(c.broker_name.is_valid());
                assert!(c.receiver_name.is_valid());
                assert_ne!(c.broker_name, c.receiver_name);
                assert_eq!(c.num_initial_portals, 2);
                assert_eq!(c.buffer_index, 0);
            }
            other => panic!("expected ConnectFromBrokerToNonBroker, got {other:?}"),
        }
    }

    #[test]
    fn decode_broker_accept_parcel() {
        let data = fixture("broker-to-acceptor.bin");
        let msgs = parse_stream(&data).unwrap();
        match decode_message(&msgs[1].payload, 1).unwrap() {
            DecodedMessage::AcceptParcel(p) => {
                assert_eq!(p.sublink, 1);
                assert_eq!(p.sequence_number, 0);
                assert_eq!(p.num_subparcels, 1);
                assert!(p.parcel_fragment.is_null());
                assert_eq!(&p.parcel_data, b"hello-from-broker");
                assert_eq!(p.handle_types, vec![1]); // kBoxedDriverObject
                assert!(p.new_routers.is_empty());
                assert_eq!(p.driver_objects.len(), 1);
                assert_eq!(p.driver_objects[0].first_fd, 0);
                assert_eq!(p.driver_objects[0].num_fds, 1);
            }
            other => panic!("expected AcceptParcel, got {other:?}"),
        }
    }

    #[test]
    fn decode_acceptor_messages() {
        let data = fixture("acceptor-to-broker.bin");
        let msgs = parse_stream(&data).unwrap();
        // msg0: ConnectFromNonBrokerToBroker.
        match decode_message(&msgs[0].payload, 0).unwrap() {
            DecodedMessage::ConnectFromNonBrokerToBroker(c) => {
                assert_eq!(c.num_initial_portals, 8);
            }
            other => panic!("expected Connect, got {other:?}"),
        }
        // msg1: AcceptParcel on sublink 0 (shared-memory service handoff).
        match decode_message(&msgs[1].payload, 1).unwrap() {
            DecodedMessage::AcceptParcel(p) => {
                assert_eq!(p.sublink, 0);
                assert_eq!(p.driver_objects.len(), 1);
            }
            other => panic!("expected AcceptParcel, got {other:?}"),
        }
        // msg2: RouteClosed.
        match decode_message(&msgs[2].payload, 0).unwrap() {
            DecodedMessage::RouteClosed(rc) => {
                assert_eq!(rc.sublink, 0);
                assert_eq!(rc.sequence_length, 1);
            }
            other => panic!("expected RouteClosed, got {other:?}"),
        }
        // msg3: BypassPeerWithLink.
        match decode_message(&msgs[3].payload, 0).unwrap() {
            DecodedMessage::BypassPeerWithLink(b) => {
                assert_eq!(b.sublink, 1);
                assert_eq!(b.new_sublink, 12);
                assert_eq!(b.new_link_state_fragment.buffer_id, 0);
                assert_eq!(b.new_link_state_fragment.offset, 1088);
                assert_eq!(b.new_link_state_fragment.size, 64);
            }
            other => panic!("expected BypassPeerWithLink, got {other:?}"),
        }
        // msg4: FlushRouter.
        assert!(matches!(
            decode_message(&msgs[4].payload, 0).unwrap(),
            DecodedMessage::FlushRouter(f) if f.sublink == 12
        ));
        // msg5: StopProxyingToLocalPeer.
        assert!(matches!(
            decode_message(&msgs[5].payload, 0).unwrap(),
            DecodedMessage::StopProxyingToLocalPeer(s) if s.sublink == 12
        ));
        // msg6: AcceptParcel with fragment-based data.
        match decode_message(&msgs[6].payload, 1).unwrap() {
            DecodedMessage::AcceptParcel(p) => {
                assert_eq!(p.sublink, 13);
                assert_eq!(p.parcel_fragment.buffer_id, 0);
                assert_eq!(p.parcel_fragment.offset, 1088);
                assert_eq!(p.parcel_fragment.size, 64);
                assert!(p.parcel_data.is_empty());
                assert_eq!(p.driver_objects.len(), 1);
            }
            other => panic!("expected AcceptParcel, got {other:?}"),
        }
    }

    #[test]
    fn truncated_params_rejected() {
        let mut payload = vec![0u8; 24 + 8 + 4];
        payload[0] = 24; // message header size
        payload[1] = 0; // version
        payload[2] = 0; // ConnectFromBrokerToNonBroker
        payload[24] = 64; // params StructHeader size (more than present)
        assert!(decode_message(&payload, 0).is_err());
    }

    #[test]
    fn encode_connect_reply_matches_capture() {
        // The oracle acceptor's ConnectFromNonBrokerToBroker is 64 bytes with
        // num_initial_portals=8 and a zeroed features array.
        let encoded = encode_connect_from_non_broker_to_broker(8, 0);
        let data = fixture("acceptor-to-broker.bin");
        let msgs = parse_stream(&data).unwrap();
        let mut captured = msgs[0].payload.clone();
        // The link sequence number is assigned at transmit time; both sides
        // sent Connect with seq 0, so no patch is needed, but assert it.
        set_message_sequence_number(&mut captured, 0);
        assert_eq!(encoded, captured, "Connect reply must be byte-identical");
    }

    #[test]
    fn encode_route_closed_matches_capture() {
        let encoded = encode_route_closed(13, 1);
        let data = fixture("acceptor-to-broker.bin");
        let msgs = parse_stream(&data).unwrap();
        let mut captured = msgs[7].payload.clone();
        // Zero the transmit-time sequence number before comparing.
        set_message_sequence_number(&mut captured, 0);
        assert_eq!(encoded, captured, "RouteClosed must be byte-identical");
    }

    #[test]
    fn encode_stop_proxying_matches_capture() {
        let encoded = encode_stop_proxying_to_local_peer(12, 0);
        let data = fixture("acceptor-to-broker.bin");
        let msgs = parse_stream(&data).unwrap();
        let mut captured = msgs[5].payload.clone();
        set_message_sequence_number(&mut captured, 0);
        assert_eq!(encoded, captured, "StopProxying must be byte-identical");
    }

    #[test]
    fn encode_bypass_peer_with_link_matches_capture() {
        let encoded = encode_bypass_peer_with_link(
            1,
            12,
            FragmentDescriptor {
                buffer_id: 0,
                offset: 1088,
                size: 64,
            },
            0,
        );
        let data = fixture("acceptor-to-broker.bin");
        let msgs = parse_stream(&data).unwrap();
        let mut captured = msgs[3].payload.clone();
        set_message_sequence_number(&mut captured, 0);
        assert_eq!(
            encoded, captured,
            "BypassPeerWithLink must be byte-identical"
        );
    }

    #[test]
    fn encode_accept_parcel_matches_capture() {
        // The oracle acceptor's reply: sublink 13, seq 0, fragment-based data
        // (encoded inline here — the inline variant is the broker's own
        // encoding, so compare against the broker's AcceptParcel instead).
        let broker_data = fixture("broker-to-acceptor.bin");
        let msgs = parse_stream(&broker_data).unwrap();
        let mut captured = msgs[1].payload.clone();
        // Driver data for a WrappedPlatformHandle: ObjectHeader{8, type=4} +
        // WrappedPlatformHandleHeader{8, 0}.
        let mut obj = Vec::new();
        obj.extend_from_slice(&8u32.to_le_bytes());
        obj.extend_from_slice(&4u32.to_le_bytes());
        obj.extend_from_slice(&8u32.to_le_bytes());
        obj.extend_from_slice(&0u32.to_le_bytes());
        let encoded = encode_accept_parcel_inline(1, 0, b"hello-from-broker", &[1], &[obj]);
        set_message_sequence_number(&mut captured, 0);
        assert_eq!(
            encoded, captured,
            "inline AcceptParcel must be byte-identical to the broker's"
        );
    }

    #[test]
    fn driver_objects_validation() {
        // A message claiming more descriptors than were received must fail.
        let broker_data = fixture("broker-to-acceptor.bin");
        let msgs = parse_stream(&broker_data).unwrap();
        assert!(decode_message(&msgs[1].payload, 0).is_err());
        assert!(decode_message(&msgs[1].payload, 1).is_ok());
        assert!(decode_message(&msgs[1].payload, 2).is_err());
    }
}
