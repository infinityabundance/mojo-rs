//! Relative pointers.
//!
//! A `Pointer<T>` on the wire is an 8-byte offset from the pointer's own
//! storage address to the target; the value 0 encodes null. Offsets are
//! relative so messages are position-independent and remain valid across
//! copies.
//!
//! Encoding and decoding here are pure arithmetic; validation of the *target*
//! (alignment, bounds, overlap) lives in [`crate::validate`].

use crate::error::{EncodeError, WireError, WireResult};

/// A decoded relative pointer: either null or an offset within the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    /// Offset 0 — null.
    Null,
    /// Non-null: the target offset relative to the pointer's storage address.
    Offset(u64),
}

impl Pointer {
    /// Decode a pointer read from `ptr_addr` (absolute address of the pointer
    /// storage within the message).
    ///
    /// `ptr_addr` is the offset of the pointer field within the message
    /// buffer; the returned [`Pointer::Offset`] is the offset of the target
    /// *within the same buffer*.
    #[inline]
    pub fn decode(ptr_addr: u64, raw: u64) -> WireResult<Pointer> {
        if raw == 0 {
            return Ok(Pointer::Null);
        }
        // target = ptr_addr + raw (this is the encoding the official
        // EncodePointer uses: *offset = p - offset_addr). Like the official
        // implementation, the arithmetic WRAPS; invalid targets are rejected
        // later by the claim/bounds checks.
        Ok(Pointer::Offset(ptr_addr.wrapping_add(raw)))
    }

    /// Encode a relative offset from the pointer's storage address to the
    /// target. `ptr_addr` and `target` are offsets within the same buffer.
    /// Negative offsets wrap (two's complement), matching the official
    /// `EncodePointer`.
    #[inline]
    pub fn encode(ptr_addr: u64, target: u64) -> WireResult<u64> {
        Ok(target.wrapping_sub(ptr_addr))
    }

    /// The absolute target offset, or `None` when null.
    #[inline]
    pub fn target(&self) -> Option<u64> {
        match self {
            Pointer::Null => None,
            Pointer::Offset(o) => Some(*o),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn null_is_zero() {
        assert_eq!(Pointer::decode(16, 0).unwrap(), Pointer::Null);
    }

    #[test]
    fn roundtrip_forward_and_backward() {
        // Pointer at offset 16 pointing forward to offset 24.
        let raw = Pointer::encode(16, 24).unwrap();
        assert_eq!(raw, 8);
        assert_eq!(Pointer::decode(16, raw).unwrap(), Pointer::Offset(24));

        // Pointer at offset 32 pointing backward to offset 24.
        let raw = Pointer::encode(32, 24).unwrap();
        assert_eq!(raw, (24u64).wrapping_sub(32)); // negative offset wraps per two's complement
        assert_eq!(Pointer::decode(32, raw).unwrap(), Pointer::Offset(24));
    }

    #[test]
    fn wrapping_matches_official_pointer_arithmetic() {
        // encode/decode wrap like the official EncodePointer/DecodePointer;
        // invalid targets are caught by bounds/claim checks downstream.
        let raw = Pointer::encode(32, 24).unwrap();
        assert_eq!(Pointer::decode(32, raw).unwrap(), Pointer::Offset(24));
        let raw = Pointer::encode(0, u64::MAX).unwrap();
        assert_eq!(Pointer::decode(0, raw).unwrap(), Pointer::Offset(u64::MAX));
    }
}
