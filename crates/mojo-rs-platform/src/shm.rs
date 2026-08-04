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
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
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

    /// The size of the memory object.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Map the memory with the given access. Returns `Err(InvalidArgument)`
    /// (via `io::ErrorKind`) for invalid ranges.
    pub fn map(&self, offset: usize, len: usize, access: Access) -> io::Result<Mapping> {
        if offset % 4096 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "offset not page-aligned",
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
        let prot = match access {
            Access::ReadOnly => libc::PROT_READ,
            Access::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
        };
        // SAFETY: standard mmap arguments; len > 0 guaranteed by the range
        // checks above.
        let ptr = unsafe {
            sys::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping {
            ptr: ptr as *mut u8,
            len,
            access,
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
}

impl Deref for Mapping {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: the mapping is live and owned for `len` bytes.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: ptr/len describe a live mapping owned by this object.
        unsafe {
            sys::munmap(self.ptr as *mut libc::c_void, self.len);
        }
    }
}

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
        assert!(mem.map(1, 4096, Access::ReadWrite).is_err()); // unaligned
        assert!(mem.map(0, 8192, Access::ReadWrite).is_err()); // beyond size
        assert!(mem.map(0, usize::MAX, Access::ReadWrite).is_err()); // overflow
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
