//! wire-relay — a man-in-the-middle relay that forwards a byte stream plus
//! attached descriptors (`SCM_RIGHTS`) between two sockets while capturing the
//! traffic to a file.
//!
//! The oracle broker and acceptor are connected through this relay so the raw
//! official ipcz node-link wire traffic (handshake, shared-memory setup,
//! invitation, portal messages) can be captured as differential evidence for
//! the future native interop implementation.
//!
//! Usage:
//!   wire-relay <fd-a> <fd-b> <capture-a> <capture-b>
//!
//! Bytes received on fd-a are appended to capture-a and forwarded to fd-b;
//! bytes received on fd-b are appended to capture-b and forwarded to fd-a.
//!
//! IMPORTANT: the relay preserves MESSAGE BOUNDARIES. Descriptors arrive via
//! `SCM_RIGHTS` attached to the first byte of the message that carried them.
//! If the relay re-chunked the stream (forwarding several messages as one
//! write), the descriptors would be attached to the wrong message on the
//! receiving socket and the fd-to-message association would be corrupted
//! (observable under dense traffic, e.g. the exhaustion court's 1486 portal
//! transfers). The relay therefore reassembles the framed messages and
//! forwards each complete message — and only that message's bytes — in a
//! single write with its descriptors.
//!
//! Ownership: the fds are shared (Arc) and owned by the main thread, so a
//! thread finishing its direction never closes an fd the other thread is
//! blocked reading (closing an fd from another thread does not wake a blocked
//! read, which would hang the relay forever).

use std::os::unix::io::{FromRawFd, RawFd};
use std::process::ExitCode;
use std::sync::Arc;

use mojo_rs_interop::ipcz::wire::{IPCZ_HEADER_SIZE, parse_channel_message};
use mojo_rs_platform::fd::OwnedFd;
use mojo_rs_platform::socket;

/// The receive direction of one socket: reassemble framed messages, capture
/// them, and forward each complete message with its descriptors.
struct Forwarder {
    from: Arc<OwnedFd>,
    to: Arc<OwnedFd>,
    capture: std::fs::File,
    /// Accumulated receive bytes not yet consumed by a complete message.
    buf: Vec<u8>,
    /// Descriptors delivered at a byte offset within `buf` (the offset of the
    /// byte that carried them — per `SCM_RIGHTS`, the first byte of the
    /// message they belong to).
    marks: Vec<(usize, Vec<OwnedFd>)>,
}

impl Forwarder {
    fn new(from: Arc<OwnedFd>, to: Arc<OwnedFd>, capture_path: &str) -> std::io::Result<Forwarder> {
        Ok(Forwarder {
            from,
            to,
            capture: std::fs::File::create(capture_path)?,
            buf: Vec::with_capacity(64 * 1024),
            marks: Vec::new(),
        })
    }

    /// Forward one complete message (a byte span `[start, end)` of `buf`),
    /// attaching the descriptors whose mark covers its first byte.
    fn forward_message(&mut self, start: usize, end: usize) -> std::io::Result<()> {
        use std::io::Write;
        let data = &self.buf[start..end];
        self.capture.write_all(data)?;
        let mut fds: Vec<OwnedFd> = Vec::new();
        // The message's descriptors are the ones marked at its first byte.
        let mut i = 0;
        while i < self.marks.len() {
            if self.marks[i].0 == start {
                fds.append(&mut self.marks[i].1);
                self.marks.remove(i);
            } else {
                i += 1;
            }
        }
        let raw: Vec<RawFd> = fds.iter().map(|f| f.as_raw_fd()).collect();
        // One write per message; the descriptors attach to its first byte.
        let mut sent = 0usize;
        while sent < data.len() {
            let r = if sent == 0 && !raw.is_empty() {
                socket::send_with_fds(&self.to, data, &raw)
            } else {
                socket::send(&self.to, &data[sent..])
            };
            match r {
                Ok(n) => sent += n,
                // The downstream peer is gone (normal teardown: a node exits
                // right after its final messages). The capture of this
                // direction already includes this message (written above), so
                // stopping here loses nothing and keeps the relay's exit
                // status clean.
                Err(e)
                    if e.kind() == std::io::ErrorKind::BrokenPipe
                        || e.kind() == std::io::ErrorKind::ConnectionReset =>
                {
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Forward everything until the peer closes. Returns an error if a
    /// malformed frame is encountered.
    fn run(mut self) -> std::io::Result<()> {
        let mut scratch = vec![0u8; 64 * 1024];
        loop {
            // Forward every complete message currently buffered.
            while let Some((_, consumed)) = self.try_split()? {
                self.forward_message(0, consumed)?;
                self.buf.drain(..consumed);
                // Shift the marks for the consumed span.
                for m in &mut self.marks {
                    m.0 = m.0.saturating_sub(consumed);
                }
            }
            // READ-SIZING: read exactly the bytes needed to complete the
            // message at the front of the buffer (the header first, then the
            // remainder), so a single `recvmsg` never spans two messages and
            // any descriptors it carries belong to the message it begins.
            // (A large fixed read would coalesce several messages, and
            // `SCM_RIGHTS` attaches the descriptors to the read's first byte
            // — the wrong message.)
            let needed = if self.buf.len() < IPCZ_HEADER_SIZE {
                IPCZ_HEADER_SIZE - self.buf.len()
            } else {
                let num_bytes =
                    u32::from_le_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]])
                        as usize;
                num_bytes.saturating_sub(self.buf.len())
            };
            if scratch.len() < needed {
                scratch.resize(needed, 0);
            }
            let mark_offset = self.buf.len();
            match socket::recv_with_fds(&self.from, &mut scratch[..needed], 32)? {
                None => break, // peer closed
                Some(sm) => {
                    self.buf.extend_from_slice(&sm.data);
                    if !sm.fds.is_empty() {
                        self.marks.push((mark_offset, sm.fds));
                    }
                }
            }
        }
        Ok(())
    }

    /// Try to split one complete framed message from the front of the buffer.
    /// Returns `(message, consumed)` or `None` when the buffer holds only a
    /// partial message (an incomplete frame is not an error).
    fn try_split(&self) -> std::io::Result<Option<((), usize)>> {
        if self.buf.len() < IPCZ_HEADER_SIZE {
            return Ok(None);
        }
        match parse_channel_message(&self.buf) {
            Ok((msg, consumed)) => {
                let _ = msg;
                Ok(Some(((), consumed)))
            }
            Err(mojo_rs_interop::ipcz::wire::WireError::TruncatedMessage) => Ok(None),
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{e:?}"),
            )),
        }
    }
}

fn forward(from: Arc<OwnedFd>, to: Arc<OwnedFd>, capture_path: &str) -> std::io::Result<()> {
    Forwarder::new(from, to, capture_path)?.run()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: wire-relay <fd-a> <fd-b> <capture-a> <capture-b>");
        return ExitCode::FAILURE;
    }
    let Ok(fd_a_num) = args[1].parse::<RawFd>() else {
        eprintln!("invalid fd-a: {}", args[1]);
        return ExitCode::FAILURE;
    };
    let Ok(fd_b_num) = args[2].parse::<RawFd>() else {
        eprintln!("invalid fd-b: {}", args[2]);
        return ExitCode::FAILURE;
    };
    // SAFETY: the caller transferred ownership of these inherited fds.
    let fd_a = Arc::new(unsafe { OwnedFd::from_raw_fd(fd_a_num) });
    // SAFETY: as above.
    let fd_b = Arc::new(unsafe { OwnedFd::from_raw_fd(fd_b_num) });

    // Both threads share both fds; the main thread keeps them alive until both
    // directions finish (no cross-thread close while a read is in flight).
    let a1 = Arc::clone(&fd_a);
    let b1 = Arc::clone(&fd_b);
    let cap_a = args[3].clone();
    let cap_b = args[4].clone();
    let t1 = std::thread::spawn(move || forward(a1, b1, cap_a.as_str()));
    let a2 = Arc::clone(&fd_a);
    let b2 = Arc::clone(&fd_b);
    let t2 = std::thread::spawn(move || forward(b2, a2, cap_b.as_str()));
    let r1 = t1.join();
    let r2 = t2.join();
    match (r1, r2) {
        (Ok(Ok(())), Ok(Ok(()))) => ExitCode::SUCCESS,
        (r1, r2) => {
            eprintln!("wire relay failed: {r1:?} {r2:?}");
            ExitCode::FAILURE
        }
    }
}
