//! Traps: signal watching with registered callbacks (Mojo traps).
//!
//! Semantics follow the official Mojo trap contract:
//! * Events are delivered only when the trap is armed.
//! * Arming reports `FailedPrecondition` when a trigger is already satisfied
//!   or unsatisfiable, delivering the immediate events.
//! * A firing trap is disarmed; it must be re-armed to watch again.
//! * Removing a trigger or closing a watched handle delivers a
//!   `Cancelled` event for that trigger.
//! * Cancellation can never produce a use-after-free callback.

use std::sync::{Arc, Mutex, Weak};

use crate::dispatcher::{Dispatcher, DispatcherType, WatchId};
use crate::error::{CoreError, CoreResult};
use crate::signal::SignalsState;

/// An event delivered for a trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapEvent {
    /// The trigger context registered with `MojoAddTrigger`.
    pub trigger_context: u64,
    /// The signal state of the watched handle at the time of the event.
    pub signals_state: SignalsState,
    /// `CoreError::Ok` for a satisfaction event; `CoreError::Cancelled` for a
    /// removal/closure cancellation.
    pub result: CoreError,
}

/// The user trap callback: `fn(context: usize, event: &TrapEvent)`.
#[derive(Clone)]
pub struct TrapCallback {
    func: fn(usize, &TrapEvent),
    user_context: usize,
}

impl TrapCallback {
    /// A callback bound to a user context pointer.
    pub fn new(func: fn(usize, &TrapEvent), user_context: usize) -> TrapCallback {
        TrapCallback { func, user_context }
    }

    /// Invoke the callback.
    pub fn invoke(&self, event: &TrapEvent) {
        (self.func)(self.user_context, event);
    }
}

/// A callback invoked by a watched dispatcher when its signal state changes.
pub type WatchCallback = Arc<dyn Fn(SignalsState, WatchKind) + Send + Sync>;

/// Why a watch callback fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    /// The watched signals became satisfied or unsatisfiable (state change).
    Changed,
    /// The watched object was closed locally; the trigger is cancelled.
    Cancelled,
}

/// The signal-change observer installed on a watched dispatcher.
pub(crate) fn watch_callback(trap: &Weak<Trap>, context: u64) -> WatchCallback {
    let weak = Weak::clone(trap);
    Arc::new(move |state, kind| {
        if let Some(trap) = weak.upgrade() {
            trap.on_watch(context, state, kind);
        }
    })
}

/// A trap handle object.
pub struct Trap {
    inner: Mutex<TrapInner>,
    callback: TrapCallback,
}

struct TrapInner {
    triggers: Vec<Trigger>,
    armed: bool,
}

struct Trigger {
    watch: WatchId,
    /// The dispatcher being watched (kept alive for the trigger lifetime).
    dispatcher: Arc<dyn Dispatcher>,
    /// The signals this trigger watches for.
    signals: crate::signal::Signals,
    context: u64,
    removed: bool,
}

impl Trap {
    /// Whether any trigger is currently registered.
    pub fn trigger_count(&self) -> usize {
        self.inner
            .lock()
            .map(|i| i.triggers.iter().filter(|t| !t.removed).count())
            .unwrap_or(0)
    }

    /// Whether the trap is currently armed.
    pub fn is_armed(&self) -> bool {
        self.inner.lock().map(|i| i.armed).unwrap_or(false)
    }

    /// Create a trap with the given callback.
    pub fn create(callback: TrapCallback) -> Arc<Trap> {
        Arc::new(Trap {
            inner: Mutex::new(TrapInner {
                triggers: Vec::new(),
                armed: false,
            }),
            callback,
        })
    }

    /// Register a trigger: watch `dispatcher` for `signals`, delivering events
    /// with `context`.
    pub fn add_trigger(
        self: &Arc<Self>,
        dispatcher: Arc<dyn Dispatcher>,
        signals: crate::signal::Signals,
        context: u64,
    ) -> CoreResult<()> {
        let callback = watch_callback(&Arc::downgrade(self), context);
        let watch = dispatcher.start_watch(signals, callback);
        let mut inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        inner.triggers.push(Trigger {
            watch,
            dispatcher,
            signals,
            context,
            removed: false,
        });
        Ok(())
    }

    /// Remove a trigger by context.
    pub fn remove_trigger(&self, context: u64) -> CoreResult<()> {
        let mut inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        let Some(idx) = inner
            .triggers
            .iter()
            .position(|t| t.context == context && !t.removed)
        else {
            return Err(CoreError::NotFound);
        };
        let trigger = &mut inner.triggers[idx];
        trigger.removed = true;
        let watch = trigger.watch;
        let dispatcher = Arc::clone(&trigger.dispatcher);
        drop(inner);
        dispatcher.cancel_watch(watch);
        self.fire_cancelled(context);
        Ok(())
    }

    /// Arm the trap. If any trigger is already satisfied (or unsatisfiable),
    /// returns `CoreError::FailedPrecondition` and delivers the immediate
    /// events through the callback. Mirrors the official `MojoTrap::Arm`:
    /// the trap becomes armed only if every trigger could be installed, and
    /// re-arming an already-armed trap returns `Ok` (the official epoch's
    /// `MojoTrap::Arm` returns `MOJO_RESULT_OK` when `armed_` is already
    /// true).
    pub fn arm(&self) -> CoreResult<()> {
        let mut inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        if inner.armed {
            return Ok(());
        }
        // Evaluate every trigger against its dispatcher's current state.
        let mut immediate = Vec::new();
        for t in &inner.triggers {
            if t.removed {
                continue;
            }
            let state = t.dispatcher.query_signals();
            if t.signals.is_empty() {
                // Triggers watching no signals can never be armed.
                immediate.push(TrapEvent {
                    trigger_context: t.context,
                    signals_state: SignalsState::default(),
                    result: CoreError::FailedPrecondition,
                });
                continue;
            }
            if state.is_satisfied(t.signals) {
                immediate.push(TrapEvent {
                    trigger_context: t.context,
                    signals_state: state,
                    result: CoreError::Ok,
                });
            } else if state.is_unsatisfiable(t.signals) {
                immediate.push(TrapEvent {
                    trigger_context: t.context,
                    signals_state: state,
                    result: CoreError::FailedPrecondition,
                });
            }
        }
        if !immediate.is_empty() {
            // Deliver immediately (the trap does not become armed).
            drop(inner);
            for e in immediate {
                self.callback.invoke(&e);
            }
            return Err(CoreError::FailedPrecondition);
        }
        inner.armed = true;
        Ok(())
    }

    /// Called by a watched dispatcher when the signal state changes or the
    /// watched object is closed.
    fn on_watch(&self, context: u64, state: SignalsState, kind: WatchKind) {
        match kind {
            WatchKind::Cancelled => {
                // The watched handle was closed: the trigger is removed and a
                // CANCELLED event is delivered even if the trap is unarmed
                // (official `HandleTrapRemoved` semantics).
                let mut inner = match self.inner.lock() {
                    Ok(i) => i,
                    Err(_) => return,
                };
                let Some(idx) = inner
                    .triggers
                    .iter()
                    .position(|t| t.context == context && !t.removed)
                else {
                    return;
                };
                let event = TrapEvent {
                    trigger_context: context,
                    signals_state: SignalsState::default(),
                    result: CoreError::Cancelled,
                };
                inner.triggers[idx].removed = true;
                drop(inner);
                self.callback.invoke(&event);
            }
            WatchKind::Changed => {
                let mut inner = match self.inner.lock() {
                    Ok(i) => i,
                    Err(_) => return,
                };
                if !inner.armed {
                    return;
                }
                let Some(trigger) = inner
                    .triggers
                    .iter()
                    .find(|t| t.context == context && !t.removed)
                else {
                    return;
                };
                // The watch fired because the signals became satisfied or
                // unsatisfiable; the event result distinguishes the two
                // (official `GetEventResultForSignalsState`).
                let result = if state.is_satisfied(trigger.signals) {
                    CoreError::Ok
                } else {
                    CoreError::FailedPrecondition
                };
                let event = TrapEvent {
                    trigger_context: context,
                    signals_state: state,
                    result,
                };
                inner.armed = false;
                drop(inner);
                self.callback.invoke(&event);
            }
        }
    }

    /// Fire a cancellation event for a removed trigger.
    fn fire_cancelled(&self, context: u64) {
        let event = TrapEvent {
            trigger_context: context,
            signals_state: SignalsState::default(),
            result: CoreError::Cancelled,
        };
        self.callback.invoke(&event);
    }

    /// Close the trap: cancel all triggers (delivering cancellation events).
    pub fn close(&self) {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return,
        };
        let triggers: Vec<(WatchId, Arc<dyn Dispatcher>, u64)> = inner
            .triggers
            .drain(..)
            .filter(|t| !t.removed)
            .map(|t| (t.watch, t.dispatcher, t.context))
            .collect();
        drop(inner);
        for (watch, dispatcher, context) in triggers {
            dispatcher.cancel_watch(watch);
            self.fire_cancelled(context);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn callback_invocation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn cb(_ctx: usize, _e: &TrapEvent) {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        let cb = TrapCallback::new(cb, 0);
        let e = TrapEvent {
            trigger_context: 1,
            signals_state: SignalsState::default(),
            result: CoreError::Ok,
        };
        cb.invoke(&e);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }
}

impl Dispatcher for Trap {
    fn dispatcher_type(&self) -> DispatcherType {
        DispatcherType::Trap
    }

    fn query_signals(&self) -> SignalsState {
        SignalsState::default()
    }

    fn on_closed(&self) {
        self.close();
    }

    fn start_watch(&self, _signals: crate::signal::Signals, _callback: WatchCallback) -> WatchId {
        WatchId::new(0)
    }

    fn cancel_watch(&self, _id: WatchId) {}

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
