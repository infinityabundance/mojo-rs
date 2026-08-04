# mojo-rs-core

The native Mojo runtime core.

**Status: Scaffolded** (structure only; no behavior yet).

## Scope (Phase 2+)

* Handle table with generation counters (no stale-handle confusion).
* Dispatcher ownership and endpoint lifecycle.
* Message pipes, message queues, and peer closure.
* Signals and signal-state tracking.
* Traps and waits (with cancellation safety).
* Invitations and process bootstrapping.
* Shared buffers and data pipes.
* Resource accounting and shutdown.

## Invariants (documented + court-tested)

* A transferred handle has exactly one logical owner.
* A closed handle cannot be resurrected.
* A message-pipe endpoint has one peer identity at a time.
* Messages remain ordered per the public contract.
* Closure is eventually observable.
* A trap fires at most as permitted by its registration state.
* Cancellation cannot produce a use-after-free callback.
* Failed transfer does not leak or duplicate ownership.
* Peer death cannot leave permanently unreachable live resources.

## Concurrency

* Every shared mutable structure is documented with its lock ordering.
* No callbacks while holding internal locks.
* Loom models cover the dangerous ownership transitions.
