//! File-descriptor ownership.
//!
//! `OwnedFd` is the safe RAII wrapper for raw descriptor numbers. All unsafe
//! descriptor handling in this crate flows through here so ownership is
//! explicit: exactly one `OwnedFd` owns a descriptor number at a time.

use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};

/// The invalid descriptor value.
pub const INVALID_FD: RawFd = -1;

/// An owned file descriptor: closes on drop.
#[derive(Debug)]
pub struct OwnedFd {
    fd: RawFd,
}

impl OwnedFd {
    /// Take ownership of an already-open descriptor.
    ///
    /// SAFETY: `fd` must be a valid open descriptor not owned by any other
    /// `OwnedFd`; the caller transfers ownership irrevocably.
    pub unsafe fn from_raw_fd(fd: RawFd) -> OwnedFd {
        OwnedFd { fd }
    }

    /// The raw descriptor number.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Duplicate the descriptor (`dup`), returning a new owned descriptor.
    pub fn try_dup(&self) -> io::Result<OwnedFd> {
        // SAFETY: dup is a plain syscall; the result is a new owned fd or -1.
        let new_fd = unsafe { crate::sys::dup(self.fd) };
        if new_fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: dup returned a fresh descriptor number owned by us.
            Ok(unsafe { OwnedFd::from_raw_fd(new_fd) })
        }
    }

    /// Whether the descriptor is valid.
    pub fn is_valid(&self) -> bool {
        self.fd >= 0
    }
}

impl AsRawFd for OwnedFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl FromRawFd for OwnedFd {
    /// SAFETY: `fd` must be a valid open descriptor with ownership transferred.
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        // SAFETY: same contract as the inherent method.
        unsafe { OwnedFd::from_raw_fd(fd) }
    }
}

impl IntoRawFd for OwnedFd {
    fn into_raw_fd(self) -> RawFd {
        let fd = self.fd;
        std::mem::forget(self);
        fd
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.fd >= 0 {
            // SAFETY: close is a plain syscall; the fd is owned by us.
            unsafe {
                crate::sys::close(self.fd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::sys;

    #[test]
    fn owned_fd_closes_on_drop() {
        // Use a socketpair to get real fds.
        let pair = crate::socket::socketpair().unwrap();
        let (a, b) = (pair.a, pair.b);
        let raw = a.as_raw_fd();
        // Check the fd is valid before drop.
        // SAFETY: raw comes from a live OwnedFd.
        let st = unsafe { sys::fcntl_getfd(raw) };
        assert!(st >= 0);
        drop(a);
        // After drop, fcntl should fail (EBADF).
        // SAFETY: calling fcntl on a closed fd is well-defined (returns EBADF).
        let st2 = unsafe { sys::fcntl_getfd(raw) };
        assert!(st2 < 0);
        drop(b);
    }

    #[test]
    fn duplicate_works() {
        let pair = crate::socket::socketpair().unwrap();
        let (a, _b) = (pair.a, pair.b);
        let dup = a.try_dup().unwrap();
        assert_ne!(dup.as_raw_fd(), a.as_raw_fd());
    }
}
