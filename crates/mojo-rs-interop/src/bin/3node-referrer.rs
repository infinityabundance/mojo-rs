//! 3node-referrer — the native Rust counterpart of the oracle's
//! `invite-node-a-3node` mode: the *referrer* node A of the Phase 5
//! multi-node referral court.
//!
//! A accepts invitation-1 from the broker (the standard Connect handshake),
//! refers B with `ReferNonBroker` (SHARE_BROKER) over the referral transport,
//! adopts the A<->B link on `NonBrokerReferralAccepted`, receives the
//! broker's transfer-y over pipe_a, re-transfers Y' through the a2b pipe (the
//! proxy serialization with `proxy_peer_node_name` = the broker), sends
//! `done`, and observes peer closure — writing a casefile-format event
//! stream.
//!
//! Usage: 3node-referrer <socket-broker-fd> <socket-b-fd> <events.jsonl>
//!
//! Exit status 0 iff the exchange completed and verified.

use std::process::ExitCode;

use mojo_rs_casefile::events::serialize_events;
use mojo_rs_interop::ipcz::routing::RoutingAcceptor;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: 3node-referrer <socket-broker-fd> <socket-b-fd> <events.jsonl>");
        return ExitCode::FAILURE;
    }
    let Ok(broker_fd) = args[1].parse::<i32>() else {
        eprintln!("invalid socket-broker-fd: {}", args[1]);
        return ExitCode::FAILURE;
    };
    let Ok(referral_fd) = args[2].parse::<i32>() else {
        eprintln!("invalid socket-b-fd: {}", args[2]);
        return ExitCode::FAILURE;
    };
    let events_path = &args[3];

    let mut acceptor = match RoutingAcceptor::new_referrer(broker_fd, referral_fd) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("3node referrer init failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = acceptor.run_3node_a();
    let events = acceptor.events();
    if let Err(e) = write_events(events_path, events) {
        eprintln!("failed to write events: {e}");
        return ExitCode::FAILURE;
    }

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("3node referrer run failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Serialize the events to the events file.
fn write_events(path: &str, events: &[mojo_rs_casefile::events::Event]) -> std::io::Result<()> {
    let out = serialize_events(events).map_err(std::io::Error::other)?;
    std::fs::write(path, out)
}
