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

/// The primary buffer id.
pub const PRIMARY_BUFFER_ID: u64 = 0;

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

/// The shared link memory: the mapped primary buffer plus any additional
/// buffers added later (AddBlockBuffer).
pub struct LinkMemory {
    /// The mapped primary buffer (BufferId 0).
    primary: Buffer,
    /// Additional buffers by buffer id.
    extra: std::collections::HashMap<u64, Buffer>,
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
        })
    }

    /// Add a block buffer (AddBlockBuffer) to the pool.
    pub fn add_block_buffer(
        &mut self,
        id: u64,
        fd: RawFd,
        size: usize,
    ) -> Result<(), std::io::Error> {
        self.extra.insert(id, Buffer::adopt(fd, size)?);
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
        if end > buffer.mapping.len() {
            return Err(LinkMemoryError::OutOfBounds);
        }
        Ok(&buffer.mapping[desc.offset as usize..end])
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
        if end > buffer.mapping.len() {
            return Err(LinkMemoryError::OutOfBounds);
        }
        // SAFETY: `&mut self` grants exclusive access to this mapping, and the
        // range was validated. The Mapping is owned by this LinkMemory.
        let ptr = unsafe { buffer.mapping.as_mut_ptr() };
        // SAFETY: the range [offset, offset+size) was validated against the
        // mapping length above; the pointer is derived from the owned mapping.
        Ok(unsafe {
            std::slice::from_raw_parts_mut(ptr.add(desc.offset as usize), desc.size as usize)
        })
    }

    fn buffer_for(&self, id: u64) -> Result<&Buffer, LinkMemoryError> {
        if id == PRIMARY_BUFFER_ID {
            Ok(&self.primary)
        } else {
            self.extra.get(&id).ok_or(LinkMemoryError::UnknownBuffer)
        }
    }

    /// The initial `RouterLinkState` offset for portal `i` (fixed location).
    pub fn initial_link_state_offset(i: usize) -> usize {
        INITIAL_LINK_STATES_OFFSET + i * ROUTER_LINK_STATE_SIZE
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

    /// Allocate a 64-byte block and initialize it as a fresh `RouterLinkState`
    /// (all zeros; `RouterLinkState::Initialize`). Returns the descriptor, or
    /// None when the allocator is exhausted.
    pub fn try_allocate_link_state(&mut self) -> Result<FragmentDescriptor, LinkMemoryError> {
        let block = self.alloc_64_block()?;
        let view = self.fragment_mut(block)?;
        // RouterLinkState::Initialize default-constructs the struct: status 0
        // and zeroed reserved fields (a zeroed block is exactly that).
        for b in view.iter_mut() {
            *b = 0;
        }
        Ok(block)
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

    /// Read the `allowed_bypass_request_source` NodeName of a link state.
    pub fn read_allowed_bypass_source(
        &self,
        desc: FragmentDescriptor,
    ) -> Result<u64, LinkMemoryError> {
        let view = self.fragment(desc)?;
        let off = Self::LINK_ALLOWED_SOURCE_OFFSET;
        let low = view.get(off..off + 8).ok_or(LinkMemoryError::OutOfBounds)?;
        Ok(u64::from_le_bytes(
            low.try_into().map_err(|_| LinkMemoryError::OutOfBounds)?,
        ))
    }

    /// Write the `allowed_bypass_request_source` NodeName (low 64 bits are the
    /// value compared against remote node names) of a link state.
    pub fn write_allowed_bypass_source(
        &mut self,
        desc: FragmentDescriptor,
        value: u64,
    ) -> Result<(), LinkMemoryError> {
        let view = self.fragment_mut(desc)?;
        let off = Self::LINK_ALLOWED_SOURCE_OFFSET;
        let slot = view
            .get_mut(off..off + 8)
            .ok_or(LinkMemoryError::OutOfBounds)?;
        slot.copy_from_slice(&value.to_le_bytes());
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

    /// Allocate a parcel data fragment from the 64-byte block allocator,
    /// writing the `FragmentHeader` and data. Returns the fragment descriptor.
    ///
    /// The free-list is a singly-linked list rooted at block 0; allocation
    /// pops the first free block, matching `BlockAllocator::Allocate`. The
    /// data is written first and the size is published with a release store,
    /// matching `Parcel::CommitData` — the consumer's acquire load must see
    /// the data.
    pub fn write_parcel_fragment(
        &mut self,
        data: &[u8],
    ) -> Result<FragmentDescriptor, LinkMemoryError> {
        if data.len() > 64 - FRAGMENT_HEADER_SIZE {
            return Err(LinkMemoryError::OutOfBounds);
        }
        let block = self.alloc_64_block()?;
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

    /// Pop the first free 64-byte block from the allocator free-list.
    ///
    /// Block 0 (at `BLOCK_ALLOCATOR_64_OFFSET`) is the header; each block
    /// starts with `BlockHeader { version u16, next i16 }` where `next` is
    /// relative to the block's successor (see `BlockAllocator`).
    fn alloc_64_block(&mut self) -> Result<FragmentDescriptor, LinkMemoryError> {
        let region = BLOCK_ALLOCATOR_64_OFFSET;
        let block_size = 64usize;
        let num_blocks = BLOCK_ALLOCATOR_64_BLOCKS;
        let front = self
            .fragment(FragmentDescriptor {
                buffer_id: PRIMARY_BUFFER_ID,
                offset: region as u32,
                size: 4,
            })?
            .to_vec();
        let front_next = i16::from_le_bytes(
            front[2..4]
                .try_into()
                .map_err(|_| LinkMemoryError::OutOfBounds)?,
        );
        // Absolute index of the first free block = front_index + next + 1.
        let first = front_next as i32 + 1;
        if first <= 0 || first >= num_blocks as i32 {
            return Err(LinkMemoryError::OutOfBounds);
        }
        let idx = first as usize;
        let block_off = region + idx * block_size;
        // Read the free block's header to find the next free block.
        let hdr = self
            .fragment(FragmentDescriptor {
                buffer_id: PRIMARY_BUFFER_ID,
                offset: block_off as u32,
                size: 4,
            })?
            .to_vec();
        let next_rel = i16::from_le_bytes(
            hdr[2..4]
                .try_into()
                .map_err(|_| LinkMemoryError::OutOfBounds)?,
        );
        let next_abs = (idx as i32) + (next_rel as i32) + 1;
        if next_abs < 0 || next_abs >= num_blocks as i32 {
            return Err(LinkMemoryError::OutOfBounds);
        }
        // Update the front block to point at the next free block.
        let version = u16::from_le_bytes(
            front[0..2]
                .try_into()
                .map_err(|_| LinkMemoryError::OutOfBounds)?,
        )
        .wrapping_add(1);
        let mut front_new = [0u8; 4];
        front_new[0..2].copy_from_slice(&version.to_le_bytes());
        // Relative from front's successor (block 1) to next_abs.
        front_new[2..4].copy_from_slice(&(next_abs as i16).wrapping_sub(1).to_le_bytes());
        let front_view = self.fragment_mut(FragmentDescriptor {
            buffer_id: PRIMARY_BUFFER_ID,
            offset: region as u32,
            size: 4,
        })?;
        front_view.copy_from_slice(&front_new);
        Ok(FragmentDescriptor {
            buffer_id: PRIMARY_BUFFER_ID,
            offset: block_off as u32,
            size: block_size as u32,
        })
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
            let last = BLOCK_ALLOCATOR_64_OFFSET + (BLOCK_ALLOCATOR_64_BLOCKS - 1) * 64;
            let rel = -(BLOCK_ALLOCATOR_64_BLOCKS as i16);
            map[last + 2..last + 4].copy_from_slice(&rel.to_le_bytes());
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
}
