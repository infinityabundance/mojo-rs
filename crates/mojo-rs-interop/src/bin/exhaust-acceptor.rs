//! exhaust-acceptor — the native Rust counterpart of the oracle's
//! `invite-acceptor-exhaust` mode: accepts an invitation over an inherited
//! channel socket and runs the Phase 5 block-capacity exhaustion scenario
//! (the broker's `RouterLinkState` pool exhausts; the broker shares a new
//! block buffer via `AddBlockBuffer`, which this acceptor adopts and then
//! resolves cross-buffer `RouterLinkState` fragments from), writing a
//! casefile-format event stream.
//!
//! Usage: exhaust-acceptor <socket-fd> <events.jsonl>
//!
//! Exit status 0 iff the exhaustion exchange completed and verified.

use std::process::ExitCode;

use mojo_rs_casefile::events::serialize_events;
use mojo_rs_interop::ipcz::routing::RoutingAcceptor;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: exhaust-acceptor <socket-fd> <events.jsonl>");
        return ExitCode::FAILURE;
    }
    let Ok(fd) = args[1].parse::<i32>() else {
        eprintln!("invalid socket-fd: {}", args[1]);
        return ExitCode::FAILURE;
    };
    let events_path = &args[2];

    let mut acceptor = match RoutingAcceptor::new(fd) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("exhaust acceptor init failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = acceptor.run_exhaust();
    let events = acceptor.events();
    if let Err(e) = write_events(events_path, events) {
        eprintln!("failed to write events: {e}");
        return ExitCode::FAILURE;
    }

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("exhaust acceptor run failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Serialize the events to the events file.
fn write_events(path: &str, events: &[mojo_rs_casefile::events::Event]) -> std::io::Result<()> {
    let out = serialize_events(events).map_err(std::io::Error::other)?;
    std::fs::write(path, out)
}
