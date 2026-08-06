//! 4node-acceptor — the native Rust counterpart of the oracle's
//! `invite-node-c-4node` mode: the *introduced* node C of the Phase 5
//! introduction court.
//!
//! C is referred by B (`INHERIT_BROKER`), receives the re-transferred portal
//! Y'' over the c2b pipe with `proxy_peer_node_name` = A, requests an
//! introduction from the broker (`RequestIntroduction`), adopts the introduced
//! C<->A link (`AcceptIntroduction`), completes the bypass with
//! `AcceptBypassLink` over the new link (the `EstablishLink` ->
//! `BypassPeerWithNewRemoteLink` path), completes the X<->Y'' round trip
//! ("hello"/"world"), and observes peer closure — writing a casefile-format
//! event stream.
//!
//! Usage: 4node-acceptor <socket-c-fd> <events.jsonl>
//!
//! Exit status 0 iff the exchange completed and verified.

use std::process::ExitCode;

use mojo_rs_casefile::events::serialize_events;
use mojo_rs_interop::ipcz::routing::RoutingAcceptor;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: 4node-acceptor <socket-c-fd> <events.jsonl>");
        return ExitCode::FAILURE;
    }
    let Ok(socket_fd) = args[1].parse::<i32>() else {
        eprintln!("invalid socket-c-fd: {}", args[1]);
        return ExitCode::FAILURE;
    };
    let events_path = &args[2];

    let mut acceptor = match RoutingAcceptor::new(socket_fd) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("4node acceptor init failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = acceptor.run_4node_c();
    let events = acceptor.events();
    if let Err(e) = write_events(events_path, events) {
        eprintln!("failed to write events: {e}");
        return ExitCode::FAILURE;
    }

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("4node acceptor run failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Serialize the events to the events file.
fn write_events(path: &str, events: &[mojo_rs_casefile::events::Event]) -> std::io::Result<()> {
    let out = serialize_events(events).map_err(std::io::Error::other)?;
    std::fs::write(path, out)
}
