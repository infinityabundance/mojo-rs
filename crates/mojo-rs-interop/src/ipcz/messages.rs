//! ipcz node message decoding — the fixed parameters of the NodeLink message
//! types (node_messages_generator.h) decoded from validated payloads.
//!
//! Offsets mirror the generated `node_messages.h` structs for the pinned
//! epoch. Only the message types needed for the interop acceptor are decoded
//! here; all reads are bounds-checked against the payload length.

use crate::ipcz::wire::{MessageHeader, WireError};

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

/// A decoded node message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeMessage {
    /// `ConnectFromBrokerToNonBroker` (id 0).
    ConnectFromBrokerToNonBroker(ConnectFromBrokerToNonBroker),
    /// `ConnectFromNonBrokerToBroker` (id 1).
    ConnectFromNonBrokerToBroker(ConnectFromNonBrokerToBroker),
    /// A message type not yet decoded by the interop layer.
    Unknown(u8),
}

/// Read a u32 at `off`, validating bounds.
fn read_u32(payload: &[u8], off: usize) -> Result<u32, WireError> {
    payload
        .get(off..off + 4)
        .ok_or(WireError::ShortMessageHeader)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap_or([0u8; 4])))
}

/// Read a `NodeName` (16 bytes) at `off`, validating bounds.
fn read_node_name(payload: &[u8], off: usize) -> Result<NodeName, WireError> {
    let high = read_u64(payload, off)?;
    let low = read_u64(payload, off + 8)?;
    Ok(NodeName { high, low })
}

/// Read a u64 at `off`, validating bounds.
fn read_u64(payload: &[u8], off: usize) -> Result<u64, WireError> {
    payload
        .get(off..off + 8)
        .ok_or(WireError::ShortMessageHeader)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0u8; 8])))
}

/// Decode a node message from a validated payload (starting at the
/// `MessageHeader`).
pub fn decode_node_message(payload: &[u8]) -> Result<NodeMessage, WireError> {
    let hdr = crate::ipcz::wire::parse_message_header(payload)?;
    // The fixed parameters begin after the message header.
    let po = hdr.size as usize;
    // The parameters struct begins with StructHeader { size u32, padding u32 }.
    let params_size = read_u32(payload, po)? as usize;
    let _params_padding = read_u32(payload, po + 4)?;
    let fields = po + 8;
    let fields_end = po + params_size;
    match hdr.message_id {
        0 => {
            // ConnectFromBrokerToNonBroker V0:
            //   NodeName broker_name (16)
            //   NodeName receiver_name (16)
            //   u32 protocol_version
            //   u32 num_initial_portals
            //   u32 buffer (driver object index)
            //   u32 padding
            if fields_end < fields + 48 {
                return Err(WireError::ShortMessageHeader);
            }
            let broker_name = read_node_name(payload, fields)?;
            let receiver_name = read_node_name(payload, fields + 16)?;
            let protocol_version = read_u32(payload, fields + 32)?;
            let num_initial_portals = read_u32(payload, fields + 36)?;
            let buffer_index = read_u32(payload, fields + 40)?;
            Ok(NodeMessage::ConnectFromBrokerToNonBroker(
                ConnectFromBrokerToNonBroker {
                    broker_name,
                    receiver_name,
                    protocol_version,
                    num_initial_portals,
                    buffer_index,
                },
            ))
        }
        1 => {
            // ConnectFromNonBrokerToBroker V0 (generated node_messages.h):
            //   u32 protocol_version
            //   u32 num_initial_portals
            if fields_end < fields + 8 {
                return Err(WireError::ShortMessageHeader);
            }
            let protocol_version = read_u32(payload, fields)?;
            let num_initial_portals = read_u32(payload, fields + 4)?;
            Ok(NodeMessage::ConnectFromNonBrokerToBroker(
                ConnectFromNonBrokerToBroker {
                    protocol_version,
                    num_initial_portals,
                },
            ))
        }
        other => Ok(NodeMessage::Unknown(other)),
    }
}

/// The message id for `ConnectFromBrokerToNonBroker`.
pub const MSG_ID_CONNECT_FROM_BROKER_TO_NON_BROKER: u8 = 0;
/// The message id for `ConnectFromNonBrokerToBroker`.
pub const MSG_ID_CONNECT_FROM_NON_BROKER_TO_BROKER: u8 = 1;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::ipcz::wire::parse_stream;

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
        let msg = decode_node_message(&msgs[0].payload).unwrap();
        match msg {
            NodeMessage::ConnectFromBrokerToNonBroker(c) => {
                assert!(c.broker_name.is_valid());
                assert!(c.receiver_name.is_valid());
                assert_ne!(c.broker_name, c.receiver_name);
                // One attached pipe yields 1 internal + 1 attachment portal.
                assert_eq!(c.num_initial_portals, 2);
                assert_eq!(c.buffer_index, 0);
            }
            other => panic!("expected ConnectFromBrokerToNonBroker, got {other:?}"),
        }
    }

    #[test]
    fn decode_acceptor_connect_reply() {
        let data = fixture("acceptor-to-broker.bin");
        let msgs = parse_stream(&data).unwrap();
        // The non-broker's first message is ConnectFromNonBrokerToBroker. It
        // always assumes the maximum possible attachment count (7 + 1 internal
        // portal); the broker's ConnectFromBrokerToNonBroker carries the real
        // count, and the difference is resolved by the peer-closure rule for
        // excess portals.
        let first = decode_node_message(&msgs[0].payload).unwrap();
        match first {
            NodeMessage::ConnectFromNonBrokerToBroker(c) => {
                assert_eq!(c.num_initial_portals, 8);
            }
            other => panic!("expected ConnectFromNonBrokerToBroker, got {other:?}"),
        }
    }

    #[test]
    fn truncated_params_rejected() {
        // A valid header claiming a params size larger than the payload.
        let mut payload = vec![0u8; 24 + 8 + 4];
        payload[0] = 24; // message header size
        payload[1] = 0; // version
        payload[2] = 0; // ConnectFromBrokerToNonBroker
        payload[24] = 64; // params StructHeader size (more than present)
        assert!(decode_node_message(&payload).is_err());
    }
}
