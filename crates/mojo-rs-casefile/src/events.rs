//! Harness events: the JSONL event stream both sides produce. Schema:
//! `casefiles/schema/events.schema.json`.

use serde::{Deserialize, Serialize};

/// One event line emitted by a harness.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Monotonic sequence number (per process).
    pub seq: u64,
    /// The operation id that produced this event (0 = lifecycle).
    pub op_id: u64,
    /// Event kind.
    pub event: EventKind,
    /// The `MOJO_RESULT_*` name.
    pub result: String,
    /// Handle token involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Payload bytes (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hex: Option<String>,
    /// Handle tokens attached/extracted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handles: Option<Vec<String>>,
    /// Signal state (for `signals` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<SignalState>,
    /// Trap trigger context (for `trap` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_context: Option<u64>,
    /// Signal state at a trap event (key matches the oracle driver: the C++
    /// harness emits `signals_state` for trap events and `signals` for
    /// `query_signals_state` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals_state: Option<SignalState>,
    /// Extra outputs (produced handle tokens etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<serde_json::Map<String, serde_json::Value>>,
    /// The process that emitted this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Process id (normalized by the pid normalizer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Descriptor number (normalized by the fd normalizer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fd: Option<i32>,
    /// Note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Event kinds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// An operation result.
    Result,
    /// A signal-state query result.
    Signals,
    /// A message was read/written.
    Message,
    /// A handle operation.
    Handle,
    /// A trap event.
    Trap,
    /// A data-pipe operation.
    Data,
    /// A shared-buffer operation.
    Buffer,
    /// An invitation operation.
    Invitation,
    /// A process lifecycle event.
    Process,
    /// Informational.
    Info,
    /// An error.
    Error,
    /// Harness lifecycle (init/shutdown).
    Lifecycle,
}

/// A signal state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignalState {
    /// Satisfied signal names.
    pub satisfied: Vec<String>,
    /// Satisfiable signal names.
    pub satisfiable: Vec<String>,
}

impl Event {
    /// Create a result event.
    pub fn result(seq: u64, op_id: u64, result: impl Into<String>) -> Event {
        Event {
            seq,
            op_id,
            event: EventKind::Result,
            result: result.into(),
            handle: None,
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
        }
    }
}

/// Parse a JSONL event stream.
pub fn parse_events(input: &str) -> Result<Vec<Event>, serde_json::Error> {
    input
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// Serialize events as JSONL.
///
/// Keys are emitted in sorted order to match the oracle driver, which
/// serializes through base::JSONWriter (std::map-backed, lexicographically
/// sorted keys). Byte-identical raw event streams make the evidence trail
/// stronger. serde_json's `Map` is a `BTreeMap` when `preserve_order` is
/// disabled (the workspace default), so round-tripping through `Value` sorts
/// keys exactly as the oracle does.
pub fn serialize_events(events: &[Event]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for e in events {
        let v = serde_json::to_value(e)?;
        out.push_str(&serde_json::to_string(&v)?);
        out.push('\n');
    }
    Ok(out)
}
