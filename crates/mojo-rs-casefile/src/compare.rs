//! Differential comparison: oracle events vs candidate events with the
//! declared normalizers. Schema: `casefiles/schema/comparison.schema.json`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::events::{Event, parse_events};
use crate::normalizers::apply_normalizers;

/// A comparison residual (a mismatch).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Residual {
    /// The event sequence position (0-based in the normalized stream).
    pub seq: usize,
    /// Residual kind.
    pub kind: ResidualKind,
    /// The oracle event (normalized).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle: Option<Event>,
    /// The candidate event (normalized).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<Event>,
    /// Explanation.
    pub note: String,
}

/// Residual classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidualKind {
    /// An oracle event has no candidate counterpart.
    Missing,
    /// A candidate event has no oracle counterpart.
    Extra,
    /// Both exist but differ.
    Mismatch,
    /// Events are out of order relative to the oracle.
    Order,
}

/// The comparison result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    /// Schema version.
    pub schema_version: u32,
    /// The case id.
    pub case_id: String,
    /// `pass`, `fail`, or `error`.
    pub status: String,
    /// Number of oracle events (normalized).
    pub oracle_events: usize,
    /// Number of candidate events (normalized).
    pub candidate_events: usize,
    /// Normalizers applied.
    pub normalizers_applied: Vec<String>,
    /// Residuals (empty on pass).
    pub residuals: Vec<Residual>,
    /// Evidence hashes.
    pub evidence: EvidenceRef,
}

/// Evidence references for a comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// sha256 of the normalized oracle event stream.
    pub oracle_events_sha256: String,
    /// sha256 of the normalized candidate event stream.
    pub candidate_events_sha256: String,
}

/// Compare two JSONL event streams for a casefile.
pub fn compare(
    case_id: &str,
    oracle_input: &str,
    candidate_input: &str,
    normalizer_ids: &[String],
) -> Result<Comparison, String> {
    let oracle_raw = parse_events(oracle_input).map_err(|e| format!("oracle events: {e}"))?;
    let candidate_raw =
        parse_events(candidate_input).map_err(|e| format!("candidate events: {e}"))?;

    let oracle = apply_normalizers(oracle_raw, normalizer_ids)?;
    let candidate = apply_normalizers(candidate_raw, normalizer_ids)?;

    let oracle_events_sha256 = sha256_events(&oracle).map_err(|e| e.to_string())?;
    let candidate_events_sha256 = sha256_events(&candidate).map_err(|e| e.to_string())?;

    let mut residuals = Vec::new();

    // Structural comparison: sequence-based equality on the canonical fields.
    let n = oracle.len().max(candidate.len());
    for i in 0..n {
        let o = oracle.get(i);
        let c = candidate.get(i);
        match (o, c) {
            (Some(o), Some(c)) => {
                if !event_eq(o, c) {
                    residuals.push(Residual {
                        seq: i,
                        kind: ResidualKind::Mismatch,
                        oracle: Some(o.clone()),
                        candidate: Some(c.clone()),
                        note: format!("event {i} differs"),
                    });
                }
            }
            (Some(o), None) => residuals.push(Residual {
                seq: i,
                kind: ResidualKind::Missing,
                oracle: Some(o.clone()),
                candidate: None,
                note: format!("oracle event {i} has no candidate counterpart"),
            }),
            (None, Some(c)) => residuals.push(Residual {
                seq: i,
                kind: ResidualKind::Extra,
                oracle: None,
                candidate: Some(c.clone()),
                note: format!("candidate event {i} has no oracle counterpart"),
            }),
            (None, None) => {}
        }
    }

    let status = if residuals.is_empty() { "pass" } else { "fail" };

    Ok(Comparison {
        schema_version: 1,
        case_id: case_id.to_string(),
        status: status.to_string(),
        oracle_events: oracle.len(),
        candidate_events: candidate.len(),
        normalizers_applied: normalizer_ids.to_vec(),
        residuals,
        evidence: EvidenceRef {
            oracle_events_sha256,
            candidate_events_sha256,
        },
    })
}

/// Canonical event equality: compares the observable fields only (not
/// timing/implementation fields).
fn event_eq(a: &Event, b: &Event) -> bool {
    a.event == b.event
        && a.result == b.result
        && a.handle == b.handle
        && a.payload_hex == b.payload_hex
        && a.handles == b.handles
        && a.signals == b.signals
        && a.trigger_context == b.trigger_context
        && a.signals_state == b.signals_state
        && a.outputs == b.outputs
        && a.process == b.process
        && a.fd == b.fd
        && a.note == b.note
}

fn sha256_events(events: &[Event]) -> Result<String, serde_json::Error> {
    use std::fmt::Write;
    let mut hasher = Sha256::new();
    for e in events {
        let line = serde_json::to_string(e)?;
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    let out = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in out {
        // fmt::Write on a String cannot fail; map to a serialization error to
        // keep the Result type without panicking.
        write!(hex, "{b:02x}")
            .map_err(|_| serde_json::Error::io(std::io::Error::other("hex write")))?;
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(seq: u64, op: u64, result: &str) -> String {
        serde_json::json!({
            "seq": seq, "op_id": op, "event": "result", "result": result
        })
        .to_string()
    }

    #[test]
    fn identical_streams_pass() {
        let oracle = format!(
            "{}\n{}\n",
            line(1, 1, "MOJO_RESULT_OK"),
            line(2, 2, "MOJO_RESULT_OK")
        );
        let candidate = oracle.clone();
        let cmp = compare("CASE.TEST.001", &oracle, &candidate, &[]).unwrap();
        assert_eq!(cmp.status, "pass");
        assert!(cmp.residuals.is_empty());
    }

    #[test]
    fn mismatch_detected() {
        let oracle = format!("{}\n", line(1, 1, "MOJO_RESULT_OK"));
        let candidate = format!("{}\n", line(1, 1, "MOJO_RESULT_FAILED_PRECONDITION"));
        let cmp = compare("CASE.TEST.002", &oracle, &candidate, &[]).unwrap();
        assert_eq!(cmp.status, "fail");
        assert_eq!(cmp.residuals.len(), 1);
        assert_eq!(cmp.residuals[0].kind, ResidualKind::Mismatch);
    }

    #[test]
    fn missing_event_detected() {
        let oracle = format!(
            "{}\n{}\n",
            line(1, 1, "MOJO_RESULT_OK"),
            line(2, 2, "MOJO_RESULT_OK")
        );
        let candidate = format!("{}\n", line(1, 1, "MOJO_RESULT_OK"));
        let cmp = compare("CASE.TEST.003", &oracle, &candidate, &[]).unwrap();
        assert_eq!(cmp.status, "fail");
        assert_eq!(cmp.residuals[0].kind, ResidualKind::Missing);
    }
}
