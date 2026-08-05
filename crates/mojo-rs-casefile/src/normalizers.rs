//! Normalizer registry: narrow, documented, tested normalization rules.
//!
//! Normalization is permitted ONLY for fields the public contract leaves
//! nondeterministic (pids, fd numbers, tokens, timestamps, and
//! scheduler-dependent ordering when the casefile declares it). Every rule
//! here has a corresponding curated case in `casefiles/curated/`.

use serde_json::Value;

use crate::events::Event;

/// Apply the normalizers named in `ids` to an event stream.
///
/// Unknown normalizer ids are an error: silent skipping would let a
/// meaningful difference hide.
pub fn apply_normalizers(events: Vec<Event>, ids: &[String]) -> Result<Vec<Event>, String> {
    let mut out = events;
    for id in ids {
        match id.as_str() {
            "normalize.pid" => out = normalize_pid(out),
            "normalize.fd" => out = normalize_fd(out),
            "normalize.timestamp" => out = normalize_timestamp(out),
            "normalize.token" => out = normalize_token(out),
            "normalize.trap_order" => out = normalize_trap_order(out),
            other => return Err(format!("unknown normalizer: {other}")),
        }
    }
    Ok(out)
}

/// Replace process ids with the literal `PID`.
fn normalize_pid(events: Vec<Event>) -> Vec<Event> {
    events
        .into_iter()
        .map(|mut e| {
            e.pid = None;
            e
        })
        .collect()
}

/// Replace fd numbers with the literal `FD` (identity/closure state is NOT
/// normalized: content and closure are compared exactly).
fn normalize_fd(events: Vec<Event>) -> Vec<Event> {
    events
        .into_iter()
        .map(|mut e| {
            if e.fd.is_some() {
                e.fd = Some(-1);
            }
            e
        })
        .collect()
}

/// Replace absolute timestamps with elapsed ms since the first event.
fn normalize_timestamp(events: Vec<Event>) -> Vec<Event> {
    // The protocol does not currently emit absolute timestamps; this rule is
    // reserved for courts that add them. Ordering is preserved.
    events
}

/// Replace random endpoint/port tokens with deterministic values derived from
/// their sequence position.
fn normalize_token(events: Vec<Event>) -> Vec<Event> {
    // Tokens are emitted in symbolic space already (the harness maps real
    // identities to casefile tokens); this rule is reserved for raw-token
    // courts.
    events
}

/// Sort trap events by trigger context when the casefile declares that
/// multi-trigger ordering is nondeterministic.
fn normalize_trap_order(events: Vec<Event>) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    let mut run: Vec<Event> = Vec::new();
    for e in events {
        if e.event == crate::events::EventKind::Trap {
            run.push(e);
        } else {
            if !run.is_empty() {
                run.sort_by_key(|x| x.trigger_context.unwrap_or(0));
                out.append(&mut run);
            }
            out.push(e);
        }
    }
    if !run.is_empty() {
        run.sort_by_key(|x| x.trigger_context.unwrap_or(0));
        out.append(&mut run);
    }
    out
}

/// Whether two JSON values are equal (used for expected-contract checks).
pub fn json_eq(a: &Value, b: &Value) -> bool {
    a == b
}

#[cfg(test)]
// Test assertions intentionally use `unwrap()`; the workspace policy denies it
// in runtime paths, and these modules are test-only.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::events::{Event, EventKind};

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            op_id: 0,
            event: kind,
            result: "MOJO_RESULT_OK".into(),
            handle: None,
            payload_hex: None,
            handles: None,
            signals: None,
            trigger_context: None,
            signals_state: None,
            outputs: None,
            process: None,
            pid: Some(42),
            fd: Some(7),
            num_bytes: None,
            size: None,
            note: None,
        }
    }

    #[test]
    fn pid_and_fd_normalized() {
        let events = vec![ev(1, EventKind::Result)];
        let out =
            apply_normalizers(events, &["normalize.pid".into(), "normalize.fd".into()]).unwrap();
        assert!(out[0].pid.is_none());
        assert_eq!(out[0].fd, Some(-1));
    }

    #[test]
    fn unknown_normalizer_is_error() {
        let err = apply_normalizers(vec![], &["normalize.bogus".into()]);
        assert!(err.is_err());
    }

    #[test]
    fn trap_order_normalized() {
        let mut e1 = ev(1, EventKind::Trap);
        e1.trigger_context = Some(5);
        let mut e2 = ev(2, EventKind::Trap);
        e2.trigger_context = Some(1);
        let e3 = ev(3, EventKind::Result);
        let out = apply_normalizers(vec![e1, e2, e3], &["normalize.trap_order".into()]).unwrap();
        assert_eq!(out[0].trigger_context, Some(1));
        assert_eq!(out[1].trigger_context, Some(5));
        assert_eq!(out[2].seq, 3);
    }
}
