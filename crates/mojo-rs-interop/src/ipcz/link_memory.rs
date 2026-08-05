//! NodeLinkMemory — the shared primary buffer exchanged during the Connect
//! handshake, and the fragment machinery built on it.
//!
//! Layout of the primary buffer (BufferId 0, 128 KiB), matching
//! `NodeLinkMemory::PrimaryBuffer` in the pinned epoch:
//!
//! ```text
//! 0x000  PrimaryBufferHeader { next_buffer_id u64, next_sublink_id u64 }
//!       + padding to 256 bytes
//! 0x100  InitialRouterLinkStateArray: 12 x RouterLinkState (64 B each);
//!       initial portal i uses the state at 0x100 + i*64
//! 0x400  64-byte BlockAllocator region (64 x 1484 blocks; block 0 is the
//!       free-list header, allocable blocks start at block 1)
//! ...    256/512/1k/2k/4k block regions
//! 0x20000 end
//! ```
//!
//! Fragments are resolved against persistent mappings owned by this object.
//! Mutable access requires `&mut self`; shared words that both processes
//! access concurrently (`RouterLinkState.status`, parcel `FragmentHeader`)
//! use `AtomicU32` with the same relaxed/acquire-release semantics as the
//! official implementation. All access is bounds-checked; a malformed
//! descriptor is a `LinkMemoryError`, never an out-of-bounds access.

use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use mojo_rs_platform::shm::{Access, Mapping, SharedMemory};

use crate::ipcz::messages::FragmentDescriptor;

/// The size of the primary buffer (128 KiB), matching `kPrimaryBufferSize`.
pub const PRIMARY_BUFFER_SIZE: usize = 128 * 1024;
/// Reserved header region size (`kPrimaryBufferReservedHeaderSize`).
pub const PRIMARY_BUFFER_HEADER_SIZE: usize = 256;
/// The maximum number of initial portals (`kMaxInitialPortals`).
pub const MAX_INITIAL_PORTALS: usize = 12;
/// The size of a `RouterLinkState` (64 bytes).
pub const ROUTER_LINK_STATE_SIZE: usize = 64;
/// The offset of the initial link-state array.
pub const INITIAL_LINK_STATES_OFFSET: usize = 0x100;
/// The offset of the 64-byte block allocator region.
pub const BLOCK_ALLOCATOR_64_OFFSET: usize = 0x400;
/// The number of blocks in the 64-byte allocator region.
pub const BLOCK_ALLOCATOR_64_BLOCKS: usize = 1484;
/// The size of a parcel fragment header (`FragmentHeader`): reserved + size.
pub const FRAGMENT_HEADER_SIZE: usize = 8;
/// The offset of the 256-byte block allocator region (after the 64-byte region).
pub const BLOCK_ALLOCATOR_256_OFFSET: usize = 0x17700;
/// The number of blocks in the 256-byte allocator region.
pub const BLOCK_ALLOCATOR_256_BLOCKS: usize = 9;
/// The offset of the 512-byte block allocator region.
pub const BLOCK_ALLOCATOR_512_OFFSET: usize = 0x18000;
/// The number of blocks in the 512-byte allocator region.
pub const BLOCK_ALLOCATOR_512_BLOCKS: usize = 8;
/// The offset of the 1024-byte block allocator region.
pub const BLOCK_ALLOCATOR_1K_OFFSET: usize = 0x19000;
/// The number of blocks in the 1024-byte allocator region.
pub const BLOCK_ALLOCATOR_1K_BLOCKS: usize = 4;
/// The offset of the 2048-byte block allocator region.
pub const BLOCK_ALLOCATOR_2K_OFFSET: usize = 0x1A000;
/// The number of blocks in the 2048-byte allocator region.
pub const BLOCK_ALLOCATOR_2K_BLOCKS: usize = 4;
/// The offset of the 4096-byte block allocator region.
pub const BLOCK_ALLOCATOR_4K_OFFSET: usize = 0x1C000;
/// The number of blocks in the 4096-byte allocator region.
pub const BLOCK_ALLOCATOR_4K_BLOCKS: usize = 4;
/// The primary buffer's fixed block allocator regions, in layout order:
/// `(block_size, region offset, block count)` (`NodeLinkMemory::PrimaryBuffer`).
pub const PRIMARY_BLOCK_REGIONS: &[(u32, usize, usize)] = &[
    (64, BLOCK_ALLOCATOR_64_OFFSET, BLOCK_ALLOCATOR_64_BLOCKS),
    (256, BLOCK_ALLOCATOR_256_OFFSET, BLOCK_ALLOCATOR_256_BLOCKS),
    (512, BLOCK_ALLOCATOR_512_OFFSET, BLOCK_ALLOCATOR_512_BLOCKS),
    (1024, BLOCK_ALLOCATOR_1K_OFFSET, BLOCK_ALLOCATOR_1K_BLOCKS),
    (2048, BLOCK_ALLOCATOR_2K_OFFSET, BLOCK_ALLOCATOR_2K_BLOCKS),
    (4096, BLOCK_ALLOCATOR_4K_OFFSET, BLOCK_ALLOCATOR_4K_BLOCKS),
];
/// The allocator page size for new block buffers (`kBlockAllocatorPageSize`).
pub const BLOCK_ALLOCATOR_PAGE_SIZE: usize = 64 * 1024;
/// The minimum number of blocks a block buffer must hold
/// (`kMinBlockAllocatorCapacity`).
pub const MIN_BLOCK_ALLOCATOR_CAPACITY: usize = 8;
/// The smallest allocable fragment size (`kMinFragmentSize`).
pub const MIN_FRAGMENT_SIZE: usize = 64;
/// The largest fragment size served by the block allocators
/// (`kMaxFragmentSizeForBlockAllocation`).
pub const MAX_FRAGMENT_SIZE_FOR_BLOCK_ALLOCATION: usize = 1024 * 1024;
/// The maximum total block capacity per fragment size before further expansion
/// is refused (`kMaxBlockAllocatorCapacityPerFragmentSize`).
pub const MAX_BLOCK_ALLOCATOR_CAPACITY_PER_FRAGMENT_SIZE: usize = 2 * 1024 * 1024;

/// The block size class for a fragment of `size` bytes
/// (`GetBlockSizeForFragmentSize`: at least the minimum, rounded up to a power
/// of two).
pub fn get_block_size_for_fragment_size(size: usize) -> u32 {
    std::cmp::max(MIN_FRAGMENT_SIZE, size.next_power_of_two()) as u32
}

/// The primary buffer id.
pub const PRIMARY_BUFFER_ID: u64 = 0;

/// A 4-byte block allocator header (`BlockAllocator::BlockHeader`):
/// `version u16` + `next i16` (relative to the block's successor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockHeader {
    version: u16,
    next: i16,
}

/// Errors from fragment resolution and link-memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMemoryError {
    /// The descriptor references an unknown buffer id.
    UnknownBuffer,
    /// The descriptor range exceeds the buffer bounds.
    OutOfBounds,
    /// The descriptor is not 8-byte aligned.
    Misaligned,
    /// The descriptor is null.
    NullDescriptor,
}

impl std::fmt::Display for LinkMemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkMemoryError::UnknownBuffer => write!(f, "unknown buffer id"),
            LinkMemoryError::OutOfBounds => write!(f, "fragment out of bounds"),
            LinkMemoryError::Misaligned => write!(f, "fragment misaligned"),
            LinkMemoryError::NullDescriptor => write!(f, "null fragment descriptor"),
        }
    }
}

/// RouterLinkState status bits (router_link_state.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterLinkStatus(pub u32);

impl RouterLinkStatus {
    /// Fresh central link, sides unstable.
    pub const UNSTABLE: u32 = 0;
    /// Side A has no decaying links.
    pub const SIDE_A_STABLE: u32 = 1 << 0;
    /// Side B has no decaying links.
    pub const SIDE_B_STABLE: u32 = 1 << 1;
    /// Both sides stable.
    pub const STABLE: u32 = Self::SIDE_A_STABLE | Self::SIDE_B_STABLE;
    /// Side A is waiting on stability.
    pub const SIDE_A_WAITING: u32 = 1 << 2;
    /// Side B is waiting on stability.
    pub const SIDE_B_WAITING: u32 = 1 << 3;
    /// The link is locked by side A.
    pub const LOCKED_BY_SIDE_A: u32 = 1 << 4;
    /// The link is locked by side B.
    pub const LOCKED_BY_SIDE_B: u32 = 1 << 5;

    /// The raw status value.
    pub fn value(self) -> u32 {
        self.0
    }

    /// Whether the side A stable bit is set.
    pub fn side_a_stable(self) -> bool {
        self.0 & Self::SIDE_A_STABLE != 0
    }

    /// Whether the side B stable bit is set.
    pub fn side_b_stable(self) -> bool {
        self.0 & Self::SIDE_B_STABLE != 0
    }

    /// Whether both stable bits are set.
    pub fn is_stable(self) -> bool {
        self.0 & Self::STABLE == Self::STABLE
    }
}

/// A buffer held by the link: the object plus a persistent mapping.
struct Buffer {
    memory: SharedMemory,
    mapping: Mapping,
}

impl Buffer {
    /// Adopt a descriptor of `size` bytes with a read-write mapping.
    fn adopt(fd: RawFd, size: usize) -> std::io::Result<Buffer> {
        let memory = SharedMemory::from_raw_fd(fd, size)?;
        let mapping = memory.map(0, size, Access::ReadWrite)?;
        Ok(Buffer { memory, mapping })
    }
}

/// An additional block buffer adopted via `AddBlockBuffer`: a whole buffer
/// backing exactly one block size (the official `NodeLinkMemory::AddBlockBuffer`
/// registers the buffer with a single `BlockAllocator` of `block_size` blocks).
struct ExtraBuffer {
    memory: SharedMemory,
    mapping: Mapping,
    /// The block size this buffer backs.
    block_size: u32,
}

/// The shared link memory: the mapped primary buffer plus any additional
/// buffers added later (AddBlockBuffer).
pub struct LinkMemory {
    /// The mapped primary buffer (BufferId 0).
    primary: Buffer,
    /// Additional buffers by buffer id.
    extra: std::collections::HashMap<u64, ExtraBuffer>,
    /// Extra buffer ids in insertion order (deterministic pool iteration).
    extra_order: Vec<u64>,
}

impl LinkMemory {
    /// Adopt the transferred primary buffer (BufferId 0).
    ///
    /// `fd` is the memfd received in the Connect message; ownership is
    /// transferred. The buffer is mapped read-write (mode `kUnsafe`).
    pub fn adopt_primary(fd: RawFd) -> Result<LinkMemory, std::io::Error> {
        Ok(LinkMemory {
            primary: Buffer::adopt(fd, PRIMARY_BUFFER_SIZE)?,
            extra: std::collections::HashMap::new(),
            extra_order: Vec::new(),
        })
    }

    /// Add a block buffer (`AddBlockBuffer`, receive side) to the pool.
    ///
    /// The buffer is adopted at the descriptor's real size (`fstat`): the
    /// message carries the block size, not the buffer size. The region is
    /// NOT re-initialized: the sender initializes the block allocator region
    /// before sharing it (matching the official `OnAddBlockBuffer`, which
    /// adopts the mapping and registers the allocator as-is).
    pub fn add_block_buffer(
        &mut self,
        id: u64,
        fd: RawFd,
        block_size: u32,
    ) -> Result<(), std::io::Error> {
        if id == crate::ipcz::messages::FragmentDescriptor::INVALID_BUFFER_ID {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid buffer id",
            ));
        }
        if block_size < MIN_FRAGMENT_SIZE as u32 || block_size & 7 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid block size",
            ));
        }
        let memory = SharedMemory::from_fd(fd)?;
        let size = memory.size();
        if size / (block_size as usize) < MIN_BLOCK_ALLOCATOR_CAPACITY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "buffer too small for the block size",
            ));
        }
        let mapping = memory.map(0, size, Access::ReadWrite)?;
        if self.extra.contains_key(&id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "duplicate buffer id",
            ));
        }
        self.extra_order.push(id);
        self.extra.insert(
            id,
            ExtraBuffer {
                memory,
                mapping,
                block_size,
            },
        );
        Ok(())
    }

    /// Register a whole-buffer block allocator locally (the send side of a
    /// capacity expansion, mirroring `self->AddBlockBuffer` in
    /// `NodeLinkMemory::RequestBlockCapacity`): adopt the descriptor, validate
    /// the block size, initialize the region (`BlockAllocator::InitializeRegion`),
    /// and insert it into the pool. The caller must have already transmitted
    /// the `AddBlockBuffer` message carrying a duplicate of the descriptor.
    pub fn register_block_buffer(
        &mut self,
        id: u64,
        fd: RawFd,
        block_size: u32,
    ) -> Result<(), std::io::Error> {
        if block_size < MIN_FRAGMENT_SIZE as u32 || block_size & 7 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid block size",
            ));
        }
        let memory = SharedMemory::from_fd(fd)?;
        let size = memory.size();
        if size / (block_size as usize) < MIN_BLOCK_ALLOCATOR_CAPACITY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "buffer too small for the block size",
            ));
        }
        let mut mapping = memory.map(0, size, Access::ReadWrite)?;
        // `BlockAllocator::InitializeRegion`: zeroing the region makes every
        // block point at its successor; the last block points back to block 0.
        for b in mapping.iter_mut() {
            *b = 0;
        }
        let num_blocks = size / block_size as usize;
        let last = (num_blocks - 1) * block_size as usize;
        mapping[last..last + 2].copy_from_slice(&0u16.to_le_bytes());
        mapping[last + 2..last + 4]
            .copy_from_slice(&(0i16.wrapping_sub(num_blocks as i16)).to_le_bytes());
        self.extra_order.push(id);
        self.extra.insert(
            id,
            ExtraBuffer {
                memory,
                mapping,
                block_size,
            },
        );
        Ok(())
    }

    /// Resolve a fragment descriptor to a read view of the shared memory.
    pub fn fragment(&self, desc: FragmentDescriptor) -> Result<&[u8], LinkMemoryError> {
        if desc.is_null() {
            return Err(LinkMemoryError::NullDescriptor);
        }
        if desc.offset % 8 != 0 {
            return Err(LinkMemoryError::Misaligned);
        }
        let buffer = self.buffer_for(desc.buffer_id)?;
        let end = (desc.offset as usize)
            .checked_add(desc.size as usize)
            .ok_or(LinkMemoryError::OutOfBounds)?;
        if end > buffer.len() {
            return Err(LinkMemoryError::OutOfBounds);
        }
        Ok(&buffer[desc.offset as usize..end])
    }

    /// Resolve a fragment descriptor to a mutable view.
    pub fn fragment_mut(&mut self, desc: FragmentDescriptor) -> Result<&mut [u8], LinkMemoryError> {
        if desc.is_null() {
            return Err(LinkMemoryError::NullDescriptor);
        }
        if desc.offset % 8 != 0 {
            return Err(LinkMemoryError::Misaligned);
        }
        let buffer = self.buffer_for(desc.buffer_id)?;
        let end = (desc.offset as usize)
            .checked_add(desc.size as usize)
            .ok_or(LinkMemoryError::OutOfBounds)?;
        if end > buffer.len() {
            return Err(LinkMemoryError::OutOfBounds);
        }
        // SAFETY: `&mut self` grants exclusive access to this mapping, and the
        // range was validated. The Mapping is owned by this LinkMemory.
        let ptr = unsafe { buffer.as_mut_ptr() };
        // SAFETY: the range [offset, offset+size) was validated against the
        // mapping length above; the pointer is derived from the owned mapping.
        Ok(unsafe {
            std::slice::from_raw_parts_mut(ptr.add(desc.offset as usize), desc.size as usize)
        })
    }

    fn buffer_for(&self, id: u64) -> Result<&Mapping, LinkMemoryError> {
        if id == PRIMARY_BUFFER_ID {
            Ok(&self.primary.mapping)
        } else {
            self.extra
                .get(&id)
                .map(|e| &e.mapping)
                .ok_or(LinkMemoryError::UnknownBuffer)
        }
    }

    /// The initial `RouterLinkState` offset for portal `i` (fixed location).
    pub fn initial_link_state_offset(i: usize) -> usize {
        INITIAL_LINK_STATES_OFFSET + i * ROUTER_LINK_STATE_SIZE
    }

    /// Whether a descriptor references one of the fixed initial-portal
    /// `RouterLinkState`s (the unmanaged `GetInitialRouterLinkState` refs,
    /// which are never refcounted or freed).
    pub fn is_initial_link_state(desc: FragmentDescriptor) -> bool {
        let initial_states_end =
            INITIAL_LINK_STATES_OFFSET + (MAX_INITIAL_PORTALS * ROUTER_LINK_STATE_SIZE);
        if desc.buffer_id != PRIMARY_BUFFER_ID {
            return false;
        }
        let off = desc.offset as usize;
        off >= INITIAL_LINK_STATES_OFFSET && off < initial_states_end
    }

    /// The offset of the `status` word within a `RouterLinkState` (after the
    /// `RefCountedFragment` ref_count).
    pub const LINK_STATUS_OFFSET: usize = 4;

    /// The offset of `allowed_bypass_request_source` within a
    /// `RouterLinkState` (NodeName, 16 bytes, at offset 8).
    pub const LINK_ALLOWED_SOURCE_OFFSET: usize = 8;

    /// Allocate `count` contiguous sublink ids from the shared header
    /// allocator (`NodeLinkMemory::AllocateSublinkIds`: atomic fetch_add on
    /// `PrimaryBufferHeader.next_sublink_id`). Both sides allocate from the
    /// same counter, so ids never collide.
    pub fn allocate_sublink_ids(&self, count: u64) -> Result<u64, LinkMemoryError> {
        let view = self.fragment(FragmentDescriptor {
            buffer_id: PRIMARY_BUFFER_ID,
            offset: 8, // PrimaryBufferHeader.next_sublink_id
            size: 8,
        })?;
        // SAFETY: the header word is 8-byte aligned within the mapping and
        // shared with the peer process; all access is atomic on both sides.
        let counter = unsafe { AtomicU64::from_ptr(view.as_ptr() as *mut u64) };
        Ok(counter.fetch_add(count, Ordering::Relaxed))
    }

    /// Allocate a new buffer id from the shared primary header
    /// (`NodeLinkMemory::AllocateNewBufferId`: atomic fetch_add on
    /// `PrimaryBufferHeader.next_buffer_id`; mem_v2 is off in this epoch, so
    /// both sides draw from the same counter).
    pub fn allocate_new_buffer_id(&self) -> Result<u64, LinkMemoryError> {
        let view = self.fragment(FragmentDescriptor {
            buffer_id: PRIMARY_BUFFER_ID,
            offset: 0, // PrimaryBufferHeader.next_buffer_id
            size: 8,
        })?;
        // SAFETY: the header word is 8-byte aligned within the mapping and
        // shared with the peer process; all access is atomic on both sides.
        let counter = unsafe { AtomicU64::from_ptr(view.as_ptr() as *mut u64) };
        Ok(counter.fetch_add(1, Ordering::Relaxed))
    }

    /// Allocate a 64-byte block and initialize it as a fresh `RouterLinkState`
    /// (all zeros; `RouterLinkState::Initialize`, with the `RefCountedFragment`
    /// ref count set to 1 — the allocating side's initial ref). Returns `None`
    /// when the allocator is exhausted (`TryAllocateRouterLinkState` returns a
    /// null fragment in that case).
    pub fn try_allocate_link_state(
        &mut self,
    ) -> Result<Option<FragmentDescriptor>, LinkMemoryError> {
        let Some(block) = self.try_allocate_fragment(ROUTER_LINK_STATE_SIZE)? else {
            return Ok(None);
        };
        let view = self.fragment_mut(block)?;
        // RouterLinkState::Initialize default-constructs the struct: ref count
        // 1 (the allocator's ref), status 0, zeroed reserved fields.
        view[0..4].copy_from_slice(&1u32.to_le_bytes());
        for b in view[4..].iter_mut() {
            *b = 0;
        }
        Ok(Some(block))
    }

    /// Take a reference to a `RouterLinkState` fragment
    /// (`FragmentRef::operator=` / the `AddRemoteRouterLink` copy:
    /// `RefCountedFragment::AddRef`, relaxed fetch-add on the shared ref
    /// count at offset 0). Note that ADOPTION (`AdoptFragmentRefIfValid`)
    /// does NOT call this — it takes the sender's `release()`d ref.
    pub fn add_link_state_ref(&self, desc: FragmentDescriptor) -> Result<(), LinkMemoryError> {
        let view = self.fragment(desc)?;
        // SAFETY: the ref count word is 8-byte aligned within the mapping and
        // shared with the peer; all access is atomic on both sides.
        let ref_count = unsafe { AtomicU32::from_ptr(view.as_ptr() as *mut u32) };
        ref_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Release a reference to a `RouterLinkState` fragment
    /// (`GenericFragmentRef::reset`: `ReleaseRef`, acq_rel fetch-sub).
    /// Returns true when this was the LAST reference — the caller must free
    /// the fragment back to the pool (`memory->FreeFragment`).
    pub fn release_link_state_ref(
        &self,
        desc: FragmentDescriptor,
    ) -> Result<bool, LinkMemoryError> {
        let view = self.fragment(desc)?;
        // SAFETY: as in `add_link_state_ref`.
        let ref_count = unsafe { AtomicU32::from_ptr(view.as_ptr() as *mut u32) };
        let old = ref_count.fetch_sub(1, Ordering::AcqRel);
        Ok(old == 1)
    }

    /// The official `RouterLinkState::SetSideStable`: OR the side's stable bit
    /// into the status word with compare-exchange semantics.
    pub fn set_side_stable(
        &self,
        desc: FragmentDescriptor,
        side_a: bool,
    ) -> Result<(), LinkMemoryError> {
        let bit = if side_a {
            RouterLinkStatus::SIDE_A_STABLE
        } else {
            RouterLinkStatus::SIDE_B_STABLE
        };
        let view = self.fragment(desc)?;
        // SAFETY: the status word is 8-byte aligned within the mapping and
        // shared with the peer; all access is atomic on both sides.
        let status =
            unsafe { AtomicU32::from_ptr(view.as_ptr().add(Self::LINK_STATUS_OFFSET) as *mut u32) };
        // `SetSideStable`: compare-exchange with the `expected` value updated
        // from the observed status on each failure (Rust's CAS does not update
        // the `expected` argument, unlike C++'s reference parameter).
        let mut expected = 0u32;
        loop {
            match status.compare_exchange_weak(
                expected,
                expected | bit,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => {
                    expected = actual;
                    // Another (possibly concurrent) operation already set our
                    // stable bit; nothing left to do.
                    if (expected & bit) != 0 {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// The official `RouterLinkState::TryLock` (mirrors the compare-exchange
    /// loop exactly: lock when both sides are stable and unlocked; set the
    /// waiting bit when the other side is still unstable).
    pub fn try_lock_link_state(
        &self,
        desc: FragmentDescriptor,
        side_a: bool,
    ) -> Result<bool, LinkMemoryError> {
        use RouterLinkStatus as S;
        let (this_stable, other_stable, locked_by_this, locked_either, this_waiting) = if side_a {
            (
                S::SIDE_A_STABLE,
                S::SIDE_B_STABLE,
                S::LOCKED_BY_SIDE_A,
                S::LOCKED_BY_SIDE_A | S::LOCKED_BY_SIDE_B,
                S::SIDE_A_WAITING,
            )
        } else {
            (
                S::SIDE_B_STABLE,
                S::SIDE_A_STABLE,
                S::LOCKED_BY_SIDE_B,
                S::LOCKED_BY_SIDE_A | S::LOCKED_BY_SIDE_B,
                S::SIDE_B_WAITING,
            )
        };
        let view = self.fragment(desc)?;
        // SAFETY: the status word is 8-byte aligned within the mapping and
        // shared with the peer; all access is atomic on both sides.
        let status =
            unsafe { AtomicU32::from_ptr(view.as_ptr().add(Self::LINK_STATUS_OFFSET) as *mut u32) };
        // `TryLock`: `expected` is refreshed from the observed status on every
        // failed CAS, exactly like the C++ reference parameter.
        let mut expected = S::STABLE;
        let mut desired_bit = locked_by_this;
        loop {
            match status.compare_exchange_weak(
                expected,
                expected | desired_bit,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => {
                    expected = actual;
                    if (expected & locked_either) != 0 || (expected & this_stable) == 0 {
                        return Ok(false);
                    }
                    if desired_bit == locked_by_this && (expected & other_stable) == 0 {
                        // Trying to lock, but the other side isn't stable yet:
                        // set our waiting bit instead.
                        desired_bit = this_waiting;
                    } else if desired_bit == this_waiting && (expected & S::STABLE) == S::STABLE {
                        // Waiting, and the other side is now stable: go back to
                        // trying to lock the link.
                        desired_bit = locked_by_this;
                    }
                }
            }
        }
        Ok(desired_bit == locked_by_this)
    }

    /// The official `RouterLinkState::Unlock`: clear the side's lock bit while
    /// the link is stable.
    pub fn unlock_link_state(
        &self,
        desc: FragmentDescriptor,
        side_a: bool,
    ) -> Result<(), LinkMemoryError> {
        use RouterLinkStatus as S;
        let locked_by_this = if side_a {
            S::LOCKED_BY_SIDE_A
        } else {
            S::LOCKED_BY_SIDE_B
        };
        let view = self.fragment(desc)?;
        // SAFETY: as in try_lock_link_state.
        let status =
            unsafe { AtomicU32::from_ptr(view.as_ptr().add(Self::LINK_STATUS_OFFSET) as *mut u32) };
        // `Unlock`: refresh `expected` from the observed status each failure.
        let mut expected = S::STABLE | locked_by_this;
        let mut desired = S::STABLE;
        loop {
            match status.compare_exchange_weak(
                expected,
                desired,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => {
                    expected = actual;
                    // Someone else already unlocked (or never locked) the link.
                    if (expected & locked_by_this) == 0 {
                        break;
                    }
                    desired = expected & !locked_by_this;
                }
            }
        }
        Ok(())
    }

    /// The official `RouterLinkState::ResetWaitingBit`: clear `side`'s waiting
    /// bit when the link is stable, unlocked, and the bit is set.
    pub fn reset_waiting_bit(
        &self,
        desc: FragmentDescriptor,
        side_a: bool,
    ) -> Result<bool, LinkMemoryError> {
        use RouterLinkStatus as S;
        let this_waiting = if side_a {
            S::SIDE_A_WAITING
        } else {
            S::SIDE_B_WAITING
        };
        let locked_either = S::LOCKED_BY_SIDE_A | S::LOCKED_BY_SIDE_B;
        let view = self.fragment(desc)?;
        // SAFETY: as in try_lock_link_state.
        let status =
            unsafe { AtomicU32::from_ptr(view.as_ptr().add(Self::LINK_STATUS_OFFSET) as *mut u32) };
        // `ResetWaitingBit`: refresh `expected` from the observed status each
        // failure.
        let mut expected = S::STABLE | this_waiting;
        let mut desired = S::STABLE;
        loop {
            match status.compare_exchange_weak(
                expected,
                desired,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(true),
                Err(actual) => {
                    expected = actual;
                    if (expected & S::STABLE) != S::STABLE
                        || (expected & this_waiting) == 0
                        || (expected & locked_either) != 0
                    {
                        // Not stable, not waiting, or already locked: nothing
                        // to change.
                        return Ok(false);
                    }
                    desired = expected & !this_waiting;
                }
            }
        }
    }

    /// Read the `allowed_bypass_request_source` NodeName of a link state (the
    /// full 16-byte name: high at `LINK_ALLOWED_SOURCE_OFFSET`, low 8 bytes
    /// after it — `RouterLinkState::allowed_bypass_request_source`).
    pub fn read_allowed_bypass_source(
        &self,
        desc: FragmentDescriptor,
    ) -> Result<crate::ipcz::messages::NodeName, LinkMemoryError> {
        let view = self.fragment(desc)?;
        let off = Self::LINK_ALLOWED_SOURCE_OFFSET;
        let high = view.get(off..off + 8).ok_or(LinkMemoryError::OutOfBounds)?;
        let low = view
            .get(off + 8..off + 16)
            .ok_or(LinkMemoryError::OutOfBounds)?;
        Ok(crate::ipcz::messages::NodeName {
            high: u64::from_le_bytes(high.try_into().map_err(|_| LinkMemoryError::OutOfBounds)?),
            low: u64::from_le_bytes(low.try_into().map_err(|_| LinkMemoryError::OutOfBounds)?),
        })
    }

    /// Write the `allowed_bypass_request_source` NodeName (the full 16 bytes,
    /// matching `RouterLinkState::allowed_bypass_request_source` — the peer
    /// validates `CanNodeRequestBypass` against it).
    pub fn write_allowed_bypass_source(
        &mut self,
        desc: FragmentDescriptor,
        value: crate::ipcz::messages::NodeName,
    ) -> Result<(), LinkMemoryError> {
        let view = self.fragment_mut(desc)?;
        let off = Self::LINK_ALLOWED_SOURCE_OFFSET;
        let slot = view
            .get_mut(off..off + 16)
            .ok_or(LinkMemoryError::OutOfBounds)?;
        slot[..8].copy_from_slice(&value.high.to_le_bytes());
        slot[8..].copy_from_slice(&value.low.to_le_bytes());
        Ok(())
    }

    /// Read the status field of a `RouterLinkState` fragment.
    pub fn read_link_status(
        &self,
        desc: FragmentDescriptor,
    ) -> Result<RouterLinkStatus, LinkMemoryError> {
        if desc.size < ROUTER_LINK_STATE_SIZE as u32 {
            return Err(LinkMemoryError::OutOfBounds);
        }
        let view = self.fragment(desc)?;
        // SAFETY: the fragment is 8-byte aligned and within the mapping, and
        // the status word is at offset 4.
        let status =
            unsafe { AtomicU32::from_ptr(view.as_ptr().add(Self::LINK_STATUS_OFFSET) as *mut u32) };
        Ok(RouterLinkStatus(status.load(Ordering::Relaxed)))
    }

    /// OR `bits` into the status field of a `RouterLinkState` (matching
    /// `SetSideStable`'s compare-exchange semantics).
    pub fn set_link_status_bits(
        &self,
        desc: FragmentDescriptor,
        bits: u32,
    ) -> Result<RouterLinkStatus, LinkMemoryError> {
        if desc.size < ROUTER_LINK_STATE_SIZE as u32 {
            return Err(LinkMemoryError::OutOfBounds);
        }
        let view = self.fragment(desc)?;
        // SAFETY: as in `read_link_status`; the word is shared with the peer
        // process and accessed atomically on both sides.
        let status =
            unsafe { AtomicU32::from_ptr(view.as_ptr().add(Self::LINK_STATUS_OFFSET) as *mut u32) };
        status.fetch_or(bits, Ordering::Relaxed);
        Ok(RouterLinkStatus(status.load(Ordering::Relaxed)))
    }

    /// Read a parcel data fragment: `FragmentHeader { size, reserved }` then
    /// the parcel bytes. The size load is acquire, matching
    /// `Parcel::AdoptDataFragment`.
    pub fn read_parcel_fragment(
        &self,
        desc: FragmentDescriptor,
    ) -> Result<Vec<u8>, LinkMemoryError> {
        if desc.size < FRAGMENT_HEADER_SIZE as u32 {
            return Err(LinkMemoryError::OutOfBounds);
        }
        let view = self.fragment(desc)?;
        // SAFETY: the fragment is 8-byte aligned and within the mapping; the
        // size word is at offset 0 (`FragmentHeader { size, reserved }`).
        let size_word = unsafe { AtomicU32::from_ptr(view.as_ptr() as *mut u32) };
        let size = size_word.load(Ordering::Acquire) as usize;
        let data = view
            .get(FRAGMENT_HEADER_SIZE..FRAGMENT_HEADER_SIZE + size)
            .ok_or(LinkMemoryError::OutOfBounds)?;
        Ok(data.to_vec())
    }

    /// Allocate a parcel data fragment (`Parcel::AllocateData` + `CommitData`):
    /// allocate a block sized for `data.len() + sizeof(FragmentHeader)` via
    /// `try_allocate_fragment`, write the `FragmentHeader` and data, and
    /// publish the data size with a release store. Returns `Err(OutOfBounds)`
    /// when no block is available — the caller falls back to inline data,
    /// exactly like the official inline fallback.
    pub fn write_parcel_fragment(
        &mut self,
        data: &[u8],
    ) -> Result<FragmentDescriptor, LinkMemoryError> {
        let requested = data.len() + FRAGMENT_HEADER_SIZE;
        let block = self
            .try_allocate_fragment(requested)?
            .ok_or(LinkMemoryError::OutOfBounds)?;
        let view = self.fragment_mut(block)?;
        // FragmentHeader { size, reserved } — the size word is at offset 0.
        view[4..8].copy_from_slice(&0u32.to_le_bytes());
        view[FRAGMENT_HEADER_SIZE..FRAGMENT_HEADER_SIZE + data.len()].copy_from_slice(data);
        // Publish the data with a release store (balanced by the consumer's
        // acquire load in read_parcel_fragment / Parcel::AdoptDataFragment).
        // SAFETY: the fragment is 8-byte aligned and within the mapping.
        let size_word = unsafe { AtomicU32::from_ptr(view.as_ptr() as *mut u32) };
        size_word.store(data.len() as u32, Ordering::Release);
        Ok(block)
    }

    /// `NodeLinkMemory::AllocateFragment`: allocate a block of the smallest
    /// registered pool whose block size is at least the ideal size for
    /// `size` (the official `GetBlockSizeForFragmentSize` + `BufferPool`
    /// `lower_bound` selection). Returns `None` when no block is available;
    /// the caller decides whether to lobby for more capacity.
    pub fn try_allocate_fragment(
        &self,
        size: usize,
    ) -> Result<Option<FragmentDescriptor>, LinkMemoryError> {
        if size == 0 || size > MAX_FRAGMENT_SIZE_FOR_BLOCK_ALLOCATION {
            return Ok(None);
        }
        let ideal = get_block_size_for_fragment_size(size);
        let Some(pool_size) = self.pool_sizes().into_iter().find(|s| *s >= ideal) else {
            return Ok(None);
        };
        // `BlockAllocatorPool::Allocate` tries the ACTIVE allocator first —
        // the most recently added buffer of this size — then walks the list.
        // Extra buffers are therefore tried in reverse insertion order before
        // the primary region.
        for &id in self.extra_order.iter().rev() {
            let e = self.extra.get(&id).ok_or(LinkMemoryError::UnknownBuffer)?;
            if e.block_size == pool_size {
                let count = e.mapping.len() / pool_size as usize;
                if let Some(b) = self.pop_block(id, 0, pool_size, count)? {
                    return Ok(Some(b));
                }
            }
        }
        if let Some(&(_, off, count)) = PRIMARY_BLOCK_REGIONS
            .iter()
            .find(|(s, _, _)| *s == pool_size)
        {
            if let Some(b) = self.pop_block(PRIMARY_BUFFER_ID, off, pool_size, count)? {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }

    /// The total managed capacity in bytes for a block size across all pools
    /// (`BufferPool::GetTotalBlockCapacity`: `capacity() * block_size` per
    /// allocator, where `capacity() = num_blocks - 1`).
    pub fn get_total_block_capacity(&self, block_size: u32) -> usize {
        let mut total = 0usize;
        if let Some(&(_, _, count)) = PRIMARY_BLOCK_REGIONS
            .iter()
            .find(|(s, _, _)| *s == block_size)
        {
            total += (count - 1) * block_size as usize;
        }
        for &id in &self.extra_order {
            if let Some(e) = self.extra.get(&id) {
                if e.block_size == block_size {
                    total += (e.mapping.len() / block_size as usize - 1) * block_size as usize;
                }
            }
        }
        total
    }

    /// The ids of the extra (post-Connect) block buffers, in registration
    /// order (used by the courts to verify a capacity expansion was adopted).
    pub fn extra_buffer_ids(&self) -> Vec<u64> {
        self.extra_order.clone()
    }

    /// Whether additional capacity may be lobbied for a block size
    /// (`CanExpandBlockCapacity`). The pinned mojo embedder sets
    /// `IPCZ_MEMORY_FIXED_PARCEL_CAPACITY` (ipcz_api.cc, crbug.com/40876289),
    /// so `allow_memory_expansion_for_parcel_data_` is false for parcel data
    /// in this epoch; this mirror is retained for the `RouterLinkState` lobby
    /// and the future exhaustion court (only the per-size capacity ceiling is
    /// enforced here).
    pub fn can_expand_block_capacity(&self, block_size: u32) -> bool {
        self.get_total_block_capacity(block_size) < MAX_BLOCK_ALLOCATOR_CAPACITY_PER_FRAGMENT_SIZE
    }

    /// Free a block back to its allocator's free-list
    /// (`BlockAllocator::Free`): push the block at the head of the list with
    /// the same CAS protocol on the front header. Returns false for a block
    /// that does not belong to a known pool.
    pub fn free_block(&self, desc: FragmentDescriptor) -> Result<bool, LinkMemoryError> {
        let (region, block_size, num_blocks) = if desc.buffer_id == PRIMARY_BUFFER_ID {
            let Some(&(bs, off, count)) = PRIMARY_BLOCK_REGIONS.iter().find(|(bs, off, count)| {
                desc.size as usize == *bs as usize
                    && desc.offset as usize >= *off
                    && desc.offset as usize + desc.size as usize <= off + count * *bs as usize
            }) else {
                return Ok(false);
            };
            (off, bs as usize, count)
        } else {
            let Some(e) = self.extra.get(&desc.buffer_id) else {
                return Ok(false);
            };
            if e.block_size as usize != desc.size as usize {
                return Ok(false);
            }
            let count = e.mapping.len() / e.block_size as usize;
            (0usize, e.block_size as usize, count)
        };
        let idx = (desc.offset as usize - region) / block_size;
        if idx == 0 || idx >= num_blocks {
            // The first block is never allocable, and out-of-range blocks are
            // invalid (`BlockAllocator::Free` rejects both).
            return Ok(false);
        }
        let front_desc = FragmentDescriptor {
            buffer_id: desc.buffer_id,
            offset: region as u32,
            size: 4,
        };
        let mut front = self.read_block_header(front_desc)?;
        loop {
            // The free-list may be empty (front points at the unallocatable
            // block 0): the freed block becomes the new head, so index 0 is
            // a valid "next" target (`is_index_valid` covers 0..num_blocks).
            let first_free = front.next as i32 + 1;
            if first_free < 0 || first_free >= num_blocks as i32 {
                return Ok(false);
            }
            // Write the freed block's header pointing at the current head
            // (`FreeBlock::SetNextFreeBlock`, release store; `version` is 0
            // for non-front blocks).
            let rel = (first_free - idx as i32 - 1) as i16;
            self.write_block_header(
                FragmentDescriptor {
                    buffer_id: desc.buffer_id,
                    offset: (region + idx * block_size) as u32,
                    size: 4,
                },
                0,
                rel,
            )?;
            // CAS the front header to point at the freed block. On failure
            // the freed block's header is rewritten with the updated front.
            let new_version = front.version.wrapping_add(1);
            match self.cas_front_header(front_desc, front, new_version, (idx as i32 - 1) as i16)? {
                Some(updated) => {
                    front = updated;
                }
                None => return Ok(true),
            }
        }
    }

    /// Pop the first free block from a block allocator's free-list
    /// (`BlockAllocator::Allocate`): a CAS loop on the front block's header
    /// with `expected` refreshed from the observed value on each failure
    /// (Rust's CAS does not update the argument, unlike C++'s reference
    /// parameter). Returns `None` when the free-list is exhausted.
    ///
    /// `region` is the byte offset of the allocator region within the buffer's
    /// mapping (0 for whole-buffer extra buffers). Block 0 is never allocable;
    /// the free-list is a singly-linked list rooted there, terminated by a
    /// back-reference to block 0.
    fn pop_block(
        &self,
        buffer_id: u64,
        region: usize,
        block_size: u32,
        num_blocks: usize,
    ) -> Result<Option<FragmentDescriptor>, LinkMemoryError> {
        if num_blocks == 0 || num_blocks > i16::MAX as usize {
            return Ok(None);
        }
        let front_desc = FragmentDescriptor {
            buffer_id,
            offset: region as u32,
            size: 4,
        };
        let mut front = self.read_block_header(front_desc)?;
        loop {
            // Absolute index of the first free block: the front's `next` is
            // relative to the front block's successor (`ForBaseIndex(0)`).
            let first_free = front.next as i32 + 1;
            if first_free == 0 || first_free >= num_blocks as i32 {
                // The front points at block 0 (empty) or out of range
                // (invalid); either way the allocation fails.
                return Ok(None);
            }
            let idx = first_free as usize;
            let hdr = self.read_block_header(FragmentDescriptor {
                buffer_id,
                offset: (region + idx * block_size as usize) as u32,
                size: 4,
            })?;
            let next_abs = idx as i32 + hdr.next as i32 + 1;
            if next_abs < 0 || next_abs >= num_blocks as i32 {
                return Ok(None);
            }
            // CAS the front header to point at the second free block. On
            // success the first free block is allocated (its header space is
            // now owned by the caller). On failure `front` is refreshed.
            match self.cas_front_header(
                front_desc,
                front,
                front.version.wrapping_add(1),
                (next_abs - 1) as i16,
            )? {
                Some(updated) => front = updated,
                None => {
                    return Ok(Some(FragmentDescriptor {
                        buffer_id,
                        offset: (region + idx * block_size as usize) as u32,
                        size: block_size,
                    }));
                }
            }
        }
    }

    /// A 4-byte block allocator header (`BlockHeader { version u16, next i16 }`).
    fn read_block_header(&self, desc: FragmentDescriptor) -> Result<BlockHeader, LinkMemoryError> {
        let view = self.fragment(desc)?.to_vec();
        if view.len() < 4 {
            return Err(LinkMemoryError::OutOfBounds);
        }
        Ok(BlockHeader {
            version: u16::from_le_bytes(
                view[0..2]
                    .try_into()
                    .map_err(|_| LinkMemoryError::OutOfBounds)?,
            ),
            next: i16::from_le_bytes(
                view[2..4]
                    .try_into()
                    .map_err(|_| LinkMemoryError::OutOfBounds)?,
            ),
        })
    }

    /// Write a block header (used for freed blocks: `version` 0 for non-front
    /// blocks, release store matching `FreeBlock::SetNextFreeBlock`).
    fn write_block_header(
        &self,
        desc: FragmentDescriptor,
        version: u16,
        next: i16,
    ) -> Result<(), LinkMemoryError> {
        let view = self.fragment(desc)?;
        // SAFETY: the header word is 8-byte aligned within the mapping and
        // shared with the peer; all access is atomic on both sides. The block
        // is exclusively owned by the freed-block protocol at this point.
        let word = unsafe { AtomicU32::from_ptr(view.as_ptr() as *mut u32) };
        let mut val = version as u32;
        val |= ((next as u16) as u32) << 16;
        word.store(val, Ordering::Release);
        Ok(())
    }

    /// CAS the front block header (`TryUpdateFrontHeader`): swap in
    /// `{version, next}` against `known`. Returns `Ok(Some(updated))` when the
    /// CAS failed (with the observed value) and `Ok(None)` when it succeeded.
    fn cas_front_header(
        &self,
        desc: FragmentDescriptor,
        known: BlockHeader,
        version: u16,
        next: i16,
    ) -> Result<Option<BlockHeader>, LinkMemoryError> {
        let view = self.fragment(desc)?;
        // SAFETY: the front header word is 8-byte aligned within the mapping
        // and shared with the peer; all access is atomic on both sides.
        let word = unsafe { AtomicU32::from_ptr(view.as_ptr() as *mut u32) };
        let known_val = (known.version as u32) | (((known.next as u16) as u32) << 16);
        let mut desired = version as u32;
        desired |= ((next as u16) as u32) << 16;
        match word.compare_exchange_weak(known_val, desired, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => Ok(None),
            Err(actual) => Ok(Some(BlockHeader {
                version: (actual & 0xffff) as u16,
                next: ((actual >> 16) & 0xffff) as u16 as i16,
            })),
        }
    }

    /// All registered pool sizes (primary regions plus extra buffers).
    fn pool_sizes(&self) -> Vec<u32> {
        let mut sizes: Vec<u32> = PRIMARY_BLOCK_REGIONS.iter().map(|(s, _, _)| *s).collect();
        for &id in &self.extra_order {
            if let Some(e) = self.extra.get(&id) {
                sizes.push(e.block_size);
            }
        }
        sizes.sort_unstable();
        sizes.dedup();
        sizes
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Create a fresh memfd-backed primary buffer initialized like the
    /// broker's `AllocateMemory`: header counters, initialized allocator
    /// regions, zeroed initial link states.
    ///
    /// Returns the keeper object (kept alive for the test) and a duplicated
    /// descriptor for adoption.
    fn fresh_buffer() -> (SharedMemory, RawFd) {
        let mem = SharedMemory::create("test-link-mem", PRIMARY_BUFFER_SIZE).unwrap();
        {
            let mut map = mem.map(0, PRIMARY_BUFFER_SIZE, Access::ReadWrite).unwrap();
            // Zero everything (memfd is already zeroed, but be explicit).
            for b in map.iter_mut() {
                *b = 0;
            }
            // next_buffer_id = 1, next_sublink_id = 12 (matches AllocateMemory).
            map[0..8].copy_from_slice(&1u64.to_le_bytes());
            map[8..16].copy_from_slice(&(MAX_INITIAL_PORTALS as u64).to_le_bytes());
            // Initialize the 64-byte allocator region: a zeroed region already
            // makes every block point at its successor; set the last block's
            // header to point back to block 0 (relative from its successor).
            for &(block_size, off, count) in PRIMARY_BLOCK_REGIONS {
                let last = off + (count - 1) * block_size as usize;
                let rel = -(count as i16);
                map[last + 2..last + 4].copy_from_slice(&rel.to_le_bytes());
            }
        }
        // SAFETY: dup is a plain syscall on a valid descriptor.
        let fd = unsafe { libc::dup(mem.as_raw_fd()) };
        assert!(fd >= 0);
        (mem, fd)
    }

    #[test]
    fn fragment_bounds_checked() {
        let (_keep, fd) = fresh_buffer();
        let mut mem = LinkMemory::adopt_primary(fd).unwrap();
        // A valid fragment.
        let d = FragmentDescriptor {
            buffer_id: 0,
            offset: 0x100,
            size: 64,
        };
        assert!(mem.fragment(d).is_ok());
        // Out of bounds.
        let d = FragmentDescriptor {
            buffer_id: 0,
            offset: PRIMARY_BUFFER_SIZE as u32 - 32,
            size: 64,
        };
        assert_eq!(mem.fragment(d), Err(LinkMemoryError::OutOfBounds));
        // Unknown buffer.
        let d = FragmentDescriptor {
            buffer_id: 7,
            offset: 0,
            size: 64,
        };
        assert_eq!(mem.fragment(d), Err(LinkMemoryError::UnknownBuffer));
        // Misaligned.
        let d = FragmentDescriptor {
            buffer_id: 0,
            offset: 4,
            size: 64,
        };
        assert_eq!(mem.fragment(d), Err(LinkMemoryError::Misaligned));
        // Null.
        assert_eq!(
            mem.fragment(FragmentDescriptor {
                buffer_id: FragmentDescriptor::INVALID_BUFFER_ID,
                offset: 0,
                size: 0,
            }),
            Err(LinkMemoryError::NullDescriptor)
        );
        let _ = mem;
    }

    #[test]
    fn link_status_bits() {
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        let d = FragmentDescriptor {
            buffer_id: 0,
            offset: INITIAL_LINK_STATES_OFFSET as u32 + 64, // portal 1
            size: 64,
        };
        assert_eq!(mem.read_link_status(d).unwrap().value(), 0);
        mem.set_link_status_bits(d, RouterLinkStatus::SIDE_B_STABLE)
            .unwrap();
        let s = mem.read_link_status(d).unwrap();
        assert!(s.side_b_stable());
        assert!(!s.side_a_stable());
        mem.set_link_status_bits(d, RouterLinkStatus::SIDE_A_STABLE)
            .unwrap();
        assert!(mem.read_link_status(d).unwrap().is_stable());
    }

    #[test]
    fn parcel_fragment_roundtrip() {
        let (_keep, fd) = fresh_buffer();
        let mut mem = LinkMemory::adopt_primary(fd).unwrap();
        let desc = mem.write_parcel_fragment(b"hello-from-acceptor").unwrap();
        assert_eq!(desc.buffer_id, 0);
        assert_eq!(desc.offset, 0x440); // first allocable 64-byte block
        assert_eq!(desc.size, 64);
        assert_eq!(
            mem.read_parcel_fragment(desc).unwrap(),
            b"hello-from-acceptor"
        );
        // The next allocation pops the second block.
        let d2 = mem.write_parcel_fragment(b"x").unwrap();
        assert_eq!(d2.offset, 0x480);
    }

    /// A link-state fragment at the first allocable 64-byte block.
    fn state_desc() -> FragmentDescriptor {
        FragmentDescriptor {
            buffer_id: 0,
            offset: (BLOCK_ALLOCATOR_64_OFFSET) as u32,
            size: ROUTER_LINK_STATE_SIZE as u32,
        }
    }

    #[test]
    fn set_side_stable_or_into_peer_bits() {
        // Regression: `SetSideStable` must OR its bit into whatever status the
        // peer already wrote. The pre-fix loop kept `expected` pinned at 0 and
        // would spin forever when the peer's stable bit was already set.
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        let d = state_desc();
        // The peer (side A) set its stable bit first.
        mem.set_side_stable(d, true).unwrap();
        assert_eq!(
            mem.read_link_status(d).unwrap().value(),
            RouterLinkStatus::SIDE_A_STABLE
        );
        // Our side B set must succeed and preserve side A's bit.
        mem.set_side_stable(d, false).unwrap();
        let s = mem.read_link_status(d).unwrap();
        assert!(s.side_a_stable());
        assert!(s.side_b_stable());
        assert_eq!(s.value(), RouterLinkStatus::STABLE);
    }

    #[test]
    fn try_lock_requires_stable_and_excludes_peer() {
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        let d = state_desc();
        // Unstable link: TryLock must fail immediately (side not stable), and
        // must NOT set any bits.
        assert!(!mem.try_lock_link_state(d, true).unwrap());
        assert_eq!(mem.read_link_status(d).unwrap().value(), 0);
        // Stabilize both sides.
        mem.set_side_stable(d, true).unwrap();
        mem.set_side_stable(d, false).unwrap();
        // Side A locks.
        assert!(mem.try_lock_link_state(d, true).unwrap());
        let locked = mem.read_link_status(d).unwrap();
        assert_eq!(
            locked.value() & RouterLinkStatus::LOCKED_BY_SIDE_A,
            RouterLinkStatus::LOCKED_BY_SIDE_A
        );
        // Side B cannot lock while A holds it.
        assert!(!mem.try_lock_link_state(d, false).unwrap());
        // A unlocks; B locks.
        mem.unlock_link_state(d, true).unwrap();
        assert_eq!(
            mem.read_link_status(d).unwrap().value(),
            RouterLinkStatus::STABLE
        );
        assert!(mem.try_lock_link_state(d, false).unwrap());
        mem.unlock_link_state(d, false).unwrap();
    }

    #[test]
    fn try_lock_sets_waiting_bit_when_peer_unstable() {
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        let d = state_desc();
        // Our side (A) is stable; the peer (B) is not.
        mem.set_side_stable(d, true).unwrap();
        assert!(!mem.try_lock_link_state(d, true).unwrap());
        // The waiting bit is set and the lock is not.
        let s = mem.read_link_status(d).unwrap();
        assert_eq!(
            s.value(),
            RouterLinkStatus::SIDE_A_STABLE | RouterLinkStatus::SIDE_A_WAITING
        );
        // The peer stabilizes; resetting our waiting bit succeeds.
        mem.set_side_stable(d, false).unwrap();
        assert!(mem.reset_waiting_bit(d, true).unwrap());
        assert_eq!(
            mem.read_link_status(d).unwrap().value(),
            RouterLinkStatus::STABLE
        );
        // Now we can lock.
        assert!(mem.try_lock_link_state(d, true).unwrap());
        mem.unlock_link_state(d, true).unwrap();
    }

    #[test]
    fn reset_waiting_bit_refuses_when_not_waiting_or_locked() {
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        let d = state_desc();
        // Stable but not waiting: nothing to reset.
        mem.set_side_stable(d, true).unwrap();
        mem.set_side_stable(d, false).unwrap();
        assert!(!mem.reset_waiting_bit(d, true).unwrap());
        // Locked: nothing to reset.
        assert!(mem.try_lock_link_state(d, true).unwrap());
        assert!(!mem.reset_waiting_bit(d, true).unwrap());
        assert!(!mem.reset_waiting_bit(d, false).unwrap());
        mem.unlock_link_state(d, true).unwrap();
    }

    #[test]
    fn buffer_id_counter_starts_at_one() {
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        // `AllocateMemory` seeds next_buffer_id = 1.
        assert_eq!(mem.allocate_new_buffer_id().unwrap(), 1);
        assert_eq!(mem.allocate_new_buffer_id().unwrap(), 2);
    }

    #[test]
    fn fragment_allocates_256_blocks_then_exhausts() {
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        // A 200-byte parcel wants a 256-byte block (ideal 256). The primary
        // 256-byte region has 8 allocable blocks (blocks 1..8).
        let mut offsets = Vec::new();
        for _ in 0..8 {
            let d = mem.try_allocate_fragment(208).unwrap().unwrap();
            assert_eq!(d.buffer_id, PRIMARY_BUFFER_ID);
            assert_eq!(d.size, 256);
            offsets.push(d.offset);
        }
        assert_eq!(
            offsets,
            (0..8)
                .map(|k| (BLOCK_ALLOCATOR_256_OFFSET + (k + 1) * 256) as u32)
                .collect::<Vec<_>>()
        );
        // The 9th allocation fails (the pool is exhausted).
        assert!(mem.try_allocate_fragment(208).unwrap().is_none());
        assert_eq!(
            mem.get_total_block_capacity(256),
            (BLOCK_ALLOCATOR_256_BLOCKS - 1) * 256
        );
    }

    #[test]
    fn free_reuses_blocks_lifo() {
        let (_keep, fd) = fresh_buffer();
        let mem = LinkMemory::adopt_primary(fd).unwrap();
        let mut blocks = Vec::new();
        for _ in 0..8 {
            blocks.push(mem.try_allocate_fragment(208).unwrap().unwrap());
        }
        assert!(mem.try_allocate_fragment(208).unwrap().is_none());
        // Free in order; the free-list pushes at the head, so re-allocation
        // pops the most recently freed block first (LIFO), exactly like the
        // memory court's m9/m10 reuse of the broker's reads.
        for b in &blocks {
            assert!(mem.free_block(*b).unwrap());
        }
        let d9 = mem.try_allocate_fragment(208).unwrap().unwrap();
        let d10 = mem.try_allocate_fragment(208).unwrap().unwrap();
        assert_eq!(d9.offset, blocks[7].offset);
        assert_eq!(d10.offset, blocks[6].offset);
        // Freeing a block from an unknown buffer is rejected.
        let foreign = FragmentDescriptor {
            buffer_id: 99,
            offset: 256,
            size: 256,
        };
        assert!(!mem.free_block(foreign).unwrap());
        // Freeing the unallocatable block 0 is rejected.
        let zero = FragmentDescriptor {
            buffer_id: PRIMARY_BUFFER_ID,
            offset: BLOCK_ALLOCATOR_256_OFFSET as u32,
            size: 256,
        };
        assert!(!mem.free_block(zero).unwrap());
    }

    #[test]
    fn registered_block_buffer_serves_allocations() {
        let (_keep, fd) = fresh_buffer();
        let mut mem = LinkMemory::adopt_primary(fd).unwrap();
        // Exhaust the primary 256-pool.
        for _ in 0..8 {
            assert!(mem.try_allocate_fragment(208).unwrap().is_some());
        }
        assert!(mem.try_allocate_fragment(208).unwrap().is_none());
        // Register a fresh 64 KiB buffer of 256-byte blocks (the send side of
        // a capacity expansion: `register_block_buffer` initializes the
        // region).
        let new_mem = SharedMemory::create("test-block-buffer", 64 * 1024).unwrap();
        let id = mem.allocate_new_buffer_id().unwrap();
        assert_eq!(id, 1);
        let fd2 = unsafe { libc::dup(new_mem.as_raw_fd()) };
        assert!(fd2 >= 0);
        mem.register_block_buffer(id, fd2, 256).unwrap();
        assert_eq!(mem.get_total_block_capacity(256), 2048 + 255 * 256);
        // The next allocation comes from the new buffer.
        let d = mem.try_allocate_fragment(208).unwrap().unwrap();
        assert_eq!(d.buffer_id, id);
        assert_eq!(d.size, 256);
        assert_eq!(d.offset, 256); // block 1
        // The new buffer's allocator is initialized: the last block points
        // back to block 0 and the free-list walks sequentially.
        let d2 = mem.try_allocate_fragment(208).unwrap().unwrap();
        assert_eq!(d2.offset, 512); // block 2
    }

    #[test]
    fn receive_side_add_block_buffer_registers_as_is() {
        let (_keep, fd) = fresh_buffer();
        let mut mem = LinkMemory::adopt_primary(fd).unwrap();
        // The receive side adopts a buffer initialized by the sender (the
        // region is NOT re-initialized).
        let new_mem = SharedMemory::create("test-add-buffer", 64 * 1024).unwrap();
        {
            let mut map = new_mem.map(0, 64 * 1024, Access::ReadWrite).unwrap();
            // Initialize like the sender's `InitializeRegion`.
            for b in map.iter_mut() {
                *b = 0;
            }
            let last = (256 - 1) * 256;
            map[last + 2..last + 4].copy_from_slice(&(-(256i16)).to_le_bytes());
        }
        let fd2 = unsafe { libc::dup(new_mem.as_raw_fd()) };
        assert!(fd2 >= 0);
        mem.add_block_buffer(1, fd2, 256).unwrap();
        let d = mem.try_allocate_fragment(208).unwrap().unwrap();
        assert_eq!(d.buffer_id, 1);
        assert_eq!(d.offset, 256);
        // Invalid registrations are rejected (fewer than 8 blocks).
        let bad_mem = SharedMemory::create("test-bad-buffer", 1024).unwrap();
        let fd3 = unsafe { libc::dup(bad_mem.as_raw_fd()) };
        assert!(fd3 >= 0);
        assert!(mem.add_block_buffer(2, fd3, 256).is_err()); // 4 blocks < 8
        // A non-8-aligned block size is rejected.
        let bad_mem2 = SharedMemory::create("test-bad-buffer2", 64 * 1024).unwrap();
        let fd4 = unsafe { libc::dup(bad_mem2.as_raw_fd()) };
        assert!(fd4 >= 0);
        assert!(mem.add_block_buffer(3, fd4, 100).is_err());
    }

    #[test]
    fn parcel_fragment_roundtrip_large() {
        let (_keep, fd) = fresh_buffer();
        let mut mem = LinkMemory::adopt_primary(fd).unwrap();
        // A 200-byte payload needs a 256-byte block.
        let data = vec![0xabu8; 200];
        let desc = mem.write_parcel_fragment(&data).unwrap();
        assert_eq!(desc.size, 256);
        assert_eq!(desc.offset, (BLOCK_ALLOCATOR_256_OFFSET + 256) as u32);
        assert_eq!(mem.read_parcel_fragment(desc).unwrap(), data);
        // The 64-byte fragment path still works.
        let small = mem.write_parcel_fragment(b"hi").unwrap();
        assert_eq!(small.size, 64);
        assert_eq!(mem.read_parcel_fragment(small).unwrap(), b"hi");
    }
}
