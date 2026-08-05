//! 3node-acceptor — the native Rust counterpart of the oracle's
//! `invite-node-b-3node` mode: the *referred* node B of the Phase 5
//! multi-node referral court.
//!
//! B is spawned with the referral transport's endpoint. It greets the broker
//! with `ConnectToReferredBroker`, is accepted with
//! `ConnectToReferredNonBroker` (adopting the broker link, the referrer link
//! transport + memory, and the initial portals on the referrer link), receives
//! the re-transferred portal Y' over the b2a pipe, bypasses the A proxy with
//! `AcceptBypassLink` to the broker (the outbound id-31 path), completes the
//! X<->Y' round trip ("hello"/"world"), and observes peer closure — writing a
//! casefile-format event stream.
//!
//! Usage: 3node-acceptor <socket-fd> <events.jsonl>
//!
//! Exit status 0 iff the exchange completed and verified.

use std::process::ExitCode;

use mojo_rs_casefile::events::serialize_events;
use mojo_rs_interop::ipcz::routing::RoutingAcceptor;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: 3node-acceptor <socket-fd> <events.jsonl>");
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
            eprintln!("3node acceptor init failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = acceptor.run_3node();
    let events = acceptor.events();
    if let Err(e) = write_events(events_path, events) {
        eprintln!("failed to write events: {e}");
        return ExitCode::FAILURE;
    }

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("3node acceptor run failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Serialize the events to the events file.
fn write_events(path: &str, events: &[mojo_rs_casefile::events::Event]) -> std::io::Result<()> {
    let out = serialize_events(events).map_err(std::io::Error::other)?;
    std::fs::write(path, out)
}
