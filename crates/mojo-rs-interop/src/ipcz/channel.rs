//! Channel transport — the socket side of a NodeLink.
//!
//! Messages are framed with the 16-byte `IpczHeader` and transmitted with one
//! `sendmsg` per message; descriptors are attached via `SCM_RIGHTS` and arrive
//! with the first byte of the message that carried them.
//!
//! The receive side reassembles the byte stream, tracking which descriptors
//! arrived with which byte offset, and hands out complete messages with their
//! descriptor list. A message's descriptors are the ones attached to the
//! chunk that contained its first byte — the kernel guarantees they are
//! delivered with the first byte of the `sendmsg` that carried them.
//!
//! Malformed framing (bad header sizes, truncated messages, impossible
//! handle counts) is classified, never panicked on.

use std::collections::VecDeque;
use std::os::unix::io::RawFd;

use mojo_rs_platform::fd::OwnedFd;
use mojo_rs_platform::socket;

use crate::ipcz::wire::{self, WireError};

/// A complete channel message: the ipcz payload plus attached descriptors.
#[derive(Debug)]
pub struct IncomingMessage {
    /// The ipcz payload (starting at the ipcz `MessageHeader`).
    pub payload: Vec<u8>,
    /// Attached descriptors, in order.
    pub fds: Vec<OwnedFd>,
}

/// The result of a nonblocking receive attempt.
#[derive(Debug)]
pub enum RecvResult {
    /// A complete message was assembled.
    Message(IncomingMessage),
    /// No complete message and no pending socket data.
    WouldBlock,
    /// The peer closed the channel.
    PeerClosed,
}

/// A byte offset within the receive stream plus the descriptors delivered at
/// that offset.
struct FdMark {
    offset: usize,
    fds: Vec<OwnedFd>,
}

/// Errors from channel operation.
#[derive(Debug)]
pub enum ChannelError {
    /// I/O failure.
    Io(std::io::Error),
    /// The peer closed the channel (recvmsg returned 0).
    PeerClosed,
    /// Malformed framing.
    Wire(WireError),
}

impl From<std::io::Error> for ChannelError {
    fn from(e: std::io::Error) -> Self {
        ChannelError::Io(e)
    }
}

impl From<WireError> for ChannelError {
    fn from(e: WireError) -> Self {
        ChannelError::Wire(e)
    }
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::Io(e) => write!(f, "channel io: {e}"),
            ChannelError::PeerClosed => write!(f, "channel peer closed"),
            ChannelError::Wire(e) => write!(f, "channel wire: {e}"),
        }
    }
}

/// A bidirectional channel over an inherited socket descriptor.
pub struct Channel {
    /// The socket.
    fd: OwnedFd,
    /// Accumulated receive bytes not yet consumed by a complete message.
    recv_buf: Vec<u8>,
    /// Descriptor delivery marks within `recv_buf`.
    fd_marks: VecDeque<FdMark>,
    /// Scratch buffer for `recvmsg`.
    scratch: Vec<u8>,
    /// The maximum number of fds to accept per `recvmsg`.
    max_fds_per_recv: usize,
}

impl Channel {
    /// Adopt a socket descriptor and put it into nonblocking mode. The
    /// acceptor drives reads with poll() plus nonblocking recvmsg so the
    /// receive loop can distinguish "no data" (WouldBlock) from "no message".
    pub fn adopt(fd: RawFd) -> std::io::Result<Channel> {
        if fd < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative socket descriptor",
            ));
        }
        // SAFETY: the harness transfers ownership of the socket descriptor;
        // it is closed when this Channel is dropped.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: fd is owned and valid.
        let flags = unsafe { mojo_rs_platform::sys::fcntl_getfl(fd) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fd is owned and valid.
        let rc = unsafe { mojo_rs_platform::sys::fcntl_setfl(fd, flags | libc::O_NONBLOCK) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Channel {
            fd: owned,
            recv_buf: Vec::with_capacity(4096),
            fd_marks: VecDeque::new(),
            scratch: vec![0u8; 64 * 1024],
            max_fds_per_recv: 32,
        })
    }

    /// The raw socket descriptor.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Whether a complete message is already buffered without touching the
    /// socket.
    pub fn has_complete_message(&self) -> bool {
        if self.recv_buf.len() < wire::IPCZ_HEADER_SIZE {
            return false;
        }
        let num_bytes = u32::from_le_bytes([
            self.recv_buf[4],
            self.recv_buf[5],
            self.recv_buf[6],
            self.recv_buf[7],
        ]) as usize;
        num_bytes >= wire::IPCZ_HEADER_SIZE && num_bytes <= self.recv_buf.len()
    }

    /// Wait until the socket is readable or `timeout` elapses.
    ///
    /// Returns true if readable (or a complete message is already buffered).
    pub fn wait_readable(&self, timeout: std::time::Duration) -> std::io::Result<bool> {
        if self.has_complete_message() {
            return Ok(true);
        }
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: pfd is a valid pointer to one pollfd.
            let rc = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as i32) };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            return Ok(rc > 0);
        }
    }

    /// Try to assemble a complete message without blocking.
    ///
    /// Returns `RecvResult::WouldBlock` when no complete message is available
    /// and the socket has no pending data.
    pub fn recv_available(&mut self) -> Result<RecvResult, ChannelError> {
        loop {
            if let Some(msg) = self.try_assemble()? {
                return Ok(RecvResult::Message(msg));
            }
            // READ-SIZING: read exactly the bytes needed to complete the
            // message at the front of the buffer (the header first, then the
            // remainder). A single `recvmsg` therefore never spans two
            // messages, and any descriptors it carries belong to the message
            // it begins — matching the official `ChannelPosix::OnFdReadable`,
            // whose `next_read_size` drives the read buffer. (A large fixed
            // read would coalesce several messages, and `SCM_RIGHTS` attaches
            // the descriptors to the read's first byte — the wrong message.)
            let needed = if self.recv_buf.len() < wire::IPCZ_HEADER_SIZE {
                wire::IPCZ_HEADER_SIZE - self.recv_buf.len()
            } else {
                let num_bytes = u32::from_le_bytes([
                    self.recv_buf[4],
                    self.recv_buf[5],
                    self.recv_buf[6],
                    self.recv_buf[7],
                ]) as usize;
                if num_bytes < wire::IPCZ_HEADER_SIZE {
                    return Err(ChannelError::Wire(WireError::BadChannelMessageSize(
                        num_bytes as u32,
                    )));
                }
                // Guard against absurd sizes so a malformed header cannot
                // trigger unbounded buffering.
                if num_bytes > 256 * 1024 * 1024 {
                    return Err(ChannelError::Wire(WireError::BadChannelMessageSize(
                        num_bytes as u32,
                    )));
                }
                num_bytes - self.recv_buf.len()
            };
            let mark_offset = self.recv_buf.len();
            if self.scratch.len() < needed {
                self.scratch.resize(needed, 0);
            }
            let scratch = &mut self.scratch[..needed];
            match socket::recv_with_fds(&self.fd, scratch, self.max_fds_per_recv) {
                Ok(Some(sm)) => {
                    self.recv_buf.extend_from_slice(&sm.data);
                    if !sm.fds.is_empty() {
                        self.fd_marks.push_back(FdMark {
                            offset: mark_offset,
                            fds: sm.fds,
                        });
                    }
                }
                Ok(None) => {
                    // EOF: peer closed. If a partial message is pending, that
                    // is a protocol error (truncation).
                    if !self.recv_buf.is_empty() {
                        return Err(ChannelError::Wire(WireError::TruncatedMessage));
                    }
                    return Ok(RecvResult::PeerClosed);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(RecvResult::WouldBlock);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(ChannelError::Io(e)),
            }
        }
    }

    /// Receive the next complete message, blocking until one is available or
    /// the peer closes.
    pub fn recv(&mut self) -> Result<Option<IncomingMessage>, ChannelError> {
        loop {
            if !self.wait_readable(std::time::Duration::from_secs(30))? {
                return Err(ChannelError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "channel read timed out",
                )));
            }
            match self.recv_available()? {
                RecvResult::Message(m) => return Ok(Some(m)),
                RecvResult::WouldBlock => continue,
                RecvResult::PeerClosed => return Ok(None),
            }
        }
    }

    /// Send one framed message with attached descriptors.
    ///
    /// The message is sent as a single `sendmsg`; if the socket accepts only
    /// part of it, the remainder is sent without descriptors (they were
    /// already delivered with the first byte).
    pub fn send(&self, payload: &[u8], fds: &[RawFd]) -> Result<(), ChannelError> {
        let frame = wire::encode_channel_message(payload, fds.len() as u16);
        let mut sent = 0usize;
        while sent < frame.len() {
            let n = if sent == 0 && !fds.is_empty() {
                socket::send_with_fds(&self.fd, &frame, fds)?
            } else {
                socket::send(&self.fd, &frame[sent..])?
            };
            if n == 0 {
                return Err(ChannelError::PeerClosed);
            }
            sent += n;
        }
        Ok(())
    }

    /// Attempt to parse one complete message from the front of the buffer.
    fn try_assemble(&mut self) -> Result<Option<IncomingMessage>, ChannelError> {
        if self.recv_buf.len() < wire::IPCZ_HEADER_SIZE {
            return Ok(None);
        }
        // Validate the header fields before trusting num_bytes.
        let size = u16::from_le_bytes([self.recv_buf[0], self.recv_buf[1]]);
        if size != wire::IPCZ_HEADER_SIZE as u16 {
            return Err(ChannelError::Wire(WireError::UnknownChannelHeaderSize(
                size,
            )));
        }
        let num_handles = u16::from_le_bytes([self.recv_buf[2], self.recv_buf[3]]);
        let num_bytes = u32::from_le_bytes([
            self.recv_buf[4],
            self.recv_buf[5],
            self.recv_buf[6],
            self.recv_buf[7],
        ]) as usize;
        if num_bytes < wire::IPCZ_HEADER_SIZE {
            return Err(ChannelError::Wire(WireError::BadChannelMessageSize(
                num_bytes as u32,
            )));
        }
        if num_bytes > self.recv_buf.len() {
            // Not enough bytes yet; also guard against absurd sizes so a
            // malformed header cannot trigger unbounded buffering.
            if num_bytes > 256 * 1024 * 1024 {
                return Err(ChannelError::Wire(WireError::BadChannelMessageSize(
                    num_bytes as u32,
                )));
            }
            return Ok(None);
        }
        let payload = self.recv_buf[wire::IPCZ_HEADER_SIZE..num_bytes].to_vec();
        // The message's descriptors are the ones marked at offset 0 (its first
        // byte). Any marks at offset 0 are consumed; marks before the message
        // start cannot exist because we parse strictly from the front.
        let mut fds = Vec::new();
        while self.fd_marks.front().is_some_and(|front| front.offset == 0) {
            let mark = self
                .fd_marks
                .pop_front()
                .ok_or(ChannelError::Wire(WireError::BadDriverObjects))?;
            fds.extend(mark.fds);
        }
        if fds.len() != num_handles as usize {
            return Err(ChannelError::Wire(WireError::BadDriverObjects));
        }
        // Consume the message bytes and shift all mark offsets.
        self.recv_buf.drain(..num_bytes);
        for mark in &mut self.fd_marks {
            mark.offset = mark.offset.saturating_sub(num_bytes);
        }
        Ok(Some(IncomingMessage { payload, fds }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::os::unix::io::IntoRawFd;

    use mojo_rs_platform::shm::SharedMemory;
    use mojo_rs_platform::socket::{SocketPair, socketpair};

    #[test]
    fn frame_roundtrip_with_fd() {
        let pair = socketpair().unwrap();
        // Transfer ownership of the socket fds into the channels so no fd is
        // aliased (a double-close would corrupt other tests' descriptors in
        // this process).
        let SocketPair { a, b } = pair;
        let mut a = Channel::adopt(a.into_raw_fd()).unwrap();
        let b = Channel::adopt(b.into_raw_fd()).unwrap();
        // Build a payload with a driver object + a memfd.
        let mem = SharedMemory::create("chan-test", 4096).unwrap();
        let raw = mem.as_raw_fd();
        // SAFETY: dup is a plain syscall on a valid descriptor owned by `mem`.
        let fd = unsafe { libc::dup(raw) };
        assert!(fd >= 0);
        b.send(b"payload", &[fd]).unwrap();
        let msg = a.recv().unwrap().expect("message");
        assert_eq!(msg.payload, b"payload");
        assert_eq!(msg.fds.len(), 1);
        drop(msg.fds);
        drop(b);
        drop(a);
        // SAFETY: fd is the test's own duplicate; closing it is correct.
        unsafe { libc::close(fd) };
        drop(mem);
    }

    #[test]
    fn eof_detected() {
        let pair = socketpair().unwrap();
        let SocketPair { a, b } = pair;
        let mut a = Channel::adopt(a.into_raw_fd()).unwrap();
        drop(b);
        assert!(a.recv().unwrap().is_none());
    }

    #[test]
    fn fd_association_survives_dense_stream() {
        // The exhaustion court's failure mode: a burst of messages where the
        // fd-bearing message is NOT first. With read-sizing, each recvmsg
        // spans one message, so the fd stays attached to its message even
        // when the socket coalesces the sender's writes.
        let pair = socketpair().unwrap();
        let SocketPair { a, b } = pair;
        let mut recv = Channel::adopt(a.into_raw_fd()).unwrap();
        let send = Channel::adopt(b.into_raw_fd()).unwrap();
        let mem = SharedMemory::create("chan-fd-stream", 4096).unwrap();
        let raw = mem.as_raw_fd();
        // SAFETY: dup is a plain syscall on a valid owned descriptor.
        let fd = unsafe { libc::dup(raw) };
        assert!(fd >= 0);
        // Send [m0 (no fd), m1 (fd), m2 (no fd)] back to back.
        send.send(b"m0", &[]).unwrap();
        send.send(b"m1", &[fd]).unwrap();
        send.send(b"m2", &[]).unwrap();
        let m0 = recv.recv().unwrap().expect("m0");
        assert_eq!(m0.payload, b"m0");
        assert!(m0.fds.is_empty());
        let m1 = recv.recv().unwrap().expect("m1");
        assert_eq!(m1.payload, b"m1");
        assert_eq!(m1.fds.len(), 1);
        let m2 = recv.recv().unwrap().expect("m2");
        assert_eq!(m2.payload, b"m2");
        assert!(m2.fds.is_empty());
        drop(recv);
        drop(send);
        // SAFETY: fd is the test's own duplicate; closing it is correct.
        unsafe { libc::close(fd) };
        drop(mem);
    }

    #[test]
    fn truncated_stream_rejected() {
        let pair = socketpair().unwrap();
        let SocketPair { a, b } = pair;
        let mut a = Channel::adopt(a.into_raw_fd()).unwrap();
        // Write a header claiming 1000 bytes but only 20, then close the peer
        // so EOF surfaces the truncation immediately.
        let mut frame = vec![0u8; 20];
        frame[0] = 16;
        frame[1] = 0;
        frame[4..8].copy_from_slice(&1000u32.to_le_bytes());
        socket::send(&b, &frame).unwrap();
        drop(b);
        let err = a.recv().unwrap_err();
        assert!(matches!(
            err,
            ChannelError::Wire(WireError::TruncatedMessage)
        ));
    }
}
