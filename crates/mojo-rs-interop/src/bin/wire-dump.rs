//! Throwaway forensic decoder for routing court wire captures.
//! Usage: cargo run -p mojo-rs-interop --bin wire-dump <file.bin>
use std::process::ExitCode;

use mojo_rs_interop::ipcz::messages::{
    self, DecodedMessage, MSG_ID_ACCEPT_BYPASS_LINK, MSG_ID_ACCEPT_PARCEL,
    MSG_ID_ACCEPT_PARCEL_DRIVER_OBJECTS, MSG_ID_ADD_BLOCK_BUFFER, MSG_ID_BYPASS_PEER,
    MSG_ID_CONNECT_FROM_BROKER_TO_NON_BROKER, MSG_ID_CONNECT_FROM_NON_BROKER_TO_BROKER,
    MSG_ID_FLUSH_ROUTER, MSG_ID_PROVIDE_MEMORY, MSG_ID_PROXY_WILL_STOP, MSG_ID_REQUEST_MEMORY,
    MSG_ID_ROUTE_CLOSED, MSG_ID_ROUTE_DISCONNECTED, MSG_ID_STOP_PROXYING,
    MSG_ID_STOP_PROXYING_TO_LOCAL_PEER, decode_message,
};
use mojo_rs_interop::ipcz::wire::{parse_message_header, parse_stream};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: wire-dump <file.bin>");
        return ExitCode::FAILURE;
    }
    let data = match std::fs::read(&args[1]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("read failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let msgs = match parse_stream(&data) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("parse failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    for (i, m) in msgs.iter().enumerate() {
        let hdr = match parse_message_header(&m.payload) {
            Ok(h) => h,
            Err(e) => {
                println!("[{i}] header error: {e}");
                continue;
            }
        };
        let id = hdr.message_id;
        let seq = hdr.node_sequence_number;
        let name = match id {
            MSG_ID_CONNECT_FROM_BROKER_TO_NON_BROKER => "Connect",
            MSG_ID_CONNECT_FROM_NON_BROKER_TO_BROKER => "ConnectReply",
            MSG_ID_ADD_BLOCK_BUFFER => "AddBlockBuffer",
            MSG_ID_ACCEPT_PARCEL => "AcceptParcel",
            MSG_ID_ACCEPT_PARCEL_DRIVER_OBJECTS => "AcceptParcelDriverObjects",
            MSG_ID_ROUTE_CLOSED => "RouteClosed",
            MSG_ID_ROUTE_DISCONNECTED => "RouteDisconnected",
            MSG_ID_BYPASS_PEER => "BypassPeer",
            MSG_ID_ACCEPT_BYPASS_LINK => "AcceptBypassLink",
            MSG_ID_PROXY_WILL_STOP => "ProxyWillStop",
            MSG_ID_STOP_PROXYING => "StopProxying",
            MSG_ID_STOP_PROXYING_TO_LOCAL_PEER => "StopProxyingToLocalPeer",
            MSG_ID_FLUSH_ROUTER => "FlushRouter",
            MSG_ID_REQUEST_MEMORY => "RequestMemory",
            MSG_ID_PROVIDE_MEMORY => "ProvideMemory",
            _ => "?",
        };
        let detail = match decode_message(&m.payload, m.num_handles as usize) {
            Ok(d) => describe(&d),
            Err(e) => format!("decode error: {e}"),
        };
        println!(
            "[{i}] seq={seq} id={id} ({name}) handles={} {detail}",
            m.num_handles
        );
    }
    ExitCode::SUCCESS
}

fn nn(n: &messages::NodeName) -> String {
    if n.is_valid() {
        format!("{:016x}{:016x}", n.high, n.low)
    } else {
        "invalid".to_string()
    }
}

fn dump_descriptors(bytes: &[u8]) -> String {
    use mojo_rs_interop::ipcz::messages::RouterDescriptor;
    if bytes.is_empty() {
        return "none".to_string();
    }
    let mut out = Vec::new();
    for chunk in bytes.chunks(RouterDescriptor::SIZE) {
        if chunk.len() != RouterDescriptor::SIZE {
            out.push("<truncated>".to_string());
            continue;
        }
        match RouterDescriptor::decode(chunk) {
            Ok(d) => out.push(format!(
                "{{new={} decaying={} next_o={} next_i={} din={} peer_closed={} pab={} proxy_node={} proxy_sub={}}}",
                d.new_sublink,
                d.new_decaying_sublink,
                d.next_outgoing_sequence_number,
                d.next_incoming_sequence_number,
                d.decaying_incoming_sequence_length,
                d.peer_closed,
                d.proxy_already_bypassed,
                nn(&d.proxy_peer_node_name),
                d.proxy_peer_sublink
            )),
            Err(e) => out.push(format!("<decode err {e}>")),
        }
    }
    out.join(", ")
}

fn describe(d: &DecodedMessage) -> String {
    match d {
        DecodedMessage::ConnectFromBrokerToNonBroker(c) => format!(
            "broker={} receiver={} protocol={} num_initial_portals={} buffer_index={}",
            nn(&c.broker_name),
            nn(&c.receiver_name),
            c.protocol_version,
            c.num_initial_portals,
            c.buffer_index
        ),
        DecodedMessage::ConnectFromNonBrokerToBroker(c) => format!(
            "protocol={} num_initial_portals={}",
            c.protocol_version, c.num_initial_portals
        ),
        DecodedMessage::AddBlockBuffer(b) => format!(
            "buffer_id={} buffer_index={} block_size={}",
            b.buffer_id, b.buffer_index, b.block_size
        ),
        DecodedMessage::AcceptParcel(p) => format!(
            "sub={} rseq={} subparcel={}/{} frag={:?} data={:?} handle_types={:?} new_routers=[{}] driver_objects={:?}",
            p.sublink,
            p.sequence_number,
            p.subparcel_index,
            p.num_subparcels,
            p.parcel_fragment,
            String::from_utf8_lossy(&p.parcel_data),
            p.handle_types,
            dump_descriptors(&p.new_routers),
            p.driver_objects
        ),
        DecodedMessage::AcceptParcelDriverObjects(_) => "driver objects".into(),
        DecodedMessage::BypassPeerWithLink(b) => format!(
            "sub={} new_sublink={} frag={:?} inbound_len={}",
            b.sublink, b.new_sublink, b.new_link_state_fragment, b.inbound_sequence_length
        ),
        DecodedMessage::RouteClosed(r) => format!("sub={} len={}", r.sublink, r.sequence_length),
        DecodedMessage::RouteDisconnected(r) => format!("sub={}", r.sublink),
        DecodedMessage::BypassPeer(b) => format!(
            "sub={} target_node={} target_sublink={}",
            b.sublink,
            nn(&b.bypass_target_node),
            b.bypass_target_sublink
        ),
        DecodedMessage::AcceptBypassLink(a) => format!(
            "current_peer_node={} current_peer_sublink={} inbound_len={} new_sublink={} frag={:?}",
            nn(&a.current_peer_node),
            a.current_peer_sublink,
            a.inbound_sequence_length_from_bypassed_link,
            a.new_sublink,
            a.new_link_state_fragment
        ),
        DecodedMessage::ProxyWillStop(w) => {
            format!(
                "sub={} inbound_len={}",
                w.sublink, w.inbound_sequence_length
            )
        }
        DecodedMessage::StopProxying(s) => format!(
            "sub={} in_len={} out_len={}",
            s.sublink, s.inbound_sequence_length, s.outbound_sequence_length
        ),
        DecodedMessage::StopProxyingToLocalPeer(s) => {
            format!("sub={} out_len={}", s.sublink, s.outbound_sequence_length)
        }
        DecodedMessage::FlushRouter(f) => format!("sub={}", f.sublink),
        DecodedMessage::RequestMemory(r) | DecodedMessage::ProvideMemory(r) => {
            format!("size={}", r.size)
        }
        DecodedMessage::Unknown(id) => format!("unknown id={id}"),
    }
}
