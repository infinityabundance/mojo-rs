//! Signals and signal-state structures.
//!
//! Constants mirror the pinned `mojo/public/c/system/types.h` (epoch 1).
//! `Signals` is a bitmask; `SignalsState` reports what is currently satisfied
//! and what could become satisfied.

/// A set of Mojo handle signals (bitmask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Signals(u32);

impl Signals {
    /// No signals.
    pub const NONE: Signals = Signals(0);
    /// Data is available to read.
    pub const READABLE: Signals = Signals(1 << 0);
    /// Data can be written without blocking.
    pub const WRITABLE: Signals = Signals(1 << 1);
    /// The peer endpoint is closed.
    pub const PEER_CLOSED: Signals = Signals(1 << 2);
    /// New data arrived since the last read (data pipes).
    pub const NEW_DATA_READABLE: Signals = Signals(1 << 3);
    /// The peer is on a different process/node.
    pub const PEER_REMOTE: Signals = Signals(1 << 4);
    /// A quota was exceeded (quota API).
    pub const QUOTA_EXCEEDED: Signals = Signals(1 << 5);

    /// All signals understood by this implementation.
    pub const ALL: Signals = Signals((1 << 6) - 1);

    /// Build a signal set from a raw bitmask.
    pub fn from_bits(bits: u32) -> Signals {
        Signals(bits & Signals::ALL.0)
    }

    /// The raw bitmask.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Whether the set contains `other`.
    pub fn contains(self, other: Signals) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the set is empty.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for Signals {
    type Output = Signals;
    fn bitor(self, rhs: Signals) -> Signals {
        Signals(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for Signals {
    type Output = Signals;
    fn bitand(self, rhs: Signals) -> Signals {
        Signals(self.0 & rhs.0)
    }
}

/// The state of a handle's signals: currently satisfied and potentially
/// satisfiable sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignalsState {
    /// Signals currently satisfied.
    pub satisfied: Signals,
    /// Signals that could become satisfied in the future.
    pub satisfiable: Signals,
}

impl SignalsState {
    /// Query the result of watching for `watch` given this state.
    ///
    /// Returns `Ok(())` if `watch` is already satisfied, and `Err(unsatisfiable)`
    /// if `watch` can never become satisfied.
    pub fn watch_result(self, watch: Signals) -> Result<(), Signals> {
        if self.satisfied.contains(watch) {
            Ok(())
        } else if !self.satisfiable.contains(watch) {
            Err(self.satisfiable)
        } else {
            // Pending: not satisfied, still satisfiable.
            Err(Signals::NONE)
        }
    }

    /// Whether the trigger set is currently satisfied.
    pub fn is_satisfied(self, trigger: Signals) -> bool {
        self.satisfied.contains(trigger)
    }

    /// Whether the trigger set can never be satisfied.
    pub fn is_unsatisfiable(self, trigger: Signals) -> bool {
        !self.satisfied.contains(trigger) && !self.satisfiable.contains(trigger)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn constants_match_pinned_types_h() {
        assert_eq!(Signals::READABLE.bits(), 1 << 0);
        assert_eq!(Signals::WRITABLE.bits(), 1 << 1);
        assert_eq!(Signals::PEER_CLOSED.bits(), 1 << 2);
        assert_eq!(Signals::NEW_DATA_READABLE.bits(), 1 << 3);
        assert_eq!(Signals::PEER_REMOTE.bits(), 1 << 4);
        assert_eq!(Signals::QUOTA_EXCEEDED.bits(), 1 << 5);
    }

    #[test]
    fn watch_result_classifies_states() {
        let st = SignalsState {
            satisfied: Signals::WRITABLE,
            satisfiable: Signals::READABLE | Signals::WRITABLE | Signals::PEER_CLOSED,
        };
        assert!(st.watch_result(Signals::WRITABLE).is_ok());
        assert!(st.watch_result(Signals::READABLE).is_err());
        let (unsat, _) = (st.watch_result(Signals::READABLE), ());
        assert_eq!(unsat.err(), Some(Signals::NONE));

        let closed = SignalsState {
            satisfied: Signals::PEER_CLOSED,
            satisfiable: Signals::NONE,
        };
        assert!(closed.watch_result(Signals::READABLE).is_err());
        assert_eq!(
            closed.watch_result(Signals::READABLE).err(),
            Some(Signals::NONE)
        );
        assert!(closed.watch_result(Signals::PEER_CLOSED).is_ok());
        assert!(closed.is_unsatisfiable(Signals::READABLE));
    }
}
