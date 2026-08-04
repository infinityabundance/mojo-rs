//! Transport abstraction: the platform-agnostic interface the core uses for
//! cross-process message delivery.
//!
//! The Linux implementation is a Unix-domain stream socket with `SCM_RIGHTS`
//! descriptor transfer (see [`crate::socket`]). Other platforms provide their
//! own implementations behind the same interface; the core never depends on
//! Unix semantics directly.

use crate::fd::OwnedFd;
use crate::socket;

/// A bidirectional byte transport between two processes.
pub trait Transport: Send + Sync {
    /// Send a frame: payload bytes plus attached descriptors.
    fn send(&self, data: &[u8], fds: &[std::os::unix::io::RawFd]) -> std::io::Result<usize>;

    /// Receive a frame (blocking).
    fn recv(&self, buf: &mut [u8], max_fds: usize) -> std::io::Result<Option<Frame>>;
}

/// A received frame.
#[derive(Debug)]
pub struct Frame {
    /// Payload bytes.
    pub data: Vec<u8>,
    /// Received descriptors.
    pub fds: Vec<OwnedFd>,
}

/// A Unix-socket transport.
pub struct UnixTransport {
    fd: OwnedFd,
}

impl UnixTransport {
    /// Wrap an owned socket descriptor.
    pub fn new(fd: OwnedFd) -> UnixTransport {
        UnixTransport { fd }
    }

    /// The underlying socket descriptor.
    pub fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.fd.as_raw_fd()
    }
}

impl Transport for UnixTransport {
    fn send(&self, data: &[u8], fds: &[std::os::unix::io::RawFd]) -> std::io::Result<usize> {
        if fds.is_empty() {
            socket::send(&self.fd, data)
        } else {
            socket::send_with_fds(&self.fd, data, fds)
        }
    }

    fn recv(&self, buf: &mut [u8], max_fds: usize) -> std::io::Result<Option<Frame>> {
        match socket::recv_with_fds(&self.fd, buf, max_fds)? {
            None => Ok(None),
            Some(m) => Ok(Some(Frame {
                data: m.data,
                fds: m.fds,
            })),
        }
    }
}

/// Create a transport pair (for bootstrap and in-process node channels).
pub fn transport_pair() -> std::io::Result<(UnixTransport, UnixTransport)> {
    let pair = socket::socketpair()?;
    Ok((UnixTransport::new(pair.a), UnixTransport::new(pair.b)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::fd::OwnedFd;
    use crate::shm::{Access, SharedMemory};
    use std::os::unix::io::RawFd;

    #[test]
    fn transport_roundtrip() {
        let (a, b) = transport_pair().unwrap();
        a.send(b"payload", &[]).unwrap();
        let mut buf = [0u8; 64];
        let frame = b.recv(&mut buf, 4).unwrap().unwrap();
        assert_eq!(frame.data, b"payload");
        assert!(frame.fds.is_empty());
    }

    /// Create a memfd pre-filled with `content` and return a duplicated
    /// descriptor (the original stays owned by the shared-memory object). The
    /// object is sized to exactly the content length so a reader can bound the
    /// read.
    fn memfd_with(content: &[u8]) -> OwnedFd {
        let len = content.len().max(1);
        let mem = SharedMemory::create("mojo-rs-xproc", len).unwrap();
        let m = mem.map(0, len, Access::ReadWrite).unwrap();
        // SAFETY: the mapping is read-write and owned; no aliasing refs.
        unsafe {
            std::ptr::copy_nonoverlapping(content.as_ptr(), m.as_mut_ptr(), content.len());
        }
        drop(m);
        // SAFETY: mem.as_raw_fd() is valid; dup creates a new owned fd.
        let dup = unsafe { crate::sys::dup(mem.as_raw_fd()) };
        assert!(dup >= 0);
        // SAFETY: dup returned a fresh owned descriptor.
        unsafe { OwnedFd::from_raw_fd(dup) }
    }

    /// Read all bytes of a seekable descriptor (memfd) from offset 0, bounded
    /// by `expected` (the exact object size).
    fn read_all(fd: &OwnedFd, expected: usize) -> Vec<u8> {
        // SAFETY: fd is owned and valid; memfd offsets are seekable.
        unsafe {
            libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET);
        }
        let mut buf = vec![0u8; expected];
        // SAFETY: buf is a valid writable buffer.
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        buf[..n.max(0) as usize].to_vec()
    }

    /// Receive with retry on EAGAIN (the sockets are nonblocking; the peer
    /// process needs scheduling time). Fails after `timeout`.
    fn recv_retry(transport: &UnixTransport, buf: &mut [u8], max_fds: usize) -> Option<Frame> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match transport.recv(buf, max_fds) {
                Ok(f) => return f,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "recv timed out waiting for peer"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("recv error: {e}"),
            }
        }
    }

    /// Child-side protocol: verify the parent's message + descriptor, reply
    /// with a message + descriptor, then observe peer close (EOF).
    fn child_protocol(fd: RawFd) {
        // SAFETY: the parent transferred ownership of this inherited fd.
        let transport = UnixTransport::new(unsafe { OwnedFd::from_raw_fd(fd) });
        let mut buf = [0u8; 256];
        let frame = recv_retry(&transport, &mut buf, 4).expect("parent frame");
        assert_eq!(frame.data, b"hello-parent");
        assert_eq!(frame.fds.len(), 1);
        assert_eq!(
            read_all(&frame.fds[0], b"from-parent".len()),
            b"from-parent"
        );

        let reply_fd = memfd_with(b"from-child");
        transport
            .send(b"hello-child", &[reply_fd.as_raw_fd()])
            .unwrap();

        // Parent closes its end: recv must observe EOF (peer death).
        let eof = recv_retry(&transport, &mut buf, 4);
        assert!(eof.is_none(), "expected peer-close EOF");
    }

    #[test]
    fn cross_process_message_and_fd_transfer() {
        const CHILD_ENV: &str = "MOJO_RS_PLATFORM_TEST_CHILD";
        const FD_ENV: &str = "MOJO_RS_PLATFORM_TEST_CHILD_FD";

        if let Ok(fd) = std::env::var(FD_ENV) {
            child_protocol(fd.parse::<RawFd>().expect("child fd parse"));
            std::process::exit(0);
        }

        let pair = crate::socket::socketpair().unwrap();
        let child_fd = pair.b.as_raw_fd();
        let parent = UnixTransport::new(pair.a);

        // The child must inherit ONLY its own endpoint (fd b). Mark the
        // parent-side endpoint (fd a) CLOEXEC so the child does not keep a copy
        // of the parent endpoint open (which would suppress the EOF/peer-close
        // observation).
        // SAFETY: parent.as_raw_fd() is owned and valid.
        let rc = unsafe { crate::sys::fcntl_setfd(parent.as_raw_fd(), libc::FD_CLOEXEC) };
        assert_eq!(rc, 0, "fcntl F_SETFD failed");

        // Spawn this same test binary as the child process, inheriting fd b.
        let exe = std::env::current_exe().expect("current exe");
        let mut child = std::process::Command::new(exe)
            .env(CHILD_ENV, "1")
            .env(FD_ENV, child_fd.to_string())
            .spawn()
            .expect("spawn child");

        // Parent: send a message + descriptor.
        let payload_fd = memfd_with(b"from-parent");
        let _sent = parent
            .send(b"hello-parent", &[payload_fd.as_raw_fd()])
            .unwrap();

        // Parent: receive the child's reply + descriptor.
        let mut buf = [0u8; 256];
        let frame = recv_retry(&parent, &mut buf, 4).expect("child frame");
        assert_eq!(frame.data, b"hello-child");
        assert_eq!(frame.fds.len(), 1);
        assert_eq!(read_all(&frame.fds[0], b"from-child".len()), b"from-child");

        // Close the parent's end: the child observes EOF and exits 0.
        drop(parent);
        let status = child.wait().expect("wait child");
        assert!(status.success(), "child failed: {status}");
    }
}
