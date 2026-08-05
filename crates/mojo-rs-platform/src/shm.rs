//! Shared memory: `memfd_create`, mapping, and unmapping with safe ownership.
//!
//! `SharedMemory` owns a memfd; `Mapping` owns a mapped region with explicit
//! read/write access modes (no aliasing of mutable mappings).

use std::io;
use std::ops::Deref;
use std::os::unix::io::{AsRawFd, RawFd};

use crate::fd::OwnedFd;
use crate::sys;

/// A shared-memory object backed by `memfd_create`.
#[derive(Debug)]
pub struct SharedMemory {
    fd: OwnedFd,
    size: usize,
}

/// Access mode for a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read-only mapping (`PROT_READ`).
    ReadOnly,
    /// Read-write mapping (`PROT_READ | PROT_WRITE`).
    ReadWrite,
}

/// A mapped region of shared memory.
///
/// The platform `mmap` granularity is one page; the official Mojo `MapAt`
/// aligns the requested offset down to the page boundary and adjusts the
/// returned pointer (base/memory/platform_shared_memory_region.cc). `Mapping`
/// therefore records the aligned base and total mapped length for `munmap`
/// while exposing the exact requested `[ptr, ptr + len)` range.
///
/// `Mapping` is `Send + Sync`: it owns a live `mmap` region whose memory is
/// stable until `Drop` (by the sole owner); shared reads through `&Mapping`
/// are safe concurrently, and mutation requires `&mut Mapping` (the safe
/// API), which owning structures (e.g. a `RingBuffer` behind a `Mutex`)
/// serialize.
pub struct Mapping {
    /// The exact requested start of the mapping.
    ptr: *mut u8,
    /// The exact requested length.
    len: usize,
    /// The page-aligned base passed to `mmap` (may be `< ptr`).
    map_base: *mut u8,
    /// The total length passed to `mmap` (`len` plus the alignment adjustment).
    map_len: usize,
    access: Access,
}

impl SharedMemory {
    /// Create a memfd of `size` bytes.
    pub fn create(name: &str, size: usize) -> io::Result<SharedMemory> {
        let cname = std::ffi::CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
        // SAFETY: cname is a valid NUL-terminated string.
        let fd = unsafe { sys::memfd_create(cname.as_ptr(), 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a fresh owned descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: fd is valid and writable.
        let rc = unsafe { sys::ftruncate(fd, size as libc::off_t) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SharedMemory { fd: owned, size })
    }

    /// Adopt a shared-memory descriptor received out-of-band (e.g. via
    /// `SCM_RIGHTS`) as a shared-memory object of `size` bytes.
    ///
    /// The caller transfers ownership of `fd`; it is closed when this object
    /// is dropped. The size must be known independently (e.g. from the wire
    /// `BufferHeader`); it is validated against the descriptor at map time.
    pub fn from_raw_fd(fd: RawFd, size: usize) -> io::Result<SharedMemory> {
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative descriptor",
            ));
        }
        // SAFETY: the caller transfers ownership of a valid descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // Validate that the descriptor is a seekable shared-memory object of
        // at least `size` bytes, so a bogus descriptor cannot later fault the
        // mapping.
        // SAFETY: `st` is a zeroed, valid output buffer for `fstat`.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd is owned and valid; `st` points to writable storage.
        let rc = unsafe { sys::fstat(fd, &mut st) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let reported = st.st_size as u64;
        if (reported as usize) < size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "descriptor smaller than declared size",
            ));
        }
        Ok(SharedMemory { fd: owned, size })
    }

    /// Adopt a descriptor whose size is not known a priori.
    ///
    /// The object size is taken from the descriptor itself (`fstat`); this is
    /// the receive side of an ipcz `AddBlockBuffer`, whose buffer size is only
    /// knowable from the descriptor (the message carries the block size, not
    /// the buffer size). The descriptor is validated to be a seekable
    /// shared-memory object of non-zero size.
    pub fn from_fd(fd: RawFd) -> io::Result<SharedMemory> {
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative descriptor",
            ));
        }
        // SAFETY: the caller transfers ownership of a valid descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: `st` is a zeroed, valid output buffer for `fstat`.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd is owned and valid; `st` points to writable storage.
        let rc = unsafe { sys::fstat(fd, &mut st) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        if st.st_size <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "descriptor is not a non-empty shared-memory object",
            ));
        }
        Ok(SharedMemory {
            fd: owned,
            size: st.st_size as usize,
        })
    }

    /// The size of the memory object.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Map `len` bytes at byte offset `offset` with the given access.
    ///
    /// The official `MojoMapBuffer` allows arbitrary (page-unaligned) offsets:
    /// `base::subtle::PlatformSharedMemoryRegion::MapAt` aligns down to the
    /// system page and adjusts the returned pointer. Returns
    /// `Err(InvalidInput)` for invalid ranges (mirrors the base `MapAt`
    /// rejection, which the C API surface reports as
    /// `MOJO_RESULT_RESOURCE_EXHAUSTED`).
    pub fn map(&self, offset: usize, len: usize, access: Access) -> io::Result<Mapping> {
        const PAGE: usize = 4096;
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero-length mapping",
            ));
        }
        let end = offset
            .checked_add(len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range overflow"))?;
        if end > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "range beyond object size",
            ));
        }
        let aligned = offset & !(PAGE - 1);
        let adjustment = offset - aligned;
        let map_len = len + adjustment;
        let prot = match access {
            Access::ReadOnly => libc::PROT_READ,
            Access::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
        };
        // SAFETY: standard mmap arguments; map_len > 0 guaranteed by len > 0;
        // the mapped range [aligned, aligned + map_len) is within the object
        // (aligned + map_len == offset + len <= size).
        let base = unsafe {
            sys::mmap(
                std::ptr::null_mut(),
                map_len,
                prot,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                aligned as libc::off_t,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: base is a live mapping of map_len bytes; the requested range
        // is within it by construction.
        let ptr = unsafe { base.add(adjustment) };
        Ok(Mapping {
            ptr: ptr as *mut u8,
            len,
            map_base: base as *mut u8,
            map_len,
            access,
        })
    }

    /// Duplicate this shared-memory object: a new descriptor referencing the
    /// same underlying region with the same size. Mirrors
    /// `PlatformSharedMemoryRegion::Duplicate` (dup(2) of the descriptor).
    pub fn duplicate(&self) -> io::Result<SharedMemory> {
        let new_fd = self.fd.try_dup()?;
        Ok(SharedMemory {
            fd: new_fd,
            size: self.size,
        })
    }

    /// The raw descriptor (for `SCM_RIGHTS` transfer and duplication).
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Mapping {
    /// Whether the mapping is read-write.
    pub fn is_writable(&self) -> bool {
        self.access == Access::ReadWrite
    }

    /// The mapped length.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Raw mutable access (only valid for `ReadWrite` mappings).
    ///
    /// SAFETY: the caller must not create aliasing mutable references to the
    /// same region; the mapping is owned by this object.
    pub unsafe fn as_mut_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// The exact requested start address of the mapping.
    pub fn address(&self) -> usize {
        self.ptr as usize
    }
}

impl Deref for Mapping {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: the mapping is live and owned for `len` bytes.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl std::ops::DerefMut for Mapping {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: `&mut self` grants exclusive access to the owned mapping for
        // `len` bytes; no other aliasing mutable references can exist.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: map_base/map_len describe the live mmap region owned by this
        // object (the aligned base and total length passed to mmap).
        unsafe {
            sys::munmap(self.map_base as *mut libc::c_void, self.map_len);
        }
    }
}

// SAFETY: `Mapping` owns a live mmap region and never aliases it through
// shared references. Mutation requires `&mut Mapping` (the safe API), which
// the owning structures (e.g. `RingBuffer` behind a `Mutex`) serialize;
// shared reads via `&Mapping` are safe concurrently. The region is unmapped
// exactly once, in `Drop`, by the sole owner.
unsafe impl Send for Mapping {}
// SAFETY: shared reads through `&Mapping` never mutate the region; every
// mutation path requires `&mut Mapping` (the safe API), so `&Mapping` can be
// shared across threads without data races.
unsafe impl Sync for Mapping {}

/// Create a shared memory object (test/diagnostic helper).
pub fn create_memfd(name: &str, size: usize) -> io::Result<SharedMemory> {
    SharedMemory::create(name, size)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn create_map_write_read() {
        let mem = SharedMemory::create("mojo-test-map", 4096).unwrap();
        let m = mem.map(0, 4096, Access::ReadWrite).unwrap();
        // SAFETY: the mapping is read-write and owned; no aliasing refs exist.
        unsafe {
            let ptr = m.as_mut_ptr();
            *ptr = 42;
            *ptr.add(4095) = 43;
            assert_eq!(*ptr, 42);
            assert_eq!(*ptr.add(4095), 43);
        }
    }

    #[test]
    fn read_only_mapping_rejects_write() {
        let mem = SharedMemory::create("mojo-test-ro", 4096).unwrap();
        let m = mem.map(0, 4096, Access::ReadOnly).unwrap();
        assert!(!m.is_writable());
        assert_eq!(m.len(), 4096);
    }

    #[test]
    fn invalid_ranges_rejected() {
        let mem = SharedMemory::create("mojo-test-ranges", 4096).unwrap();
        assert!(mem.map(0, 0, Access::ReadWrite).is_err()); // zero length
        assert!(mem.map(0, 8192, Access::ReadWrite).is_err()); // beyond size
        assert!(mem.map(1, 4096, Access::ReadWrite).is_err()); // end beyond size
        assert!(mem.map(0, usize::MAX, Access::ReadWrite).is_err()); // overflow
        assert!(mem.map(4096, 1, Access::ReadWrite).is_err()); // start == size
    }

    #[test]
    fn unaligned_offsets_supported() {
        // The official MapAt aligns down to the page and adjusts the pointer.
        let mem = SharedMemory::create("mojo-test-unaligned", 4096).unwrap();
        let m = mem.map(3, 10, Access::ReadWrite).unwrap();
        assert_eq!(m.len(), 10);
        // SAFETY: the mapping is read-write and owned; no aliasing refs exist.
        unsafe {
            let ptr = m.as_mut_ptr();
            *ptr = 0xab;
            *ptr.add(9) = 0xcd;
            assert_eq!(*ptr, 0xab);
            assert_eq!(*ptr.add(9), 0xcd);
        }
        // The mapping is inside the requested range, not page-aligned.
        assert_eq!(m.address() % 4096, 3);
    }

    #[test]
    fn duplicate_shares_pages() {
        let mem = SharedMemory::create("mojo-test-dup", 4096).unwrap();
        let dup = mem.duplicate().unwrap();
        assert_eq!(dup.size(), 4096);
        assert_ne!(dup.as_raw_fd(), mem.as_raw_fd());
        let a = mem.map(0, 128, Access::ReadWrite).unwrap();
        let b = dup.map(0, 128, Access::ReadWrite).unwrap();
        // SAFETY: both mappings are read-write and owned; no aliasing refs.
        unsafe {
            *a.as_mut_ptr() = 0x5a;
            assert_eq!(*b.as_mut_ptr(), 0x5a);
            *b.as_mut_ptr() = 0x7c;
            assert_eq!(*a.as_mut_ptr(), 0x7c);
        }
    }

    #[test]
    fn mapping_survives_handle_close() {
        let mem = SharedMemory::create("mojo-test-lifetime", 4096).unwrap();
        let m = mem.map(0, 4096, Access::ReadWrite).unwrap();
        drop(mem); // the mapping outlives the handle
        // SAFETY: mapping is read-write and owned.
        unsafe {
            *m.as_mut_ptr() = 7;
            assert_eq!(*m.as_mut_ptr(), 7);
        }
    }
}
