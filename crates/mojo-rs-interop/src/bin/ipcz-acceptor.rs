//! ipcz-acceptor — the native Rust counterpart of the oracle's
//! `invite-acceptor` mode: accepts an invitation over an inherited channel
//! socket, exchanges a message plus a wrapped descriptor through the
//! bootstrap pipe, and writes a casefile-format event stream.
//!
//! Usage: ipcz-acceptor <socket-fd> <events.jsonl>
//!
//! Exit status 0 iff the exchange completed and verified.

use std::process::ExitCode;

use mojo_rs_casefile::events::serialize_events;
use mojo_rs_interop::ipcz::acceptor::{Acceptor, AcceptorError, RunOutcome};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: ipcz-acceptor <socket-fd> <events.jsonl>");
        return ExitCode::FAILURE;
    }
    let Ok(fd) = args[1].parse::<i32>() else {
        eprintln!("invalid socket-fd: {}", args[1]);
        return ExitCode::FAILURE;
    };
    let events_path = &args[2];

    let mut acceptor = match Acceptor::new(fd) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("acceptor init failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let outcome = match acceptor.run() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("acceptor run failed: {e}");
            if let Err(we) = write_events(events_path, acceptor.events()) {
                eprintln!("failed to write events: {we}");
            }
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = write_events(events_path, acceptor.events()) {
        eprintln!("failed to write events: {e}");
        return ExitCode::FAILURE;
    }

    match outcome {
        RunOutcome::Success => ExitCode::SUCCESS,
        RunOutcome::PeerClosed => {
            eprintln!("acceptor: peer closed before the exchange completed");
            ExitCode::FAILURE
        }
    }
}

/// Serialize the events to the events file.
fn write_events(path: &str, events: &[mojo_rs_casefile::events::Event]) -> std::io::Result<()> {
    let out = serialize_events(events).map_err(std::io::Error::other)?;
    std::fs::write(path, out)
}

// Keep `AcceptorError` referenced so its Display is exercised in debug builds.
#[allow(dead_code)]
fn _display_error(e: &AcceptorError) -> String {
    format!("{e}")
}
