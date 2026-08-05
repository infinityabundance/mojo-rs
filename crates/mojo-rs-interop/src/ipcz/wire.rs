//! ipcz node-link wire protocol — the on-the-wire format used by Mojo over a
//! channel transport in the CoreIpcz epoch.
//!
//! This module parses and validates the framing of the official ipcz transport
//! traffic, using the captured broker⇄acceptor wire bytes
//! (`testdata/ipcz/*.bin`, produced by `scripts/run_invite_court.sh`) as golden
//! fixtures. It is the foundation for the native interop acceptor (Phase 3
//! gate: bidirectional official C++ ⇄ native Rust transfer).
//!
//! Wire layout of a channel message (packed, little-endian):
//!
//! ```text
//! 0  IpczHeader (mojo/core/channel.h, `IpczHeader`)
//!      u16 size                     header size in bytes (16)
//!      u16 num_handles              SCM_RIGHTS descriptors attached
//!      u32 num_bytes                total message size incl. this header
//!      i64 creation_timeticks_us    (v2 field; ignored semantically)
//! 16 ipcz MessageHeader (third_party/ipcz/src/ipcz/message.h)
//!      u8  size                     header size in bytes (24 for v0)
//!      u8  version
//!      u8  message_id               IPCZ_MSG_ID (node_messages_generator.h)
//!      u8  reserved0[5]
//!      u64 node_sequence_number     per-NodeLink ordering
//!      u32 driver_object_data_array offset of the DriverObjectData array, or 0
//!      u32 reserved1
//! 40 parameters struct (StructHeader{size u32, padding u32} + fixed fields)
//!     ...
//!     arrays and driver objects follow the fixed parameters
//! ```
//!
//! All lengths are validated before any access; a malformed stream is a parse
//! error, never a panic or an out-of-bounds read.

use std::fmt;

/// A parsed channel message: the channel header plus the raw ipcz payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMessage {
    /// `IpczHeader.num_handles`.
    pub num_handles: u16,
    /// The ipcz payload (starting at the ipcz `MessageHeader`).
    pub payload: Vec<u8>,
}

/// The ipcz `MessageHeader` (v0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageHeader {
    /// Header size in bytes (`size` field; 24 for v0).
    pub size: u8,
    /// Header version.
    pub version: u8,
    /// Message id (`IPCZ_MSG_ID`).
    pub message_id: u8,
    /// Per-NodeLink sequence number (ordering contract).
    pub node_sequence_number: u64,
    /// Offset of the `DriverObjectData` array, or 0 when none.
    pub driver_object_data_array: u32,
}

/// Parse errors — all malformed-input conditions are classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// Fewer than 16 bytes for the channel header.
    ShortChannelHeader,
    /// The channel header's `size` field is not 16 (unknown version).
    UnknownChannelHeaderSize(u16),
    /// The channel header's `num_bytes` is smaller than the header.
    BadChannelMessageSize(u32),
    /// The message extends past the captured stream.
    TruncatedMessage,
    /// Fewer than 24 bytes for the ipcz message header.
    ShortMessageHeader,
    /// The ipcz header's `size` field is not 24 (unknown version).
    UnknownMessageHeaderSize(u8),
    /// The parameter struct is truncated or absent.
    ShortParams,
    /// The parameter struct claims an inconsistent size.
    BadParamsSize,
    /// An array offset does not resolve within the message.
    BadArrayOffset,
    /// An array header is malformed (size/alignment/elements).
    BadArray,
    /// The driver object array or descriptor spans are malformed.
    BadDriverObjects,
    /// A parcel has neither inline data nor a fragment (or both).
    BadParcelData,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::ShortChannelHeader => write!(f, "short channel header"),
            WireError::UnknownChannelHeaderSize(s) => {
                write!(f, "unknown channel header size {s}")
            }
            WireError::BadChannelMessageSize(n) => write!(f, "bad channel message size {n}"),
            WireError::TruncatedMessage => write!(f, "truncated message"),
            WireError::ShortMessageHeader => write!(f, "short ipcz message header"),
            WireError::UnknownMessageHeaderSize(s) => {
                write!(f, "unknown ipcz message header size {s}")
            }
            WireError::ShortParams => write!(f, "short parameter struct"),
            WireError::BadParamsSize => write!(f, "bad parameter struct size"),
            WireError::BadArrayOffset => write!(f, "array offset out of bounds"),
            WireError::BadArray => write!(f, "malformed array"),
            WireError::BadDriverObjects => write!(f, "malformed driver objects"),
            WireError::BadParcelData => write!(f, "parcel data neither inline nor fragment"),
        }
    }
}

/// The size of the `IpczHeader` in this epoch.
pub const IPCZ_HEADER_SIZE: usize = 16;
/// The size of the ipcz `MessageHeader` v0.
pub const MESSAGE_HEADER_SIZE: usize = 24;

/// Parse one channel message from the start of `stream`.
///
/// Returns the message and the number of bytes consumed.
pub fn parse_channel_message(stream: &[u8]) -> Result<(ChannelMessage, usize), WireError> {
    if stream.len() < IPCZ_HEADER_SIZE {
        return Err(WireError::ShortChannelHeader);
    }
    let size = u16::from_le_bytes([stream[0], stream[1]]);
    if size != IPCZ_HEADER_SIZE as u16 {
        return Err(WireError::UnknownChannelHeaderSize(size));
    }
    let num_handles = u16::from_le_bytes([stream[2], stream[3]]);
    let num_bytes = u32::from_le_bytes([stream[4], stream[5], stream[6], stream[7]]);
    let num_bytes = num_bytes as usize;
    if num_bytes < IPCZ_HEADER_SIZE {
        return Err(WireError::BadChannelMessageSize(num_bytes as u32));
    }
    let payload_len = num_bytes - IPCZ_HEADER_SIZE;
    if stream.len() < num_bytes {
        return Err(WireError::TruncatedMessage);
    }
    Ok((
        ChannelMessage {
            num_handles,
            payload: stream[IPCZ_HEADER_SIZE..num_bytes].to_vec(),
        },
        num_bytes,
    ))
}

/// Parse the ipcz `MessageHeader` from the start of a payload.
pub fn parse_message_header(payload: &[u8]) -> Result<MessageHeader, WireError> {
    if payload.len() < MESSAGE_HEADER_SIZE {
        return Err(WireError::ShortMessageHeader);
    }
    let size = payload[0];
    if size != MESSAGE_HEADER_SIZE as u8 {
        return Err(WireError::UnknownMessageHeaderSize(size));
    }
    // version = payload[1]; message_id = payload[2]; reserved0 = payload[3..8].
    // The length check above guarantees these slices are in bounds, so the
    // copies below cannot fail.
    let mut seq_bytes = [0u8; 8];
    seq_bytes.copy_from_slice(&payload[8..16]);
    let mut dobj_bytes = [0u8; 4];
    dobj_bytes.copy_from_slice(&payload[16..20]);
    let node_sequence_number = u64::from_le_bytes(seq_bytes);
    let driver_object_data_array = u32::from_le_bytes(dobj_bytes);
    Ok(MessageHeader {
        size,
        version: payload[1],
        message_id: payload[2],
        node_sequence_number,
        driver_object_data_array,
    })
}

/// Parse all channel messages in a captured stream.
pub fn parse_stream(stream: &[u8]) -> Result<Vec<ChannelMessage>, WireError> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < stream.len() {
        let (msg, consumed) = parse_channel_message(&stream[off..])?;
        out.push(msg);
        off += consumed;
    }
    Ok(out)
}

/// Encode an `IpczHeader` framing a raw ipcz payload of `payload_len` bytes.
///
/// `num_handles` is the number of `SCM_RIGHTS` descriptors that will be
/// attached to the message's first transmission chunk.
pub fn encode_channel_header(payload_len: usize, num_handles: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(IPCZ_HEADER_SIZE);
    out.extend_from_slice(&(IPCZ_HEADER_SIZE as u16).to_le_bytes());
    out.extend_from_slice(&num_handles.to_le_bytes());
    // The encoder only emits messages far below the u32 limit; an overflow is
    // a programming error (the official encoder asserts the same invariant).
    let total = (IPCZ_HEADER_SIZE + payload_len) as u32;
    debug_assert_eq!(total as usize, IPCZ_HEADER_SIZE + payload_len);
    out.extend_from_slice(&total.to_le_bytes());
    // creation_timeticks_us: informational; zero is accepted.
    out.extend_from_slice(&0i64.to_le_bytes());
    out
}

/// Encode a complete channel message: `IpczHeader` + ipcz payload.
pub fn encode_channel_message(payload: &[u8], num_handles: u16) -> Vec<u8> {
    let mut out = encode_channel_header(payload.len(), num_handles);
    out.extend_from_slice(payload);
    out
}

/// An in-progress ipcz message buffer: the `MessageHeader` plus params and
/// any arrays appended after them. Arrays are appended in allocation order
/// and 8-byte aligned, exactly as the official `Message::AllocateGenericArray`
/// lays them out.
#[derive(Debug, Default)]
pub struct MessageBuilder {
    /// The message header.
    pub header: MessageHeader,
    /// All payload bytes after the fixed `MessageHeader`.
    body: Vec<u8>,
}

impl MessageBuilder {
    /// Start a new message of the given id. The header is 24 bytes and the
    /// node sequence number is filled in by the sender at transmit time.
    pub fn new(message_id: u8) -> MessageBuilder {
        MessageBuilder {
            header: MessageHeader {
                size: MESSAGE_HEADER_SIZE as u8,
                version: 0,
                message_id,
                node_sequence_number: 0,
                driver_object_data_array: 0,
            },
            body: Vec::new(),
        }
    }

    /// Append a parameter struct: `StructHeader` + raw field bytes. The
    /// params size is rounded up to an 8-byte boundary (zero-padded), matching
    /// the official generator's padded struct sizes.
    pub fn append_params(&mut self, fields: &[u8]) {
        let padded = align8(8 + fields.len());
        // Encoder invariant: params sizes are bounded by the message
        // definitions (tens of bytes); the official encoder asserts the same.
        let size = padded as u32;
        debug_assert_eq!(size as usize, padded);
        self.body.extend_from_slice(&size.to_le_bytes());
        self.body.extend_from_slice(&0u32.to_le_bytes());
        self.body.extend_from_slice(fields);
        let pad = padded - 8 - fields.len();
        self.body.resize(self.body.len() + pad, 0);
    }

    /// Append an array with the given element bytes; returns its byte offset
    /// within the message payload (from the start of the `MessageHeader`).
    ///
    /// `num_elements` is the element COUNT encoded in the `ArrayHeader` (the
    /// official encoder stores the element count, not the byte length). The
    /// array is laid out as `ArrayHeader` + element bytes, padded to an 8-byte
    /// boundary, matching the official encoder.
    pub fn append_array(&mut self, elements: &[u8], num_elements: u32) -> u32 {
        if elements.is_empty() {
            return 0;
        }
        let offset = self.payload_len();
        let num_bytes = align8(8 + elements.len());
        self.body
            .extend_from_slice(&(num_bytes as u32).to_le_bytes());
        self.body.extend_from_slice(&num_elements.to_le_bytes());
        self.body.extend_from_slice(elements);
        // Zero the padding so uninitialized bytes never leak onto the wire.
        let pad = num_bytes - 8 - elements.len();
        self.body.resize(self.body.len() + pad, 0);
        // Encoder invariant: array offsets are bounded by the message size
        // (far below u32 limits); the official encoder asserts the same.
        let offset = offset as u32;
        debug_assert_eq!(
            offset as usize,
            self.payload_len() - 8 - elements.len() - pad
        );
        offset
    }

    /// The current payload length (message header included).
    pub fn payload_len(&self) -> usize {
        MESSAGE_HEADER_SIZE + self.body.len()
    }

    /// The `MessageHeader.driver_object_data_array` value: the offset of the
    /// `DriverObjectData` array from the start of the payload, or 0 if none.
    pub fn driver_object_data_array(&self) -> u32 {
        self.header.driver_object_data_array
    }

    /// Whether any driver object data has been appended.
    pub fn has_driver_objects(&self) -> bool {
        self.header.driver_object_data_array != 0
    }

    /// Append the `DriverObjectData` array and the serialized driver data for
    /// each object. `driver_data` entries are the serialized object payloads
    /// (e.g. `ObjectHeader` + object data); the matching number of raw
    /// descriptors is attached out-of-band at transmission time.
    ///
    /// The `DriverObjectData` array is placed before the per-object data
    /// arrays, matching the official `Message::Serialize()` which allocates
    /// the driver object array first and each object's data array after.
    pub fn append_driver_objects(&mut self, driver_data: &[Vec<u8>]) {
        if driver_data.is_empty() {
            return;
        }
        // Precompute the placement: the DriverObjectData array occupies the
        // current position; each object's data array follows, aligned.
        let base = self.payload_len();
        let do_arr_size = align8(8 + driver_data.len() * 8);
        let mut data_offsets = Vec::with_capacity(driver_data.len());
        let mut off = base + do_arr_size;
        for data in driver_data {
            data_offsets.push(off);
            off += align8(8 + data.len());
        }
        let mut objects = Vec::with_capacity(driver_data.len() * 8);
        let mut num_handles = 0u16;
        for (i, data) in driver_data.iter().enumerate() {
            objects.extend_from_slice(&(data_offsets[i] as u32).to_le_bytes());
            objects.extend_from_slice(&num_handles.to_le_bytes());
            objects.extend_from_slice(&1u16.to_le_bytes());
            num_handles += 1;
            let _ = data;
        }
        let arr_offset = self.append_array(&objects, driver_data.len() as u32);
        debug_assert_eq!(arr_offset as usize, base, "driver object array placement");
        for (i, data) in driver_data.iter().enumerate() {
            let o = self.append_array(data, data.len() as u32);
            debug_assert_eq!(o as usize, data_offsets[i], "driver data array placement");
        }
        self.header.driver_object_data_array = arr_offset;
    }

    /// The number of driver handles attached (for the `IpczHeader`).
    pub fn num_attached_handles(&self) -> u16 {
        if self.has_driver_objects() {
            // One SCM_RIGHTS descriptor per driver object.
            let count = self.driver_object_count();
            count as u16
        } else {
            0
        }
    }

    fn driver_object_count(&self) -> usize {
        // The DriverObjectData array element count encodes the object count.
        let off = self.header.driver_object_data_array as usize;
        if off == 0 || off + 8 > self.payload_len() {
            return 0;
        }
        let rel = off - MESSAGE_HEADER_SIZE;
        if rel + 8 > self.body.len() {
            return 0;
        }
        // The bounds were validated above; the 4-byte window is in range.
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.body[rel + 4..rel + 8]);
        u32::from_le_bytes(b) as usize
    }

    /// Serialize to the full ipcz payload (header + params + arrays).
    pub fn build(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.payload_len());
        out.push(self.header.size);
        out.push(self.header.version);
        out.push(self.header.message_id);
        out.extend_from_slice(&[0u8; 5]); // reserved0
        out.extend_from_slice(&self.header.node_sequence_number.to_le_bytes());
        out.extend_from_slice(&self.header.driver_object_data_array.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved1
        out.extend_from_slice(&self.body);
        out
    }
}

/// Round `n` up to a multiple of 8.
pub(crate) fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Patch the `node_sequence_number` field of an encoded message payload
/// (bytes 8..16 of the `MessageHeader`). Used by `NodeLink::Transmit`.
pub fn set_message_sequence_number(payload: &mut [u8], seq: u64) {
    payload[8..16].copy_from_slice(&seq.to_le_bytes());
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/testdata/ipcz/{}", env!("CARGO_MANIFEST_DIR"), name);
        std::fs::read(path).expect("fixture")
    }

    #[test]
    fn broker_to_acceptor_stream_parses() {
        let data = fixture("broker-to-acceptor.bin");
        let msgs = parse_stream(&data).unwrap();
        assert!(!msgs.is_empty());
        // First message: the broker's ConnectFromBrokerToNonBroker greeting.
        let hdr = parse_message_header(&msgs[0].payload).unwrap();
        assert_eq!(hdr.version, 0);
        assert_eq!(hdr.message_id, 0); // ConnectFromBrokerToNonBroker
        assert_eq!(hdr.node_sequence_number, 0); // first message
        // The Connect greeting carries the link-memory driver object.
        assert_eq!(msgs[0].num_handles, 1);
    }

    #[test]
    fn all_messages_have_valid_headers() {
        for name in ["broker-to-acceptor.bin", "acceptor-to-broker.bin"] {
            let data = fixture(name);
            let msgs = parse_stream(&data).unwrap();
            for m in &msgs {
                let hdr = parse_message_header(&m.payload).unwrap();
                assert_eq!(hdr.size, 24);
            }
        }
    }

    #[test]
    fn roundtrip_consumes_exact_stream() {
        let data = fixture("acceptor-to-broker.bin");
        let msgs = parse_stream(&data).unwrap();
        // The parsed messages must account for every captured byte.
        let total: usize = msgs
            .iter()
            .map(|m| IPCZ_HEADER_SIZE + m.payload.len())
            .sum();
        assert_eq!(total, data.len());
    }

    #[test]
    fn malformed_inputs_rejected() {
        assert_eq!(
            parse_channel_message(&[]),
            Err(WireError::ShortChannelHeader)
        );
        assert_eq!(
            parse_channel_message(&[0; 8]),
            Err(WireError::ShortChannelHeader)
        );
        // Unknown header size.
        let mut bad = vec![0u8; 32];
        bad[0] = 32;
        bad[1] = 0;
        assert!(matches!(
            parse_channel_message(&bad),
            Err(WireError::UnknownChannelHeaderSize(32))
        ));
        // Truncated: header says 100 bytes but only 20 present.
        let mut trunc = vec![0u8; 20];
        trunc[0] = 16;
        trunc[1] = 0;
        trunc[4] = 100;
        trunc[5] = 0;
        trunc[6] = 0;
        trunc[7] = 0;
        assert_eq!(
            parse_channel_message(&trunc),
            Err(WireError::TruncatedMessage)
        );
        // Bad total size smaller than the header.
        let mut badsize = vec![0u8; 32];
        badsize[0] = 16;
        badsize[1] = 0;
        badsize[4] = 8;
        assert!(matches!(
            parse_channel_message(&badsize),
            Err(WireError::BadChannelMessageSize(8))
        ));
        // Short ipcz payload.
        assert_eq!(
            parse_message_header(&[0u8; 10]),
            Err(WireError::ShortMessageHeader)
        );
    }
}
