//! Idiomatic safe Rust shared-buffer API (Phase 4).
//!
//! Wraps the core `SharedBuffer` with ownership-enforcing types:
//!
//! * `SharedBuffer` handles are `Clone` (a duplicate handle referencing the
//!   same region; the read-only state machine is enforced by the core).
//! * `Mapping` is RAII: the region stays mapped until the `Mapping` is
//!   dropped (unmap-on-drop), so a mapping can never outlive the buffer or
//!   leak.
//! * Shared reads are safe (`bytes`, `read_at`). Writes require `unsafe`
//!   (`write_at`): shared memory is inherently aliased across handles and
//!   processes, so the type system cannot prove the absence of concurrent
//!   mutable access to the same pages — the caller must synchronize.

use std::sync::Arc;

use mojo_rs_core::shared_buffer::{BufferMapping, SharedBuffer as CoreSharedBuffer};

use crate::error::{SystemError, SystemResult};

/// A shared buffer handle. Duplicates reference the same underlying region.
#[derive(Clone)]
pub struct SharedBuffer {
    buffer: Arc<CoreSharedBuffer>,
}

impl SharedBuffer {
    /// Create a writable shared buffer of `num_bytes` bytes.
    pub fn create(num_bytes: u64) -> SystemResult<SharedBuffer> {
        Ok(SharedBuffer {
            buffer: CoreSharedBuffer::create(num_bytes)?,
        })
    }

    /// Duplicate this handle. With `read_only`, the duplicate (and all future
    /// duplicates of the converted region) is read-only; the official state
    /// machine (Writable → Unsafe/ReadOnly on first duplicate, then immutable)
    /// is enforced by the core.
    pub fn duplicate(&self, read_only: bool) -> SystemResult<SharedBuffer> {
        Ok(SharedBuffer {
            buffer: self.buffer.duplicate(read_only)?,
        })
    }

    /// The size of the buffer in bytes.
    pub fn size(&self) -> u64 {
        self.buffer.size()
    }

    /// Map `num_bytes` bytes at byte `offset`. The mapping is unmapped when
    /// the returned `Mapping` is dropped. Invalid ranges return
    /// `ResourceExhausted` (matching the official C entry, which maps every
    /// map failure to `MOJO_RESULT_RESOURCE_EXHAUSTED`).
    pub fn map(&self, offset: u64, num_bytes: u64) -> SystemResult<Mapping> {
        let mapping = self.buffer.map(offset, num_bytes)?;
        Ok(Mapping { inner: mapping })
    }
}

/// An active mapping of a shared buffer. Unmapped on drop.
pub struct Mapping {
    inner: BufferMapping,
}

impl Mapping {
    /// The start address of the mapping (informational; addresses are
    /// process-specific and never part of any wire contract).
    pub fn address(&self) -> usize {
        self.inner.address()
    }

    /// The mapped length.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the mapping is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The mapped bytes as a shared slice.
    ///
    /// SAFETY note: the caller must not concurrently write to the same pages
    /// through another mapping (or another process) while this slice is
    /// alive; shared memory is inherently aliased.
    pub fn bytes(&self) -> &[u8] {
        self.inner.bytes()
    }

    /// Read `len` bytes at byte `offset` within the mapping.
    pub fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        self.inner.read_at(offset, len)
    }

    /// Write `content` at byte `offset` within the mapping.
    ///
    /// SAFETY: the caller must ensure the mapping is writable (not derived
    /// from a read-only region) and that no other live reference (in this
    /// process or another) aliases the target bytes while this write is in
    /// flight. Returns `false` for read-only mappings and out-of-range
    /// writes.
    pub unsafe fn write_at(&mut self, offset: usize, content: &[u8]) -> bool {
        // SAFETY: delegated to the core mapping, which bounds-checks; the
        // caller upholds the no-aliasing precondition documented above.
        unsafe { self.inner.write_at(offset, content) }
    }
}

impl std::fmt::Debug for SharedBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedBuffer")
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Mapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mapping")
            .field("address", &format_args!("{:#x}", self.address()))
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

fn _assert_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SharedBuffer>();
    assert_send_sync::<Mapping>();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn create_duplicate_map_shared_pages() {
        let b = SharedBuffer::create(64).unwrap();
        assert_eq!(b.size(), 64);
        let d = b.duplicate(false).unwrap();
        let mut m0 = b.map(0, 16).unwrap();
        // SAFETY: the mapping is exclusively owned and writable; no aliases.
        unsafe {
            m0.write_at(0, &[1, 2, 3, 4]).then_some(()).unwrap();
        }
        let m1 = d.map(0, 16).unwrap();
        assert_eq!(m1.read_at(0, 4), Some(vec![1, 2, 3, 4]));
    }

    #[test]
    fn read_only_duplicate_roundtrip() {
        let b = SharedBuffer::create(64).unwrap();
        let ro = b.duplicate(true).unwrap();
        // The original converted to ReadOnly: writable duplicates now fail.
        assert_eq!(
            b.duplicate(false).unwrap_err(),
            SystemError::FailedPrecondition
        );
        let _m = ro.map(0, 16).unwrap();
        // Writes to a read-only mapping are rejected without faulting.
        let mut m = ro.map(0, 16).unwrap();
        // SAFETY: write_at returns false for read-only mappings.
        assert!(!unsafe { m.write_at(0, &[1]) });
    }

    #[test]
    fn mapping_unmaps_on_drop() {
        let b = SharedBuffer::create(4096).unwrap();
        {
            let m = b.map(0, 128).unwrap();
            assert_eq!(m.len(), 128);
        }
        // Dropped: a fresh mapping of the same range still works (the region
        // itself is independent of any single mapping's lifetime).
        let m2 = b.map(0, 128).unwrap();
        assert_eq!(m2.len(), 128);
    }

    #[test]
    fn invalid_ranges() {
        let b = SharedBuffer::create(64).unwrap();
        assert_eq!(b.map(0, 0).unwrap_err(), SystemError::ResourceExhausted);
        assert_eq!(b.map(0, 65).unwrap_err(), SystemError::ResourceExhausted);
        assert_eq!(b.map(64, 1).unwrap_err(), SystemError::ResourceExhausted);
    }
}
