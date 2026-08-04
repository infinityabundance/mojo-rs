//! Blocking waits on dispatcher signals.
//!
//! `Waiter::wait` blocks until the watched signals are satisfied, become
//! unsatisfiable, or the deadline expires. The implementation registers a
//! one-shot watch and parks on a condvar; state changes wake the waiter, which
//! re-checks the signal state (spurious wakeups are safe).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::dispatcher::Dispatcher;
use crate::error::{CoreError, CoreResult};
use crate::signal::{Signals, SignalsState};
use crate::trap::WatchCallback;

/// A blocking waiter for signal changes.
#[derive(Clone, Default)]
pub struct Waiter {
    inner: Arc<Mutex<WaiterInner>>,
    cond: Arc<Condvar>,
}

#[derive(Default)]
struct WaiterInner {
    woken: bool,
}

impl Waiter {
    /// Create a waiter.
    pub fn new() -> Waiter {
        Waiter::default()
    }

    /// Block until `signals` are satisfied on `dispatcher`, they become
    /// unsatisfiable, or `deadline` expires.
    ///
    /// * `CoreError::Ok` — signals satisfied.
    /// * `CoreError::FailedPrecondition` — signals unsatisfiable.
    /// * `CoreError::DeadlineExceeded` — deadline expired.
    pub fn wait(
        &self,
        dispatcher: &Arc<dyn Dispatcher>,
        signals: Signals,
        deadline: Option<Instant>,
    ) -> CoreResult<SignalsState> {
        loop {
            // Fast path: check the current state.
            let state = dispatcher.query_signals();
            if state.is_satisfied(signals) {
                return Ok(state);
            }
            if state.is_unsatisfiable(signals) {
                return Err(CoreError::FailedPrecondition);
            }
            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    return Err(CoreError::DeadlineExceeded);
                }
            }

            // Park until a state change. Register a fresh one-shot watch and
            // re-check to close the lost-wakeup window.
            let callback = self.watch_callback();
            let watch = dispatcher.start_watch(signals, callback);
            let state2 = dispatcher.query_signals();
            if state2.is_satisfied(signals) {
                dispatcher.cancel_watch(watch);
                return Ok(state2);
            }
            if state2.is_unsatisfiable(signals) {
                dispatcher.cancel_watch(watch);
                return Err(CoreError::FailedPrecondition);
            }

            let mut inner = match self.inner.lock() {
                Ok(i) => i,
                Err(_) => {
                    dispatcher.cancel_watch(watch);
                    return Err(CoreError::Internal);
                }
            };
            if !inner.woken {
                let timed_out = match deadline {
                    Some(dl) => {
                        let now = Instant::now();
                        if dl <= now {
                            dispatcher.cancel_watch(watch);
                            return Err(CoreError::DeadlineExceeded);
                        }
                        let timeout = dl - now;
                        let (guard, result) = self
                            .cond
                            .wait_timeout(inner, timeout)
                            .map_err(|_| CoreError::Internal)?;
                        inner = guard;
                        result.timed_out()
                    }
                    None => {
                        inner = self.cond.wait(inner).map_err(|_| CoreError::Internal)?;
                        false
                    }
                };
                if timed_out && !inner.woken {
                    dispatcher.cancel_watch(watch);
                    return Err(CoreError::DeadlineExceeded);
                }
            }
            inner.woken = false;
            drop(inner);
            dispatcher.cancel_watch(watch);
            // Loop: re-evaluate with the fresh state.
        }
    }

    /// A callback that marks the waiter woken.
    fn watch_callback(&self) -> WatchCallback {
        let inner = Arc::clone(&self.inner);
        let cond = Arc::clone(&self.cond);
        Arc::new(move |_state: SignalsState, _kind: crate::trap::WatchKind| {
            if let Ok(mut i) = inner.lock() {
                i.woken = true;
            }
            cond.notify_all();
        })
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.cond.notify_all();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::message::Message;
    use crate::pipe::{End, MessagePipe};

    #[test]
    fn wait_returns_when_satisfied() {
        let (a, b) = MessagePipe::create();
        let b_d: Arc<dyn Dispatcher> = b;
        let w = Waiter::new();
        let a2 = Arc::clone(&a);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            a2.pipe().write(End::A, Message::empty())
        });
        let _ = handle.join();
        let state = w
            .wait(
                &b_d,
                Signals::READABLE,
                Some(Instant::now() + Duration::from_secs(5)),
            )
            .unwrap();
        assert!(state.is_satisfied(Signals::READABLE));
    }

    #[test]
    fn wait_deadline_exceeded() {
        let (_a, b) = MessagePipe::create();
        let b_d: Arc<dyn Dispatcher> = b;
        let w = Waiter::new();
        let err = w
            .wait(
                &b_d,
                Signals::READABLE,
                Some(Instant::now() + Duration::from_millis(30)),
            )
            .unwrap_err();
        assert_eq!(err, CoreError::DeadlineExceeded);
    }

    #[test]
    fn wait_unsatisfiable() {
        let (_a, b) = MessagePipe::create();
        let b_d: Arc<dyn Dispatcher> = b;
        b_d.on_closed(); // close the endpoint locally
        let w = Waiter::new();
        let err = w
            .wait(
                &b_d,
                Signals::WRITABLE,
                Some(Instant::now() + Duration::from_secs(1)),
            )
            .unwrap_err();
        assert_eq!(err, CoreError::FailedPrecondition);
    }
}
