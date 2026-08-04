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

    #[test]
    fn transport_roundtrip() {
        let (a, b) = transport_pair().unwrap();
        a.send(b"payload", &[]).unwrap();
        let mut buf = [0u8; 64];
        let frame = b.recv(&mut buf, 4).unwrap().unwrap();
        assert_eq!(frame.data, b"payload");
        assert!(frame.fds.is_empty());
    }
}
