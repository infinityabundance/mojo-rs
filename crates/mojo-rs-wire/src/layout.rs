//! Wire type layouts — ground truth.
//!
//! All sizes/alignments come from the pinned revision's headers
//! (`atlas/reference/wire/bindings_internal.h`, `message_internal.h`) where
//! they are enforced by `static_assert`. Do not "fix" these values without
//! re-verifying against the pinned source and re-running the wire courts.

/// Message alignment: every wire object is 8-byte aligned.
pub const MESSAGE_ALIGNMENT: usize = 8;

/// The encoded value for an invalid handle in `Handle_Data`.
pub const ENCODED_INVALID_HANDLE_VALUE: u32 = 0xFFFF_FFFF;

/// Maximum supported nesting depth for messages (defense in depth; the
/// official validator uses a per-message recursion limit).
pub const DEFAULT_MAX_RECURSION_DEPTH: usize = 32;

/// Default maximum message size accepted by the validator.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 128 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Header layouts (packed; #pragma pack(push, 1) in the pinned source).
// ---------------------------------------------------------------------------

/// `StructHeader { num_bytes: u32, version: u32 }` — 8 bytes.
pub const STRUCT_HEADER_SIZE: usize = 8;

/// `ArrayHeader { num_bytes: u32, num_elements: u32 }` — 8 bytes.
pub const ARRAY_HEADER_SIZE: usize = 8;

/// `Pointer<T> { uint64_t offset }` — 8 bytes; 0 encodes null.
pub const POINTER_SIZE: usize = 8;

/// `Handle_Data { uint32_t value }` — 4 bytes.
pub const HANDLE_DATA_SIZE: usize = 4;

/// `Interface_Data { Handle_Data handle, uint32_t version }` — 8 bytes.
pub const INTERFACE_DATA_SIZE: usize = 8;

/// `AssociatedEndpointHandle_Data { uint32_t value }` — 4 bytes.
pub const ASSOCIATED_ENDPOINT_HANDLE_DATA_SIZE: usize = 4;

/// `AssociatedInterface_Data { handle, version }` — 8 bytes.
pub const ASSOCIATED_INTERFACE_DATA_SIZE: usize = 8;

/// A serialized union always takes 16 bytes (kUnionDataSize, pinned).
pub const UNION_DATA_SIZE: usize = 16;

// ---------------------------------------------------------------------------
// Message headers.
// ---------------------------------------------------------------------------

/// v0: 24 bytes — base header (no request_id, no payload pointers).
pub const MESSAGE_HEADER_V0_SIZE: usize = 24;

/// v1: 32 bytes — adds `request_id: u64` (expects-response / is-response).
pub const MESSAGE_HEADER_V1_SIZE: usize = 32;

/// v2: 48 bytes — adds payload + payload_interface_ids (associated endpoints).
pub const MESSAGE_HEADER_V2_SIZE: usize = 48;

/// v3: 56 bytes — adds creation timeticks.
pub const MESSAGE_HEADER_V3_SIZE: usize = 56;

/// The minimum valid message size: the v0 header.
pub const MIN_MESSAGE_SIZE: usize = MESSAGE_HEADER_V0_SIZE;

/// Offset of the interface id within the base header.
pub const HEADER_INTERFACE_ID_OFFSET: usize = 8;
/// Offset of the message name within the base header.
pub const HEADER_NAME_OFFSET: usize = 12;
/// Offset of the flags field within the base header.
pub const HEADER_FLAGS_OFFSET: usize = 16;
/// Offset of the trace nonce within the base header.
pub const HEADER_TRACE_NONCE_OFFSET: usize = 20;
/// Offset of the request id within the v1 header.
pub const HEADER_REQUEST_ID_OFFSET: usize = 24;
/// Offset of the payload pointer within the v2 header.
pub const HEADER_PAYLOAD_OFFSET: usize = 32;
/// Offset of the payload_interface_ids pointer within the v2 header.
pub const HEADER_PAYLOAD_INTERFACE_IDS_OFFSET: usize = 40;
/// Offset of the creation timeticks within the v3 header.
pub const HEADER_CREATION_TIMETICKS_OFFSET: usize = 48;

// ---------------------------------------------------------------------------
// Message flags (mojo/public/cpp/bindings/message.h).
// ---------------------------------------------------------------------------

/// The message expects a response (sets request_id).
pub const MESSAGE_FLAG_EXPECTS_RESPONSE: u32 = 0x1;
/// The message is a response (sets request_id).
pub const MESSAGE_FLAG_IS_RESPONSE: u32 = 0x2;
/// The message is synchronous.
pub const MESSAGE_FLAG_IS_SYNC: u32 = 0x4;
/// The message carries associated-interface ids.
pub const MESSAGE_FLAG_HAS_INTERFACE_ID: u32 = 0x8;
/// The message uses the v3 header (timeticks).
pub const MESSAGE_FLAG_USE_V3_HEADER: u32 = 0x10;

// ---------------------------------------------------------------------------
// Primitive sizes / alignments used by struct field layout.
// ---------------------------------------------------------------------------

/// Size of each primitive Mojom type in bytes.
pub fn primitive_size(t: PrimitiveType) -> usize {
    use PrimitiveType::*;
    match t {
        Bool | I8 | U8 => 1,
        I16 | U16 => 2,
        I32 | U32 | F32 | Enum => 4,
        I64 | U64 | F64 | Handle | AssociatedEndpointHandle => 8,
    }
}

/// Alignment of each primitive Mojom type in bytes (message alignment is 8).
pub fn primitive_alignment(t: PrimitiveType) -> usize {
    use PrimitiveType::*;
    match t {
        Bool | I8 | U8 | I16 | U16 | I32 | U32 | F32 | Enum | Handle | AssociatedEndpointHandle => {
            4
        }
        I64 | U64 | F64 => 8,
    }
}

/// Mojom primitive type classification for layout purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveType {
    /// Boolean (1 byte, bit-packed in structs).
    Bool,
    /// Signed 8-bit integer.
    I8,
    /// Unsigned 8-bit integer.
    U8,
    /// Signed 16-bit integer.
    I16,
    /// Unsigned 16-bit integer.
    U16,
    /// Signed 32-bit integer.
    I32,
    /// Unsigned 32-bit integer.
    U32,
    /// Signed 64-bit integer.
    I64,
    /// Unsigned 64-bit integer.
    U64,
    /// 32-bit float.
    F32,
    /// 64-bit float.
    F64,
    /// Enum (4 bytes).
    Enum,
    /// Handle (4 bytes).
    Handle,
    /// Associated endpoint handle (4 bytes).
    AssociatedEndpointHandle,
}

/// Round `n` up to the next multiple of `align` (which must be a power of two).
///
/// Returns `None` on overflow.
pub fn align_up(n: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    n.checked_add(mask).map(|v| v & !mask)
}

/// Round `n` down to a multiple of `align`.
pub fn align_down(n: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    n & !(align - 1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn header_sizes_match_pinned_source() {
        assert_eq!(MESSAGE_HEADER_V0_SIZE, 24);
        assert_eq!(MESSAGE_HEADER_V1_SIZE, 32);
        assert_eq!(MESSAGE_HEADER_V2_SIZE, 48);
        assert_eq!(MESSAGE_HEADER_V3_SIZE, 56);
        assert_eq!(STRUCT_HEADER_SIZE, 8);
        assert_eq!(ARRAY_HEADER_SIZE, 8);
        assert_eq!(POINTER_SIZE, 8);
        assert_eq!(HANDLE_DATA_SIZE, 4);
        assert_eq!(INTERFACE_DATA_SIZE, 8);
        assert_eq!(ASSOCIATED_INTERFACE_DATA_SIZE, 8);
    }

    #[test]
    fn align_up_rounds() {
        assert_eq!(align_up(0, 8), Some(0));
        assert_eq!(align_up(1, 8), Some(8));
        assert_eq!(align_up(8, 8), Some(8));
        assert_eq!(align_up(9, 8), Some(16));
        assert_eq!(align_up(usize::MAX, 8), None);
    }
}
