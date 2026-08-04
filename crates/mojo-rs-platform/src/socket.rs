//! Unix-domain sockets: socketpair creation and `SCM_RIGHTS` descriptor
//! transfer with ancillary-data validation.
//!
//! The transfer layer is the security-sensitive boundary: descriptor counts,
//! truncation, and malformed control messages must be handled exactly.
//! Every `recvmsg` validates the control area before trusting it.

use std::io;
use std::os::unix::io::RawFd;

use crate::fd::OwnedFd;
use crate::sys;

/// A connected pair of Unix-domain stream sockets.
pub struct SocketPair {
    /// Endpoint A.
    pub a: OwnedFd,
    /// Endpoint B.
    pub b: OwnedFd,
}

/// Create a nonblocking Unix-domain stream socketpair.
pub fn socketpair() -> io::Result<SocketPair> {
    let mut fds = [-1; 2];
    // SAFETY: `fds` is a valid 2-element output array.
    let rc = unsafe { sys::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair returned two fresh, owned descriptors.
    let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: as above for the second descriptor.
    let b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    set_nonblocking(&a)?;
    set_nonblocking(&b)?;
    Ok(SocketPair { a, b })
}

/// Set a descriptor to nonblocking mode.
pub fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fd is owned and valid.
    let flags = unsafe { sys::fcntl_getfl(fd.as_raw_fd()) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is owned and valid.
    let rc = unsafe { sys::fcntl_setfl(fd.as_raw_fd(), flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A message sent or received on a socket: payload plus attached descriptors.
#[derive(Debug)]
pub struct SocketMessage {
    /// Payload bytes.
    pub data: Vec<u8>,
    /// Received descriptors (empty for sends).
    pub fds: Vec<OwnedFd>,
}

/// Send payload bytes plus descriptors via `SCM_RIGHTS`.
pub fn send_with_fds(fd: &OwnedFd, data: &[u8], fds: &[RawFd]) -> io::Result<usize> {
    let iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };
    let mut cmsg_buf = [0u8; 1024];
    let msg = build_msghdr_with_fds(&iov, fds, &mut cmsg_buf)?;
    // SAFETY: msg points to a fully initialized msghdr with valid iov and
    // control buffers.
    let n = unsafe { sys::sendmsg(fd.as_raw_fd(), &msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

/// Send payload bytes without descriptors.
pub fn send(fd: &OwnedFd, data: &[u8]) -> io::Result<usize> {
    let iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };
    // SAFETY: msghdr is a plain C struct; zeroed is a valid
    // initial state, and every field we rely on is set before use.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &iov as *const libc::iovec as *mut libc::iovec;
    msg.msg_iovlen = 1;
    // SAFETY: msg points to a fully initialized msghdr with a valid iov.
    let n = unsafe { sys::sendmsg(fd.as_raw_fd(), &msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

/// Receive payload bytes plus any attached descriptors, validating the
/// ancillary data. Returns `Ok(None)` when the peer closed the connection
/// (zero-length read with no control data).
pub fn recv_with_fds(
    fd: &OwnedFd,
    buf: &mut [u8],
    max_fds: usize,
) -> io::Result<Option<SocketMessage>> {
    let iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    // Control buffer sized for up to max_fds descriptors (plus alignment).
    // SAFETY: CMSG_SPACE is a size-computing macro with no memory access.
    let cmsg_len = unsafe {
        libc::CMSG_SPACE((max_fds * std::mem::size_of::<RawFd>()) as libc::c_uint) as usize
    };
    let mut cmsg_buf = vec![0u8; cmsg_len.max(64)];
    // SAFETY: msghdr is a plain C struct; zeroed is a valid
    // initial state, and every field we rely on is set before use.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &iov as *const libc::iovec as *mut libc::iovec;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_buf.len();

    // SAFETY: msg points to a fully initialized msghdr with valid iov and
    // control buffers.
    let n = unsafe { sys::recvmsg(fd.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let n = n as usize;
    // On a stream socket a zero-length read is EOF: the kernel does NOT reset
    // msg_controllen when there is no data (it leaves our pre-set buffer size),
    // so a control-length check would misclassify EOF as an empty frame.
    if n == 0 {
        return Ok(None); // peer closed
    }

    // Validate and extract the control data.
    let fds = extract_fds(&msg, &cmsg_buf, max_fds)?;

    let mut out = buf[..n].to_vec();
    out.shrink_to_fit();
    Ok(Some(SocketMessage { data: out, fds }))
}

/// Build a `msghdr` with `SCM_RIGHTS` control data for `fds`.
fn build_msghdr_with_fds(
    iov: &libc::iovec,
    fds: &[RawFd],
    cmsg_buf: &mut [u8],
) -> io::Result<libc::msghdr> {
    if fds.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fds must be non-empty for SCM_RIGHTS",
        ));
    }
    let needed = unsafe {
        // SAFETY: CMSG_SPACE is a size-computing macro with no memory access.
        libc::CMSG_SPACE((fds.len() * std::mem::size_of::<RawFd>()) as libc::c_uint) as usize
    };
    if cmsg_buf.len() < needed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control buffer too small",
        ));
    }
    // SAFETY: msghdr is a plain C struct; zeroed is a valid
    // initial state, and every field we rely on is set before use.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov as *const libc::iovec as *mut libc::iovec;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = needed;

    // SAFETY: cmsg_buf is writable for `needed` bytes; the CMSG_* macros only
    // access that region; `fds` is a valid slice.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len =
            libc::CMSG_LEN((fds.len() * std::mem::size_of::<RawFd>()) as libc::c_uint) as _;
        let data_ptr = libc::CMSG_DATA(cmsg);
        std::ptr::copy_nonoverlapping(fds.as_ptr(), data_ptr as *mut RawFd, fds.len());
    }
    Ok(msg)
}

/// Extract validated descriptors from a received `msghdr`.
fn extract_fds(msg: &libc::msghdr, _buf: &[u8], max_fds: usize) -> io::Result<Vec<OwnedFd>> {
    let mut out = Vec::new();
    // SAFETY: iterate the control messages exactly as libc intends; msg was
    // filled by recvmsg with msg_controllen bytes.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let len = (*cmsg).cmsg_len as usize;
                // SAFETY: CMSG_LEN(0) is a constant size computed by the macro.
                let base = unsafe { libc::CMSG_LEN(0) } as usize;
                let data_len = len
                    .checked_sub(base)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short cmsg"))?;
                let fd_count = data_len / std::mem::size_of::<RawFd>();
                if fd_count == 0 {
                    // Malformed: SCM_RIGHTS with no descriptors.
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "SCM_RIGHTS with zero descriptors",
                    ));
                }
                if out.len() + fd_count > max_fds {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "too many descriptors",
                    ));
                }
                let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                for i in 0..fd_count {
                    let fd = *data.add(i);
                    if fd < 0 {
                        // Negative fd in SCM_RIGHTS: malformed.
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "negative descriptor in SCM_RIGHTS",
                        ));
                    }
                    // SAFETY: the kernel transferred ownership of `fd` to us.
                    out.push(unsafe { OwnedFd::from_raw_fd(fd) });
                }
            }
            cmsg = libc::CMSG_NXTHDR(msg, cmsg);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::shm;

    #[test]
    fn socketpair_roundtrip() {
        let pair = socketpair().unwrap();
        let mut buf = [0u8; 64];
        send(&pair.a, b"hello").unwrap();
        let msg = recv_with_fds(&pair.b, &mut buf, 4).unwrap().unwrap();
        assert_eq!(msg.data, b"hello");
        assert!(msg.fds.is_empty());
    }

    #[test]
    fn fd_transfer_via_scm_rights() {
        let pair = socketpair().unwrap();
        // Create a memfd to transfer.
        let mem = shm::create_memfd("mojo-test", 16).unwrap();
        let raw = mem.as_raw_fd();
        send_with_fds(&pair.a, b"x", &[raw]).unwrap();
        let mut buf = [0u8; 64];
        let msg = recv_with_fds(&pair.b, &mut buf, 4).unwrap().unwrap();
        assert_eq!(msg.data, b"x");
        assert_eq!(msg.fds.len(), 1);
        // The received fd is a distinct descriptor number (transferred, not
        // duplicated), and it is usable (ftruncate succeeds).
        let received = &msg.fds[0];
        assert_ne!(received.as_raw_fd(), raw);
        // SAFETY: received fd is owned and valid.
        let rc = unsafe { sys::ftruncate(received.as_raw_fd(), 32) };
        assert_eq!(rc, 0);
    }

    #[test]
    fn peer_close_detected() {
        let pair = socketpair().unwrap();
        drop(pair.a);
        let mut buf = [0u8; 64];
        let msg = recv_with_fds(&pair.b, &mut buf, 4).unwrap();
        assert!(msg.is_none());
    }
}
