//! Shared buffers: memfd-backed regions with duplicate/map/unmap semantics.
//!
//! Mirrors the pinned epoch's `mojo/core/ipcz_driver/shared_buffer.{h,cc}` and
//! `MojoCreateSharedBufferIpcz`/`MojoDuplicateBufferHandleIpcz`/
//! `MojoMapBufferIpcz`/`MojoUnmapBufferIpcz`/`MojoGetBufferInfoIpcz`
//! (Chromium 151.0.7922.105, CoreIpcz architecture):
//!
//! * `create` rejects `num_bytes == 0` and sizes above `i32::MAX` with
//!   `ResourceExhausted` (official `PlatformSharedMemoryRegion::Create`).
//! * The region mode state machine (`Writable` → `Unsafe`/`ReadOnly` on the
//!   first duplicate, then immutable) exactly mirrors the official
//!   `SharedBuffer::Duplicate`: converting mode on a `Writable` region, then
//!   requiring the target mode for every subsequent duplicate
//!   (`FailedPrecondition` on mismatch).
//! * `map` mirrors `base::subtle::PlatformSharedMemoryRegion::MapAt`:
//!   zero-length or out-of-range requests fail (reported as
//!   `ResourceExhausted`, matching the official `MojoMapBufferIpcz` which maps
//!   every `MapAt` failure to `RESOURCE_EXHAUSTED`); arbitrary offsets are
//!   supported (page-aligned down with pointer adjustment).
//! * Unmapping is by address via a process-wide table (official
//!   `MappingTable`); unmapping an unknown address returns `InvalidArgument`.
//!
//! Read-only regions map `PROT_READ`; writable/unsafe regions map
//! `PROT_READ|PROT_WRITE`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mojo_rs_platform::shm::{Access, Mapping, SharedMemory};

use crate::dispatcher::{Dispatcher, DispatcherType, WatchId};
use crate::error::{CoreError, CoreResult};
use crate::signal::{Signals, SignalsState};
use crate::trap::WatchCallback;

/// The access mode of a shared-buffer region (official
/// `base::subtle::PlatformSharedMemoryRegion::Mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferMode {
    /// Created writable; only this mode may be converted by duplication.
    Writable,
    /// Duplicated without the read-only flag: writable but no longer
    /// convertible to read-only.
    Unsafe,
    /// Read-only (duplicated with the read-only flag, or derived from it).
    ReadOnly,
}

/// A shared buffer dispatcher: one per logical buffer object. All handles
/// duplicated from a buffer share the same dispatcher's mode state.
pub struct SharedBuffer {
    inner: Mutex<BufferInner>,
}

impl std::fmt::Debug for SharedBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedBuffer")
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

struct BufferInner {
    shm: SharedMemory,
    size: u64,
    mode: BufferMode,
}

impl SharedBuffer {
    /// Create a writable shared buffer of `num_bytes` (official
    /// `MojoCreateSharedBufferIpcz`). Returns `ResourceExhausted` for
    /// zero-size or oversized requests and for backing-allocation failure.
    pub fn create(num_bytes: u64) -> CoreResult<Arc<SharedBuffer>> {
        // Official PlatformSharedMemoryRegion::Create: size 0 and sizes above
        // INT_MAX yield an invalid region (RESOURCE_EXHAUSTED at the C entry).
        if num_bytes == 0 || num_bytes > i32::MAX as u64 {
            return Err(CoreError::ResourceExhausted);
        }
        let shm = SharedMemory::create("mojo-rs-shared-buffer", num_bytes as usize)
            .map_err(|_| CoreError::ResourceExhausted)?;
        Ok(Arc::new(SharedBuffer {
            inner: Mutex::new(BufferInner {
                shm,
                size: num_bytes,
                mode: BufferMode::Writable,
            }),
        }))
    }

    /// Duplicate this buffer (official `SharedBuffer::Duplicate`). The
    /// returned buffer is a new dispatcher over a duplicated descriptor of the
    /// same region.
    ///
    /// Mode state machine (externally observable):
    /// * A `Writable` region is converted on first duplicate: to `Unsafe`
    ///   without the read-only flag, to `ReadOnly` with it.
    /// * Afterwards, duplicating with the wrong flag returns
    ///   `FailedPrecondition`.
    /// * Descriptor duplication failure returns `ResourceExhausted`.
    pub fn duplicate(&self, read_only: bool) -> CoreResult<Arc<SharedBuffer>> {
        let mut inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        if inner.mode == BufferMode::Writable {
            // The official conversion can fail (fd duplication); in-process it
            // succeeds as long as the descriptor can be duplicated, which is
            // checked below.
            inner.mode = if read_only {
                BufferMode::ReadOnly
            } else {
                BufferMode::Unsafe
            };
        }
        let required = if read_only {
            BufferMode::ReadOnly
        } else {
            BufferMode::Unsafe
        };
        if inner.mode != required {
            return Err(CoreError::FailedPrecondition);
        }
        let new_shm = inner
            .shm
            .duplicate()
            .map_err(|_| CoreError::ResourceExhausted)?;
        let size = inner.size;
        let mode = inner.mode;
        drop(inner);
        Ok(Arc::new(SharedBuffer {
            inner: Mutex::new(BufferInner {
                shm: new_shm,
                size,
                mode,
            }),
        }))
    }

    /// The size of the buffer in bytes (official `MojoGetBufferInfoIpcz`).
    pub fn size(&self) -> u64 {
        self.inner.lock().map(|i| i.size).unwrap_or(0)
    }

    /// Map `num_bytes` at byte offset `offset` (official
    /// `MojoMapBufferIpcz`). Any invalid range (zero length, overflow, beyond
    /// the buffer) or mapping failure returns `ResourceExhausted` — the
    /// official maps every `MapAt` failure to `MOJO_RESULT_RESOURCE_EXHAUSTED`.
    ///
    /// The mapping access follows the region mode: read-only regions map
    /// `PROT_READ`; writable and unsafe regions map `PROT_READ|PROT_WRITE`.
    pub fn map(&self, offset: u64, num_bytes: u64) -> CoreResult<BufferMapping> {
        let inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        let access = match inner.mode {
            BufferMode::ReadOnly => Access::ReadOnly,
            BufferMode::Writable | BufferMode::Unsafe => Access::ReadWrite,
        };
        let mapping = inner
            .shm
            .map(offset as usize, num_bytes as usize, access)
            .map_err(|_| CoreError::ResourceExhausted)?;
        let address = mapping.address();
        let len = mapping.len();
        Ok(BufferMapping {
            mapping,
            address,
            len,
        })
    }
}

/// An active mapping of a shared buffer. The mapping stays valid until
/// dropped (unmapped); `MojoMapBuffer` semantics require unmap by address via
/// `MappingTable`.
pub struct BufferMapping {
    mapping: Mapping,
    address: usize,
    len: usize,
}

impl std::fmt::Debug for BufferMapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferMapping")
            .field("address", &format_args!("{:#x}", self.address))
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl BufferMapping {
    /// The start address of the mapping.
    pub fn address(&self) -> usize {
        self.address
    }

    /// The mapped length.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The mapped bytes (shared view).
    pub fn bytes(&self) -> &[u8] {
        &self.mapping[..]
    }

    /// Read `len` bytes at byte `offset` within the mapping.
    pub fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        self.mapping
            .get(offset..offset.checked_add(len)?)
            .map(<[u8]>::to_vec)
    }

    /// Write `content` at byte `offset` within the mapping.
    ///
    /// SAFETY: the caller must ensure no aliasing mutable access to the target
    /// bytes exists. The mapping itself rejects writes to read-only regions
    /// and out-of-range writes (returns false) instead of faulting.
    pub unsafe fn write_at(&mut self, offset: usize, content: &[u8]) -> bool {
        if !self.mapping.is_writable() {
            return false;
        }
        let Some(end) = offset.checked_add(content.len()) else {
            return false;
        };
        if end > self.len {
            return false;
        }
        // SAFETY: the mapping is writable and bounds were checked above; the
        // caller upholds the no-aliasing precondition.
        unsafe {
            self.mapping[offset..end].copy_from_slice(content);
        }
        true
    }
}

/// The process-wide mapping table (official `MappingTable`): maps buffer
/// addresses to their owning mappings so `MojoUnmapBuffer(address)` can
/// release them. Ownership is explicit: the table owns every `BufferMapping`
/// until it is removed.
pub struct MappingTable {
    inner: Mutex<HashMap<usize, Arc<BufferMapping>>>,
}

impl MappingTable {
    /// An empty table.
    pub fn new() -> MappingTable {
        MappingTable {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Insert a mapping and return its address (official `MappingTable::Add`).
    pub fn add(&self, mapping: Arc<BufferMapping>) -> usize {
        let address = mapping.address();
        if let Ok(mut inner) = self.inner.lock() {
            inner.insert(address, mapping);
        }
        address
    }

    /// Remove and drop the mapping at `address` (official `MappingTable::Remove`):
    /// `InvalidArgument` when the address is not mapped.
    pub fn remove(&self, address: usize) -> CoreResult<()> {
        let mut inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        if inner.remove(&address).is_none() {
            return Err(CoreError::InvalidArgument);
        }
        Ok(())
    }

    /// Look up the mapping at `address` (keeps it alive).
    pub fn get(&self, address: usize) -> Option<Arc<BufferMapping>> {
        self.inner
            .lock()
            .ok()
            .and_then(|i| i.get(&address).cloned())
    }
}

impl Default for MappingTable {
    fn default() -> MappingTable {
        MappingTable::new()
    }
}

impl Dispatcher for SharedBuffer {
    fn dispatcher_type(&self) -> DispatcherType {
        DispatcherType::SharedBuffer
    }

    fn is_duplicable(&self) -> bool {
        true
    }

    fn query_signals(&self) -> SignalsState {
        // The official MojoQueryHandleSignalsStateIpcz returns
        // MOJO_RESULT_INVALID_ARGUMENT for boxed driver objects other than
        // data pipes; the shared buffer's signal state is therefore empty and
        // the C entry layer rejects queries. The dispatcher surface reports an
        // empty state and the harness maps the handle kind itself.
        SignalsState::default()
    }

    fn on_closed(&self) {
        // Dropping the region closes the descriptors; nothing else to do
        // (official `SharedBuffer::Close` clears the region).
    }

    fn start_watch(&self, _signals: Signals, _callback: WatchCallback) -> WatchId {
        WatchId::new(0)
    }

    fn cancel_watch(&self, _id: WatchId) {}

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn create_rejects_invalid_sizes() {
        assert_eq!(
            SharedBuffer::create(0).unwrap_err(),
            CoreError::ResourceExhausted
        );
        assert_eq!(
            SharedBuffer::create(i32::MAX as u64 + 1).unwrap_err(),
            CoreError::ResourceExhausted
        );
        let b = SharedBuffer::create(4096).unwrap();
        assert_eq!(b.size(), 4096);
    }

    #[test]
    fn duplicate_mode_state_machine() {
        let b = SharedBuffer::create(64).unwrap();
        // First non-read-only duplicate converts Writable -> Unsafe.
        let d1 = b.duplicate(false).unwrap();
        // Now read-only duplication of the original fails (Unsafe != ReadOnly).
        assert_eq!(
            b.duplicate(true).unwrap_err(),
            CoreError::FailedPrecondition
        );
        // Non-read-only duplication still works.
        let d2 = b.duplicate(false).unwrap();
        // A read-only buffer can only produce read-only duplicates.
        assert_eq!(
            d1.duplicate(true).unwrap_err(),
            CoreError::FailedPrecondition
        );
        let _ = d1.duplicate(false).unwrap();
        let _ = d2;

        let ro = SharedBuffer::create(64).unwrap();
        let ro1 = ro.duplicate(true).unwrap();
        // Read-only duplicate of the (now ReadOnly) original.
        let _ = ro.duplicate(true).unwrap();
        // Non-read-only of a read-only buffer fails.
        assert_eq!(
            ro1.duplicate(false).unwrap_err(),
            CoreError::FailedPrecondition
        );
    }

    #[test]
    fn map_unmap_and_shared_pages() {
        let b = SharedBuffer::create(4096).unwrap();
        let m = b.map(0, 128).unwrap();
        assert_eq!(m.len(), 128);
        // The mapping is writable (Writable mode) and exclusively owned here.
        let mut owned = Arc::new(m);
        // SAFETY: the mapping is writable and exclusively owned (sole Arc);
        // write_at bounds-checks the range.
        unsafe {
            Arc::get_mut(&mut owned).unwrap().write_at(4, &[0xde, 0xad]);
        }
        // A duplicate sees the same pages.
        let d = b.duplicate(false).unwrap();
        let dm = d.map(0, 128).unwrap();
        assert_eq!(dm.read_at(4, 2), Some(vec![0xde, 0xad]));
    }

    #[test]
    fn map_range_validation() {
        let b = SharedBuffer::create(4096).unwrap();
        assert_eq!(b.map(0, 0).unwrap_err(), CoreError::ResourceExhausted);
        assert_eq!(b.map(0, 4097).unwrap_err(), CoreError::ResourceExhausted);
        assert_eq!(b.map(4095, 2).unwrap_err(), CoreError::ResourceExhausted);
        assert_eq!(b.map(4096, 1).unwrap_err(), CoreError::ResourceExhausted);
        // Unaligned offset is supported (MapAt aligns down).
        let m = b.map(3, 10).unwrap();
        assert_eq!(m.len(), 10);
        assert_eq!(m.address() % 4096, 3);
    }

    #[test]
    fn mapping_table_ownership() {
        let t = MappingTable::new();
        let b = SharedBuffer::create(64).unwrap();
        let m = Arc::new(b.map(0, 64).unwrap());
        let addr = t.add(Arc::clone(&m));
        assert_eq!(addr, m.address());
        assert!(t.get(addr).is_some());
        assert_eq!(t.remove(addr), Ok(()));
        assert!(t.get(addr).is_none());
        assert_eq!(t.remove(addr).unwrap_err(), CoreError::InvalidArgument);
    }
}
