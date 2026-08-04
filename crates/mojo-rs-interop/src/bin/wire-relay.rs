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
//! Descriptors attached via SCM_RIGHTS are forwarded with the first chunk that
//! carries them (the kernel delivers them at the start of a message).
//!
//! Ownership: the fds are shared (Arc) and owned by the main thread, so a
//! thread finishing its direction never closes an fd the other thread is
//! blocked reading (closing an fd from another thread does not wake a blocked
//! read, which would hang the relay forever).

use std::os::unix::io::{FromRawFd, RawFd};
use std::process::ExitCode;
use std::sync::Arc;

use mojo_rs_platform::fd::OwnedFd;
use mojo_rs_platform::socket;

fn forward(from: &OwnedFd, to: &OwnedFd, capture_path: &str) -> std::io::Result<()> {
    let mut capture = std::fs::File::create(capture_path)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match socket::recv_with_fds(from, &mut buf, 32)? {
            None => break, // peer closed
            Some(msg) => {
                use std::io::Write;
                capture.write_all(&msg.data)?;
                let raw_fds: Vec<RawFd> = msg.fds.iter().map(|f| f.as_raw_fd()).collect();
                // Forward the whole chunk (with any attached descriptors),
                // retrying on EAGAIN and partial sends.
                let mut sent = 0usize;
                while sent < msg.data.len() {
                    match if raw_fds.is_empty() {
                        socket::send(to, &msg.data[sent..])
                    } else if sent == 0 {
                        socket::send_with_fds(to, &msg.data, &raw_fds)
                    } else {
                        socket::send(to, &msg.data[sent..])
                    } {
                        Ok(n) => sent += n,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
    Ok(())
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
    let t1 = std::thread::spawn(move || forward(&a1, &b1, &cap_a));
    let a2 = Arc::clone(&fd_a);
    let b2 = Arc::clone(&fd_b);
    let t2 = std::thread::spawn(move || forward(&b2, &a2, &cap_b));
    let _ = t1.join();
    let _ = t2.join();
    ExitCode::SUCCESS
}
