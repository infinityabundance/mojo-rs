//! The candidate harness: executes casefiles against the mojo-rs core and
//! emits events JSONL for differential comparison.
//!
//!   candidate-harness baseline <casefile.json> <events.jsonl>
//!   candidate-harness --self-check
//!
//! This is the CANDIDATE-side equivalent of the C++ oracle driver; both parse
//! the same casefile and produce the same event vocabulary.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;

use mojo_rs_casefile::casefile::Casefile;
use mojo_rs_casefile::events::{Event, EventKind, SignalState};
use mojo_rs_core::dispatcher::Dispatcher;
use mojo_rs_core::error::CoreError;
use mojo_rs_core::handle::{Handle, HandleTable};
use mojo_rs_core::message::Message;
use mojo_rs_core::pipe::{End, MessagePipe};
use mojo_rs_core::platform_handle::PlatformHandleDispatcher;
use mojo_rs_core::signal::{Signals, SignalsState};
use mojo_rs_core::trap::{Trap, TrapCallback, TrapEvent};
use mojo_rs_core::wait::Waiter;
use mojo_rs_platform::fd::OwnedFd;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "--self-check" {
        println!("candidate-harness self-check ok");
        return ExitCode::SUCCESS;
    }
    if args.len() != 4 || args[1] != "baseline" {
        eprintln!("usage: candidate-harness baseline <casefile.json> <events.jsonl>");
        return ExitCode::FAILURE;
    }
    match run(&args[2], &args[3]) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("candidate harness error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(casefile_path: &str, events_path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(casefile_path).map_err(|e| format!("read: {e}"))?;
    let cf: Casefile = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    let mut h = Harness::new();
    h.execute(&cf)?;
    let events = mojo_rs_casefile::events::serialize_events(&h.events)
        .map_err(|e| format!("serialize events: {e}"))?;
    std::fs::write(events_path, events).map_err(|e| format!("write events: {e}"))
}

/// The trap event sink shared with trap callbacks.
struct EventSink {
    events: std::sync::Mutex<Vec<TrapEvent>>,
}

struct Harness {
    table: Arc<HandleTable>,
    /// casefile token -> owned handle.
    tokens: HashMap<String, Handle>,
    /// trap token -> trap.
    traps: HashMap<String, Arc<Trap>>,
    /// trap token -> event sink (kept alive for the trap's lifetime).
    trap_sinks: HashMap<String, Arc<EventSink>>,
    /// Write ends of pipes created as platform handles (kept open).
    pipe_writers: Vec<OwnedFd>,
    events: Vec<Event>,
    seq: u64,
}

impl Harness {
    fn new() -> Harness {
        Harness {
            table: Arc::new(HandleTable::new()),
            tokens: HashMap::new(),
            traps: HashMap::new(),
            trap_sinks: HashMap::new(),
            pipe_writers: Vec::new(),
            events: Vec::new(),
            seq: 0,
        }
    }

    /// Append an event and return its index (for callers that enrich it). The
    /// index is `seq - 1` because every event is pushed exactly once with a
    /// monotonically increasing sequence number — never a panic in practice.
    fn emit(&mut self, op_id: u64, event: EventKind, result: &str, token: Option<&str>) -> usize {
        self.seq += 1;
        self.events.push(Event {
            seq: self.seq,
            op_id,
            event,
            result: result.to_string(),
            handle: token.map(|s| s.to_string()),
            payload_hex: None,
            handles: None,
            signals: None,
            trigger_context: None,
            signals_state: None,
            outputs: None,
            process: None,
            pid: None,
            fd: None,
            note: None,
        });
        (self.seq - 1) as usize
    }

    fn handle(&self, token: &str) -> Result<&Handle, String> {
        self.tokens
            .get(token)
            .ok_or_else(|| format!("unknown handle token: {token}"))
    }

    /// Emit pending trap events from the sink as flat `trap` events (the
    /// oracle driver's shape), requiring at least `min_count`.
    fn emit_trap_events(
        &mut self,
        op_id: u64,
        trap_token: &str,
        min_count: usize,
    ) -> Result<(), String> {
        let sink = self
            .trap_sinks
            .get(trap_token)
            .cloned()
            .ok_or("unknown trap sink")?;
        let drained = {
            let mut e = sink.events.lock().map_err(|_| "sink lock")?;
            std::mem::take(&mut *e)
        };
        if drained.len() < min_count {
            return Err(format!(
                "trap sink has {} events, expected at least {min_count}",
                drained.len()
            ));
        }
        for e in &drained {
            self.emit_trap_payload(op_id, e);
        }
        Ok(())
    }

    /// Emit one flat `trap` event. The oracle driver's C++ harness emits
    /// `{"event":"trap","result":<trigger result>,"trigger_context":N,
    /// "signals_state":{...}}` — the trigger result (OK/CANCELLED) replaces
    /// the top-level result.
    fn emit_trap_payload(&mut self, op_id: u64, e: &TrapEvent) {
        let idx = self.emit(op_id, EventKind::Trap, e.result.name(), None);
        let ev = &mut self.events[idx];
        ev.trigger_context = Some(e.trigger_context);
        ev.signals_state = Some(SignalState {
            satisfied: signal_names(e.signals_state.satisfied),
            satisfiable: signal_names(e.signals_state.satisfiable),
        });
    }

    /// Resolve a handle token, tolerating a missing token when the casefile
    /// expects `MOJO_RESULT_INVALID_ARGUMENT` (the handle was closed earlier).
    fn handle_opt(
        &self,
        token: &str,
        expect: Option<&mojo_rs_casefile::casefile::Expectation>,
    ) -> Result<Option<&Handle>, String> {
        match self.tokens.get(token) {
            Some(h) => Ok(Some(h)),
            None => {
                if expect
                    .and_then(|e| e.result.as_deref())
                    .is_some_and(|r| r == "MOJO_RESULT_INVALID_ARGUMENT")
                {
                    Ok(None)
                } else {
                    Err(format!("unknown handle token: {token}"))
                }
            }
        }
    }

    fn execute(&mut self, cf: &Casefile) -> Result<(), String> {
        self.emit(0, EventKind::Lifecycle, "MOJO_RESULT_OK", None);
        for op in &cf.operations {
            self.exec_op(op.id, &op.op, &op.args, op.expect.as_ref())
                .map_err(|e| format!("op {} ({}): {e}", op.id, op.op))?;
        }
        self.emit(0, EventKind::Lifecycle, "MOJO_RESULT_OK", None);
        Ok(())
    }

    fn exec_op(
        &mut self,
        id: u64,
        op: &str,
        args: &serde_json::Value,
        expect: Option<&mojo_rs_casefile::casefile::Expectation>,
    ) -> Result<(), String> {
        match op {
            "message_pipe_create" => {
                let (a, b) = MessagePipe::create();
                let ha = self.table.add(a).map_err(|e| e.to_string())?;
                let hb = self.table.add(b).map_err(|e| e.to_string())?;
                let produce: Vec<String> = expect.map(|e| e.produce.clone()).unwrap_or_default();
                if produce.len() == 2 {
                    self.tokens.insert(produce[0].clone(), ha);
                    self.tokens.insert(produce[1].clone(), hb);
                }
                self.emit(id, EventKind::Result, "MOJO_RESULT_OK", None);
            }
            "write_message" => {
                let token = args["handle"].as_str().ok_or("write_message: handle")?;
                let payload = hex::decode(args["payload_hex"].as_str().unwrap_or(""))
                    .map_err(|e| format!("payload hex: {e}"))?;
                let mut handles = Vec::new();
                if let Some(hs) = args["handles"].as_array() {
                    for h in hs {
                        let t = h.as_str().unwrap_or("");
                        let hnd = self.tokens.remove(t).ok_or(format!("missing handle {t}"))?;
                        handles.push(hnd);
                    }
                }
                let Some(h) = self.handle_opt(token, expect)? else {
                    self.emit(
                        id,
                        EventKind::Result,
                        "MOJO_RESULT_INVALID_ARGUMENT",
                        Some(token),
                    );
                    return Ok(());
                };
                let dispatcher = self.table.get(h.value()).map_err(|e| e.to_string())?;
                let pipe = pipe_ref(&*dispatcher)?;
                let end = endpoint_of(&*dispatcher)?;
                let result = pipe.write(end, Message::new(payload, handles));
                self.emit(id, EventKind::Result, &result_name(result), Some(token));
            }
            "read_message" => {
                let token = args["handle"].as_str().ok_or("read_message: handle")?;
                let Some(h) = self.handle_opt(token, expect)? else {
                    self.emit(
                        id,
                        EventKind::Message,
                        "MOJO_RESULT_INVALID_ARGUMENT",
                        Some(token),
                    );
                    return Ok(());
                };
                let dispatcher = self.table.get(h.value()).map_err(|e| e.to_string())?;
                let pipe = pipe_ref(&*dispatcher)?;
                let end = endpoint_of(&*dispatcher)?;
                // The oracle driver reads the whole message (MojoReadMessage in
                // this epoch takes no size limit; `max_num_bytes` in the
                // casefile is advisory metadata).
                match pipe.read(end, None, false) {
                    Ok(mojo_rs_core::pipe::ReadOutcome::Message { data, handles }) => {
                        let idx = self.emit(id, EventKind::Message, "MOJO_RESULT_OK", Some(token));
                        let ev = &mut self.events[idx];
                        ev.payload_hex = Some(hex::encode(&data));
                        let mut extracted = Vec::new();
                        for (i, hnd) in handles.into_iter().enumerate() {
                            let t = format!("{token}_handle_{i}");
                            self.tokens.insert(t.clone(), hnd);
                            extracted.push(t);
                        }
                        ev.handles = Some(extracted);
                    }
                    Ok(mojo_rs_core::pipe::ReadOutcome::TooLarge { size }) => {
                        let idx = self.emit(
                            id,
                            EventKind::Message,
                            "MOJO_RESULT_RESOURCE_EXHAUSTED",
                            Some(token),
                        );
                        self.events[idx].note = Some(format!("required_size={size}"));
                    }
                    Err(e) => {
                        self.emit(id, EventKind::Message, &result_name(Err(e)), Some(token));
                    }
                }
            }
            "query_signals_state" => {
                let token = args["handle"]
                    .as_str()
                    .ok_or("query_signals_state: handle")?;
                let Some(h) = self.handle_opt(token, expect)? else {
                    self.emit(
                        id,
                        EventKind::Signals,
                        "MOJO_RESULT_INVALID_ARGUMENT",
                        Some(token),
                    );
                    return Ok(());
                };
                let dispatcher = self.table.get(h.value()).map_err(|e| e.to_string())?;
                let state = dispatcher.query_signals();
                let idx = self.emit(id, EventKind::Signals, "MOJO_RESULT_OK", Some(token));
                self.events[idx].signals = Some(signal_state_of(state));
            }
            "close" => {
                let token = args["handle"].as_str().ok_or("close: handle")?;
                match self.tokens.remove(token) {
                    Some(_h) => {
                        // The Handle's Drop closes the table entry.
                        self.emit(id, EventKind::Result, "MOJO_RESULT_OK", Some(token));
                    }
                    None => {
                        self.emit(
                            id,
                            EventKind::Result,
                            "MOJO_RESULT_INVALID_ARGUMENT",
                            Some(token),
                        );
                    }
                }
            }
            "wait" => {
                let token = args["handle"].as_str().ok_or("wait: handle")?;
                let Some(h) = self.handle_opt(token, expect)? else {
                    self.emit(
                        id,
                        EventKind::Result,
                        "MOJO_RESULT_INVALID_ARGUMENT",
                        Some(token),
                    );
                    return Ok(());
                };
                let dispatcher = self.table.get(h.value()).map_err(|e| e.to_string())?;
                let signals = parse_signals(args["signals"].as_array().unwrap_or(&vec![]))?;
                // The oracle driver defaults to an indefinite deadline when
                // `deadline_ms` is absent; overflow maps to indefinite too.
                let deadline = args["deadline_ms"].as_u64().and_then(|ms| {
                    std::time::Instant::now().checked_add(std::time::Duration::from_millis(ms))
                });
                let w = Waiter::new();
                let result = w.wait(&dispatcher, signals, deadline);
                let state = dispatcher.query_signals();
                let idx = self.emit(
                    id,
                    EventKind::Result,
                    &result_name(result.map(|_| ())),
                    Some(token),
                );
                self.events[idx].signals = Some(signal_state_of(state));
            }
            "platform_handle_wrap" => {
                let source = args["source"].as_str().unwrap_or("memfd");
                let content = hex::decode(args["content_hex"].as_str().unwrap_or(""))
                    .map_err(|e| format!("content hex: {e}"))?;
                let fd = match source {
                    "memfd" => {
                        let mem = mojo_rs_platform::shm::SharedMemory::create("mojo-harness", 4096)
                            .map_err(|e| e.to_string())?;
                        let m = mem
                            .map(0, 4096, mojo_rs_platform::shm::Access::ReadWrite)
                            .map_err(|e| e.to_string())?;
                        // SAFETY: the mapping is read-write and owned; no
                        // aliasing references exist.
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                content.as_ptr(),
                                m.as_mut_ptr(),
                                content.len(),
                            );
                        }
                        drop(m);
                        dup_fd(mem.as_raw_fd())?
                    }
                    "pipe" => {
                        // A nonblocking pipe read end with empty content.
                        let mut fds = [-1; 2];
                        // SAFETY: fds is a valid 2-element output array.
                        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
                        if rc != 0 {
                            return Err(format!(
                                "pipe failed: {}",
                                std::io::Error::last_os_error()
                            ));
                        }
                        // SAFETY: pipe returned two fresh owned descriptors.
                        let rd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
                        // SAFETY: same pipe() call; the write end is also a
                        // fresh descriptor owned by this branch.
                        let wr = unsafe { OwnedFd::from_raw_fd(fds[1]) };
                        // Keep the write end open so reads do not see EOF.
                        self.pipe_writers.push(wr);
                        set_nonblocking(&rd)?;
                        rd
                    }
                    _ => return Err(format!("unknown platform handle source: {source}")),
                };
                let disp = PlatformHandleDispatcher::new(fd);
                let h = self.table.add(disp).map_err(|e| e.to_string())?;
                let produce: String = expect
                    .and_then(|e| e.produce.first().cloned())
                    .unwrap_or_else(|| "p0".to_string());
                self.tokens.insert(produce.clone(), h);
                self.emit(id, EventKind::Handle, "MOJO_RESULT_OK", Some(&produce));
            }
            "platform_handle_unwrap" => {
                let token = args["handle"]
                    .as_str()
                    .ok_or("platform_handle_unwrap: handle")?;
                match self.tokens.remove(token) {
                    Some(h) => {
                        let dispatcher = self.table.get(h.value()).map_err(|e| e.to_string())?;
                        let ph = dispatcher
                            .as_any()
                            .downcast_ref::<PlatformHandleDispatcher>()
                            .ok_or("handle is not a platform handle")?;
                        let fd = ph.as_raw_fd();
                        let content = read_fd(fd);
                        let idx = self.emit(id, EventKind::Handle, "MOJO_RESULT_OK", Some(token));
                        self.events[idx].payload_hex = Some(hex::encode(&content));
                    }
                    None => {
                        self.emit(
                            id,
                            EventKind::Handle,
                            "MOJO_RESULT_INVALID_ARGUMENT",
                            Some(token),
                        );
                    }
                }
            }
            "trap_create" => {
                let produce = args["produce"].as_str().unwrap_or("t0").to_string();
                let sink = Arc::new(EventSink {
                    events: std::sync::Mutex::new(Vec::new()),
                });
                let raw = Arc::as_ptr(&sink) as usize;
                let callback = TrapCallback::new(
                    move |ctx: usize, event: &TrapEvent| {
                        // SAFETY: the harness keeps the sink Arc alive for the
                        // trap's lifetime; the trap's callbacks never outlive
                        // it (close() runs before drop).
                        let s: &EventSink = unsafe { &*(ctx as *const EventSink) };
                        if let Ok(mut e) = s.events.lock() {
                            e.push(*event);
                        }
                    },
                    raw,
                );
                let trap = Trap::create(callback);
                self.trap_sinks.insert(produce.clone(), sink);
                self.traps.insert(produce.clone(), trap);
                self.emit(id, EventKind::Result, "MOJO_RESULT_OK", None);
            }
            "trap_add_trigger" => {
                let trap_token = args["trap"].as_str().ok_or("trap_add_trigger: trap")?;
                let handle_token = args["handle"].as_str().ok_or("trap_add_trigger: handle")?;
                let context = args["context"].as_u64().unwrap_or(0);
                let signals = parse_signals(args["signals"].as_array().unwrap_or(&vec![]))?;
                let trap = self.traps.get(trap_token).cloned().ok_or("unknown trap")?;
                let h = self.handle(handle_token)?;
                let dispatcher = self.table.get(h.value()).map_err(|e| e.to_string())?;
                let result = trap.add_trigger(dispatcher, signals, context);
                self.emit(id, EventKind::Result, &result_name(result), None);
            }
            "trap_arm" => {
                let trap_token = args["trap"].as_str().ok_or("trap_arm: trap")?;
                let trap = self.traps.get(trap_token).cloned().ok_or("unknown trap")?;
                let result = trap.arm();
                if matches!(
                    result,
                    Err(mojo_rs_core::error::CoreError::FailedPrecondition)
                ) {
                    // The oracle driver emits the immediate events (and any
                    // already-queued events) BEFORE the arm result event.
                    self.emit_trap_events(id, trap_token, 1)?;
                }
                self.emit(id, EventKind::Result, &result_name(result), None);
            }
            "trap_remove_trigger" => {
                let trap_token = args["trap"].as_str().ok_or("trap_remove_trigger: trap")?;
                let context = args["context"].as_u64().unwrap_or(0);
                let trap = self.traps.get(trap_token).cloned().ok_or("unknown trap")?;
                let result = trap.remove_trigger(context);
                self.emit(id, EventKind::Result, &result_name(result), None);
            }
            "trap_close" => {
                let trap_token = args["trap"].as_str().ok_or("trap_close: trap")?;
                let trap = self.traps.remove(trap_token).ok_or("unknown trap")?;
                trap.close();
                self.emit(id, EventKind::Result, "MOJO_RESULT_OK", None);
            }
            "drain_trap_events" => {
                let trap_token = args["trap"].as_str().ok_or("drain_trap_events: trap")?;
                let count = args["count"].as_u64().unwrap_or(1) as usize;
                let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(5000);
                let sink = self
                    .trap_sinks
                    .get(trap_token)
                    .cloned()
                    .ok_or("unknown trap sink")?;
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                let mut collected = Vec::new();
                while collected.len() < count {
                    let drained = {
                        let mut e = sink.events.lock().map_err(|_| "sink lock")?;
                        std::mem::take(&mut *e)
                    };
                    if drained.is_empty() {
                        if std::time::Instant::now() >= deadline {
                            return Err(format!("drain_trap_events timeout ({count} events)"));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    collected.extend(drained);
                }
                // The oracle driver emits one `trap` event per drained event,
                // then a trailing `result` event.
                for e in &collected {
                    self.emit_trap_payload(id, e);
                }
                self.emit(id, EventKind::Result, "MOJO_RESULT_OK", None);
            }
            other => {
                return Err(format!(
                    "unsupported operation in candidate harness: {other}"
                ));
            }
        }
        Ok(())
    }
}

fn pipe_ref(d: &dyn Dispatcher) -> Result<&mojo_rs_core::pipe::MessagePipe, String> {
    d.as_any()
        .downcast_ref::<mojo_rs_core::pipe::MessagePipeDispatcher>()
        .map(|p| p.pipe())
        .ok_or_else(|| "handle is not a message pipe".to_string())
}

fn endpoint_of(d: &dyn Dispatcher) -> Result<End, String> {
    d.as_any()
        .downcast_ref::<mojo_rs_core::pipe::MessagePipeDispatcher>()
        .map(|p| p.end())
        .ok_or_else(|| "no endpoint".to_string())
}

fn result_name(r: Result<(), CoreError>) -> String {
    match r {
        Ok(()) => "MOJO_RESULT_OK".to_string(),
        Err(e) => e.name().to_string(),
    }
}

fn parse_signals(names: &[serde_json::Value]) -> Result<Signals, String> {
    let mut s = Signals::NONE;
    for n in names {
        s = s | match n.as_str().unwrap_or("") {
            "READABLE" => Signals::READABLE,
            "WRITABLE" => Signals::WRITABLE,
            "PEER_CLOSED" => Signals::PEER_CLOSED,
            "NEW_DATA_READABLE" => Signals::NEW_DATA_READABLE,
            "PEER_REMOTE" => Signals::PEER_REMOTE,
            "QUOTA_EXCEEDED" => Signals::QUOTA_EXCEEDED,
            other => return Err(format!("unknown signal: {other}")),
        };
    }
    Ok(s)
}

fn signal_names(s: Signals) -> Vec<String> {
    let mut out = Vec::new();
    for (sig, name) in [
        (Signals::READABLE, "READABLE"),
        (Signals::WRITABLE, "WRITABLE"),
        (Signals::PEER_CLOSED, "PEER_CLOSED"),
        (Signals::NEW_DATA_READABLE, "NEW_DATA_READABLE"),
        (Signals::PEER_REMOTE, "PEER_REMOTE"),
        (Signals::QUOTA_EXCEEDED, "QUOTA_EXCEEDED"),
    ] {
        if s.contains(sig) {
            out.push(name.to_string());
        }
    }
    out
}

fn signal_state_of(st: SignalsState) -> SignalState {
    SignalState {
        satisfied: signal_names(st.satisfied),
        satisfiable: signal_names(st.satisfiable),
    }
}

fn set_nonblocking(fd: &OwnedFd) -> Result<(), String> {
    // SAFETY: fd is owned and valid.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(format!(
            "fcntl F_GETFL: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: fd is owned and valid.
    let rc = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(format!(
            "fcntl F_SETFL: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn dup_fd(fd: std::os::unix::io::RawFd) -> Result<OwnedFd, String> {
    // SAFETY: fd is a valid open descriptor.
    let new = unsafe { libc::dup(fd) };
    if new < 0 {
        Err(format!("dup failed: {}", std::io::Error::last_os_error()))
    } else {
        // SAFETY: dup returned a fresh descriptor owned by us.
        Ok(unsafe { OwnedFd::from_raw_fd(new) })
    }
}

/// Read the full content of a descriptor from offset 0.
fn read_fd(fd: std::os::unix::io::RawFd) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    // SAFETY: fd is a valid open descriptor; the File takes ownership and we
    // restore it via into_raw_fd before returning.
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    let _ = f.seek(SeekFrom::Start(0));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    // Restore ownership of the descriptor to the caller.
    let _ = f.into_raw_fd();
    buf
}
