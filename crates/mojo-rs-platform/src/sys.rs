//! Low-level syscall wrappers.
//!
//! This module is the ONLY place in the platform crate that calls raw
//! syscalls. Each wrapper is unsafe with a documented `SAFETY:` invariant.
//! Wrappers are minimal: argument validation happens in the safe callers.

use std::os::unix::io::RawFd;

/// `close(fd)`. Returns the raw result (0 on success, -1 on error).
///
/// SAFETY: `fd` must be a valid open descriptor (or -1), and no other thread
/// may be using `fd` concurrently in a way that would make closing it
/// incorrect (closing an fd while another thread dup()s it is a caller error).
pub unsafe fn close(fd: RawFd) -> libc::c_int {
    // SAFETY: single syscall with no pointer arguments; contract per the
    // caller.
    unsafe { libc::close(fd) }
}

/// `dup(fd)`. Returns the new descriptor or -1.
///
/// SAFETY: `fd` must be a valid open descriptor.
pub unsafe fn dup(fd: RawFd) -> RawFd {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::dup(fd) }
}

/// `fcntl(fd, F_GETFD)`. Returns the flags or -1.
///
/// SAFETY: `fd` must be a valid open descriptor.
pub unsafe fn fcntl_getfd(fd: RawFd) -> libc::c_int {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::fcntl(fd, libc::F_GETFD) }
}

/// `fcntl(fd, F_SETFD, flags)`.
///
/// SAFETY: `fd` must be a valid open descriptor.
pub unsafe fn fcntl_setfd(fd: RawFd, flags: libc::c_int) -> libc::c_int {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::fcntl(fd, libc::F_SETFD, flags) }
}

/// `fcntl(fd, F_GETFL)`. Returns the file status flags or -1.
///
/// SAFETY: `fd` must be a valid open descriptor.
pub unsafe fn fcntl_getfl(fd: RawFd) -> libc::c_int {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::fcntl(fd, libc::F_GETFL) }
}

/// `fcntl(fd, F_SETFL, flags)`.
///
/// SAFETY: `fd` must be a valid open descriptor.
pub unsafe fn fcntl_setfl(fd: RawFd, flags: libc::c_int) -> libc::c_int {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }
}

/// `socketpair(domain, type, protocol, fds)`.
///
/// SAFETY: `fds` must be a valid pointer to a 2-element `[RawFd; 2]` array.
pub unsafe fn socketpair(
    domain: libc::c_int,
    kind: libc::c_int,
    protocol: libc::c_int,
    fds: *mut libc::c_int,
) -> libc::c_int {
    // SAFETY: single syscall with a validated output buffer.
    unsafe { libc::socketpair(domain, kind, protocol, fds) }
}

/// `sendmsg(fd, msg, flags)`. Returns bytes sent or -1.
///
/// SAFETY: `msg` must point to a fully initialized `msghdr` with valid
/// pointers into caller-owned buffers.
pub unsafe fn sendmsg(fd: RawFd, msg: *const libc::msghdr, flags: libc::c_int) -> isize {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::sendmsg(fd, msg, flags) }
}

/// `recvmsg(fd, msg, flags)`. Returns bytes received or -1.
///
/// SAFETY: `msg` must point to a fully initialized `msghdr` with valid
/// buffers for both the iovec and the control area.
pub unsafe fn recvmsg(fd: RawFd, msg: *mut libc::msghdr, flags: libc::c_int) -> isize {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::recvmsg(fd, msg, flags) }
}

/// `memfd_create(name, flags)`. Returns the new fd or -1.
///
/// SAFETY: `name` must be a valid NUL-terminated string pointer.
pub unsafe fn memfd_create(name: *const libc::c_char, flags: libc::c_uint) -> RawFd {
    // memfd_create may not exist on older glibc; use the syscall directly.
    // SAFETY: single syscall with a validated string pointer.
    unsafe { libc::syscall(libc::SYS_memfd_create, name, flags) as RawFd }
}

/// `ftruncate(fd, length)`.
///
/// SAFETY: `fd` must be a valid descriptor opened for writing.
pub unsafe fn ftruncate(fd: RawFd, length: libc::off_t) -> libc::c_int {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::ftruncate(fd, length) }
}

/// `mmap(addr, length, prot, flags, fd, offset)`. Returns MAP_FAILED on error.
///
/// SAFETY: see mmap(2): `addr` must be 0 or a valid hint; `length` must be
/// nonzero; `fd` must be valid for the requested mapping (or -1 for anonymous).
pub unsafe fn mmap(
    addr: *mut libc::c_void,
    length: usize,
    prot: libc::c_int,
    flags: libc::c_int,
    fd: RawFd,
    offset: libc::off_t,
) -> *mut libc::c_void {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::mmap(addr, length, prot, flags, fd, offset) }
}

/// `munmap(addr, length)`.
///
/// SAFETY: `addr`/`length` must describe a live mapping previously returned
/// by mmap.
pub unsafe fn munmap(addr: *mut libc::c_void, length: usize) -> libc::c_int {
    // SAFETY: single syscall; contract per the caller.
    unsafe { libc::munmap(addr, length) }
}

/// Whether a raw result from a libc call indicates error (-1).
pub fn is_err(result: libc::c_int) -> bool {
    result < 0
}
