//! Message validation.
//!
//! Validates a raw message buffer (header + payload) without trusting any
//! length, offset, or pointer in it. The payload struct is walked against its
//! schema with region tracking, alignment, bounds, recursion, and handle-count
//! checks. Rejection is exact: the returned [`WireError`] class is the
//! externally observable classification.

use crate::error::{WireError, WireResult};
use crate::layout;
use crate::message::MessageHeader;
use crate::value::{Decoder, Type, Value};

/// Validation limits.
#[derive(Debug, Clone, Copy)]
pub struct ValidationLimits {
    /// Maximum accepted message size in bytes.
    pub max_message_size: usize,
    /// Maximum nesting depth of objects in a message.
    pub max_recursion_depth: usize,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        ValidationLimits {
            max_message_size: layout::DEFAULT_MAX_MESSAGE_SIZE,
            max_recursion_depth: layout::DEFAULT_MAX_RECURSION_DEPTH,
        }
    }
}

/// Result of validating a message: the parsed header and the decoded payload
/// value.
#[derive(Debug)]
pub struct ValidatedMessage {
    /// The parsed and validated message header.
    pub header: MessageHeader,
    /// The decoded payload value.
    pub payload: Value,
    /// Offset of the payload struct within the message.
    pub payload_offset: usize,
}

/// Validate a complete message (header + payload struct).
///
/// `payload_type` is the schema of the payload struct (the top-level mojom
/// struct). `handle_count` is the number of handles attached to the message.
pub fn validate_message(
    bytes: &[u8],
    handle_count: usize,
    payload_type: &Type,
    limits: &ValidationLimits,
) -> WireResult<ValidatedMessage> {
    if bytes.len() < layout::MIN_MESSAGE_SIZE {
        return Err(WireError::unexpected_struct_header());
    }
    if bytes.len() > limits.max_message_size {
        return Err(WireError::Encode(
            crate::error::EncodeError::MessageTooLarge {
                size: bytes.len(),
                limit: limits.max_message_size,
            },
        ));
    }

    let (header, header_size) = MessageHeader::parse(bytes)?;

    // The payload struct begins at the next 8-aligned offset after the header.
    let payload_offset = layout::align_up(header_size, layout::MESSAGE_ALIGNMENT).ok_or(
        WireError::Encode(crate::error::EncodeError::ArithmeticOverflow),
    )?;
    if payload_offset >= bytes.len() {
        return Err(WireError::unexpected_struct_header());
    }

    let mut dec = Decoder::new(bytes, handle_count);
    dec.max_depth = limits.max_recursion_depth;
    let payload = dec.decode_struct(payload_offset, payload_type)?;

    Ok(ValidatedMessage {
        header,
        payload,
        payload_offset,
    })
}

/// Validate just the header (used by courts that feed raw headers).
#[allow(missing_docs)]
pub fn validate_header(bytes: &[u8]) -> WireResult<MessageHeader> {
    let (header, _) = MessageHeader::parse(bytes)?;
    Ok(header)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::value::{FieldType, Value};

    #[test]
    fn validates_good_message() {
        let ty = Type::Struct {
            fields: vec![
                FieldType {
                    name: "a",
                    ty: Type::U32,
                    min_version: None,
                },
                FieldType {
                    name: "s",
                    ty: Type::String { nullable: true },
                    min_version: Some(1),
                },
            ],
            version: 1,
            nullable: false,
        };
        let header = {
            let mut h = vec![0u8; 24];
            h[0..4].copy_from_slice(&24u32.to_le_bytes()); // num_bytes
            h[4..8].copy_from_slice(&0u32.to_le_bytes()); // version 0
            h
        };
        let val = Value::Struct {
            version: 1,
            fields: vec![Value::U32(7), Value::String("x".to_owned())],
        };
        let enc = crate::value::encode_message(&header, &ty, &val, 0).unwrap();
        let vm = validate_message(&enc.bytes, 0, &ty, &ValidationLimits::default()).unwrap();
        assert_eq!(vm.header.version, 0);
        assert!(matches!(vm.payload, Value::Struct { .. }));
    }

    #[test]
    fn rejects_oversized_message() {
        let ty = Type::Struct {
            fields: vec![],
            version: 0,
            nullable: false,
        };
        let bytes = vec![0u8; 1024];
        let limits = ValidationLimits {
            max_message_size: 512,
            ..Default::default()
        };
        assert!(matches!(
            validate_message(&bytes, 0, &ty, &limits),
            Err(WireError::Encode(
                crate::error::EncodeError::MessageTooLarge { .. }
            ))
        ));
    }
}
