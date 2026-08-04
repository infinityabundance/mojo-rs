//! The wire message header: parse/validate per the official
//! `message_header_validator.cc` (pinned epoch), which tolerates future
//! versions by design.

use crate::error::{ValidationError, WireError, WireResult};
use crate::layout::*;
use crate::pointer::Pointer;

/// The interface id constant for the primary interface.
pub const PRIMARY_INTERFACE_ID: u32 = 0xFFFF_FFFF;
/// The interface id constant that is invalid.
pub const INVALID_INTERFACE_ID: u32 = 0xFFFF_FFFE;

/// A parsed message header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageHeader {
    /// Interface id in the base header.
    pub interface_id: u32,
    /// Message name (scoped to the interface).
    pub name: u32,
    /// Combination of `MESSAGE_FLAG_*` constants.
    pub flags: u32,
    /// Trace nonce.
    pub trace_nonce: u32,
    /// Declared version (may exceed 3 for forward compatibility).
    pub version: u32,
    /// Present when `version >= 1`.
    pub request_id: Option<u64>,
    /// Present when `version >= 2`: pointer to the payload struct.
    pub payload: Option<Pointer>,
    /// Present when `version >= 2`: pointer to the payload interface id array.
    pub payload_interface_ids: Option<Pointer>,
    /// Present when `version >= 3`: creation timeticks (microseconds).
    pub creation_timeticks_us: Option<i64>,
}

impl MessageHeader {
    /// Header size required by a known version (0..=3).
    pub fn size_for_version(version: u32) -> Option<usize> {
        match version {
            0 => Some(MESSAGE_HEADER_V0_SIZE),
            1 => Some(MESSAGE_HEADER_V1_SIZE),
            2 => Some(MESSAGE_HEADER_V2_SIZE),
            3 => Some(MESSAGE_HEADER_V3_SIZE),
            _ => None,
        }
    }

    /// Whether the header requests a response.
    pub fn expects_response(&self) -> bool {
        self.flags & MESSAGE_FLAG_EXPECTS_RESPONSE != 0
    }

    /// Whether the header is a response.
    pub fn is_response(&self) -> bool {
        self.flags & MESSAGE_FLAG_IS_RESPONSE != 0
    }

    /// Whether the header carries associated-interface ids.
    pub fn has_interface_id(&self) -> bool {
        self.flags & MESSAGE_FLAG_HAS_INTERFACE_ID != 0
    }

    /// Whether the header uses the v3 layout.
    pub fn use_v3_header(&self) -> bool {
        self.flags & MESSAGE_FLAG_USE_V3_HEADER != 0
    }

    /// Parse and validate a message header per the official
    /// `IsValidMessageHeader` rules.
    ///
    /// Returns the header plus the byte length it consumed.
    pub fn parse(bytes: &[u8]) -> WireResult<(MessageHeader, usize)> {
        let size = bytes.len();
        if size < MIN_MESSAGE_SIZE {
            return Err(WireError::Encode(
                crate::error::EncodeError::MessageTooLarge {
                    size,
                    limit: MIN_MESSAGE_SIZE,
                },
            ));
        }

        // StructHeader { num_bytes, version } at offset 0 (little-endian).
        let num_bytes = read_u32_at(bytes, 0);
        let version = read_u32_at(bytes, 4);

        // Official rule: known versions require exact size; future versions
        // (> 3) require at least the v3 size ("preserve support for future
        // extension of the message header").
        let required = match version {
            0 => MESSAGE_HEADER_V0_SIZE,
            1 => MESSAGE_HEADER_V1_SIZE,
            2 => MESSAGE_HEADER_V2_SIZE,
            3 => MESSAGE_HEADER_V3_SIZE,
            v if v > 3 => MESSAGE_HEADER_V3_SIZE,
            _ => unreachable!(),
        };
        if version <= 3 {
            if (num_bytes as usize) != required {
                return Err(WireError::unexpected_struct_header());
            }
        } else if (num_bytes as usize) < required {
            return Err(WireError::unexpected_struct_header());
        }

        let interface_id = read_u32_at(bytes, HEADER_INTERFACE_ID_OFFSET);
        let name = read_u32_at(bytes, HEADER_NAME_OFFSET);
        let flags = read_u32_at(bytes, HEADER_FLAGS_OFFSET);
        let trace_nonce = read_u32_at(bytes, HEADER_TRACE_NONCE_OFFSET);

        const REQUEST_ID_FLAGS: u32 = MESSAGE_FLAG_EXPECTS_RESPONSE | MESSAGE_FLAG_IS_RESPONSE;
        if version == 0 && (flags & REQUEST_ID_FLAGS) != 0 {
            return Err(WireError::message_header_missing_request_id());
        }
        if (flags & REQUEST_ID_FLAGS) == REQUEST_ID_FLAGS {
            return Err(WireError::message_header_invalid_flags());
        }

        let request_id = if version >= 1 {
            Some(read_u64_at(bytes, HEADER_REQUEST_ID_OFFSET))
        } else {
            None
        };
        let payload = if version >= 2 {
            let raw = read_u64_at(bytes, HEADER_PAYLOAD_OFFSET);
            let ptr = Pointer::decode(HEADER_PAYLOAD_OFFSET as u64, raw)?;
            // Non-nullable payload pointer.
            if ptr == Pointer::Null {
                return Err(WireError::unexpected_null_pointer());
            }
            Some(ptr)
        } else {
            None
        };
        let payload_interface_ids = if version >= 2 {
            let raw = read_u64_at(bytes, HEADER_PAYLOAD_INTERFACE_IDS_OFFSET);
            Some(Pointer::decode(
                HEADER_PAYLOAD_INTERFACE_IDS_OFFSET as u64,
                raw,
            )?)
        } else {
            None
        };
        let creation_timeticks_us = if version >= 3 {
            Some(read_u64_at(bytes, HEADER_CREATION_TIMETICKS_OFFSET) as i64)
        } else {
            None
        };

        Ok((
            MessageHeader {
                interface_id,
                name,
                flags,
                trace_nonce,
                version,
                request_id,
                payload,
                payload_interface_ids,
                creation_timeticks_us,
            },
            if version <= 3 {
                required
            } else {
                MESSAGE_HEADER_V3_SIZE
            },
        ))
    }

    /// Whether an interface id is valid (not the invalid or primary id).
    pub fn is_valid_interface_id(id: u32) -> bool {
        id != INVALID_INTERFACE_ID && id != PRIMARY_INTERFACE_ID
    }

    /// Serialize this header into a fresh buffer (payload pointers are
    /// encoded as null and must be patched by the encoder).
    pub fn serialize(&self) -> WireResult<Vec<u8>> {
        let size = match self.version {
            0 => MESSAGE_HEADER_V0_SIZE,
            1 => MESSAGE_HEADER_V1_SIZE,
            2 => MESSAGE_HEADER_V2_SIZE,
            3 => MESSAGE_HEADER_V3_SIZE,
            v if v > 3 => MESSAGE_HEADER_V3_SIZE,
            _ => return Err(WireError::unexpected_struct_header()),
        };
        let mut out = vec![0u8; size];
        out[0..4].copy_from_slice(&(size as u32).to_le_bytes());
        out[4..8].copy_from_slice(&self.version.to_le_bytes());
        out[HEADER_INTERFACE_ID_OFFSET..HEADER_INTERFACE_ID_OFFSET + 4]
            .copy_from_slice(&self.interface_id.to_le_bytes());
        out[HEADER_NAME_OFFSET..HEADER_NAME_OFFSET + 4].copy_from_slice(&self.name.to_le_bytes());
        out[HEADER_FLAGS_OFFSET..HEADER_FLAGS_OFFSET + 4]
            .copy_from_slice(&self.flags.to_le_bytes());
        out[HEADER_TRACE_NONCE_OFFSET..HEADER_TRACE_NONCE_OFFSET + 4]
            .copy_from_slice(&self.trace_nonce.to_le_bytes());
        if let Some(id) = self.request_id {
            out[HEADER_REQUEST_ID_OFFSET..HEADER_REQUEST_ID_OFFSET + 8]
                .copy_from_slice(&id.to_le_bytes());
        }
        if let Some(ts) = self.creation_timeticks_us {
            out[HEADER_CREATION_TIMETICKS_OFFSET..HEADER_CREATION_TIMETICKS_OFFSET + 8]
                .copy_from_slice(&ts.to_le_bytes());
        }
        Ok(out)
    }
}

impl From<WireError> for ValidationError {
    fn from(e: WireError) -> Self {
        match e {
            WireError::Validation(v) => v,
            WireError::Encode(_) => ValidationError::DeserializationFailed,
        }
    }
}

/// Read a little-endian u32 at `offset` (caller guarantees bounds).
fn read_u32_at(bytes: &[u8], offset: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(b)
}

/// Read a little-endian u64 at `offset` (caller guarantees bounds).
fn read_u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn header_bytes(version: u32, flags: u32, num_bytes: u32) -> Vec<u8> {
        let size = match version {
            0 => 24,
            1 => 32,
            2 => 48,
            3 => 56,
            v if v > 3 => 56,
            _ => 24,
        };
        let mut b = vec![0u8; size];
        b[0..4].copy_from_slice(&num_bytes.to_le_bytes());
        b[4..8].copy_from_slice(&version.to_le_bytes());
        b[HEADER_FLAGS_OFFSET..HEADER_FLAGS_OFFSET + 4].copy_from_slice(&flags.to_le_bytes());
        b
    }

    #[test]
    fn parses_v0() {
        let b = header_bytes(0, 0, 24);
        let (h, len) = MessageHeader::parse(&b).unwrap();
        assert_eq!(h.version, 0);
        assert_eq!(len, 24);
        assert_eq!(h.request_id, None);
    }

    #[test]
    fn parses_v1_with_request_id() {
        let mut b = header_bytes(1, MESSAGE_FLAG_EXPECTS_RESPONSE, 32);
        b[24..32].copy_from_slice(&0x1122334455667788u64.to_le_bytes());
        let (h, _) = MessageHeader::parse(&b).unwrap();
        assert_eq!(h.request_id, Some(0x1122334455667788));
    }

    #[test]
    fn rejects_wrong_num_bytes() {
        // version 0 with num_bytes 32 is invalid per the official rule.
        let b = header_bytes(0, 0, 32);
        assert_eq!(
            MessageHeader::parse(&b).unwrap_err(),
            WireError::unexpected_struct_header()
        );
    }

    #[test]
    fn rejects_missing_request_id() {
        let b = header_bytes(0, MESSAGE_FLAG_IS_RESPONSE, 24);
        assert_eq!(
            MessageHeader::parse(&b).unwrap_err(),
            WireError::message_header_missing_request_id()
        );
    }

    #[test]
    fn rejects_mutually_exclusive_flags() {
        let b = header_bytes(
            1,
            MESSAGE_FLAG_EXPECTS_RESPONSE | MESSAGE_FLAG_IS_RESPONSE,
            32,
        );
        assert_eq!(
            MessageHeader::parse(&b).unwrap_err(),
            WireError::message_header_invalid_flags()
        );
    }

    #[test]
    fn tolerates_future_versions() {
        // version 5 with num_bytes >= 56 is accepted (forward compat), but
        // the v2+ payload pointer is still required to be non-null.
        let mut b = header_bytes(5, 0, 56);
        b[HEADER_PAYLOAD_OFFSET..HEADER_PAYLOAD_OFFSET + 8]
            .copy_from_slice(&(56 - HEADER_PAYLOAD_OFFSET as u64).to_le_bytes());
        b.resize(64, 0);
        b[56..60].copy_from_slice(&8u32.to_le_bytes());
        b[60..64].copy_from_slice(&0u32.to_le_bytes());
        let (h, _) = MessageHeader::parse(&b).unwrap();
        assert_eq!(h.version, 5);
    }

    #[test]
    fn rejects_v2_without_payload() {
        // version 2 with a null payload pointer is rejected.
        let mut b = header_bytes(2, MESSAGE_FLAG_HAS_INTERFACE_ID, 48);
        b[HEADER_PAYLOAD_OFFSET..HEADER_PAYLOAD_OFFSET + 8].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(
            MessageHeader::parse(&b).unwrap_err(),
            WireError::unexpected_null_pointer()
        );
    }

    #[test]
    fn interface_id_constants() {
        assert!(MessageHeader::is_valid_interface_id(0));
        assert!(!MessageHeader::is_valid_interface_id(INVALID_INTERFACE_ID));
        assert!(!MessageHeader::is_valid_interface_id(PRIMARY_INTERFACE_ID));
    }
}
