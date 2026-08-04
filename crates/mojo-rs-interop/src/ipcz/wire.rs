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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    let node_sequence_number = u64::from_le_bytes(payload[8..16].try_into().expect("8 bytes"));
    let driver_object_data_array = u32::from_le_bytes(payload[16..20].try_into().expect("4 bytes"));
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
