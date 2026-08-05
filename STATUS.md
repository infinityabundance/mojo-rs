# mojo-rs — Implementation Status

Status as of: 2026-08-05 (epoch 1, Chromium 151.0.7922.105, commit
bfa3579138998e2fbb981725570fa588c5b6f8cd, CoreIpcz architecture).

The capability ladder (atlas/feature-matrix.json) is authoritative. A status
below is a CLAIM only when the cited evidence exists and verifies.

## Distribution

* Repository: <https://github.com/infinityabundance/mojo-rs> (public, `main`).
* Crates.io: `mojo-rs` umbrella + 13 `mojo-rs-*` crates, all v0.2.0
  (published 2026-08-05, lockstep bump after the Phase 4 seal). The umbrella
  re-exports the runtime and language-toolchain crates; forensic tooling is
  published separately.
* Publication does not change the sealed capability matrix: a claim below is
  still a claim only with its cited evidence.

## Sealed

### Phase 4 differential parity seal — data pipes and shared buffers

The system court now runs **26 cases**; all produce BYTE-IDENTICAL event
streams between the official oracle and the native candidate. The 16 Phase 4
cases seal data pipes and shared buffers on the Mojo API surface:

| Case | Coverage | Residuals |
|---|---|---|
| DATA_PIPE.CREATE.001 | defaults, option validation, initial signals | 0 |
| DATA_PIPE.WRITE_READ.002 | one-phase FIFO, signal transitions | 0 |
| DATA_PIPE.ALL_OR_NONE.003 | all-or-none write, OUT_OF_RANGE/SHOULD_WAIT | 0 |
| DATA_PIPE.ELEMENT_SIZE.004 | element alignment, flush-before-check order | 0 |
| DATA_PIPE.TWO_PHASE.005 | begin/end write+read, zero-commit, end-without-begin | 0 |
| DATA_PIPE.BUSY.006 | BUSY during two-phase, zero-consume | 0 |
| DATA_PIPE.PEEK_QUERY_DISCARD.007 | QUERY/PEEK/DISCARD, invalid combos | 0 |
| DATA_PIPE.SIGNALS.008 | WRITABLE/READABLE/NEW_DATA_READABLE transitions | 0 |
| DATA_PIPE.PEER_CLOSE.009 | closure, buffered-data readability, failed ops | 0 |
| DATA_PIPE.TRAP.010 | trap fires, arm-failure blocking events, 0-signal | 0 |
| SHARED_BUFFER.CREATE.001 | create/info, zero-size, signal-query rejection | 0 |
| SHARED_BUFFER.DUPLICATE.002 | duplicate, shared pages across handles | 0 |
| SHARED_BUFFER.MAP_UNMAP.003 | map/unmap, range errors, unaligned offsets | 0 |
| SHARED_BUFFER.CROSS_HANDLE.004 | cross-handle page visibility | 0 |
| SHARED_BUFFER.MODE_STATE.005 | Writable→Unsafe/ReadOnly state machine | 0 |
| SHARED_BUFFER.INFO.006 | info on originals/duplicates, i32::MAX bound | 0 |

The candidate implementation (`mojo-rs-core` `data_pipe.rs`, `ring_buffer.rs`,
`shared_buffer.rs`) mirrors the pinned `ipcz_driver/data_pipe.{h,cc}`,
`ring_buffer.{h,cc}`, `shared_buffer.{h,cc}` and the C entries in
`core_ipcz.cc` operation-for-operation: the two-ring-per-pair model (each
endpoint owns a mapping of the shared region), control-message flushes that
skip zero counts (parcel presence is the signal), the latched
`has_new_data`, the region-mode state machine (Writable → Unsafe/ReadOnly on
first duplicate, then immutable), and `MapAt` failure semantics reported as
RESOURCE_EXHAUSTED.

Oracle-verified behavior corrections found while sealing this phase:
- Misaligned reads still flush the peer update before the element-size check
  fails (the data is available to the next aligned read).
- `MojoArmTrap` returns OK when the trap is already armed; the driver's
  blocking-event plumbing now fills and emits immediate events on a failed
  arm.
- Signal queries on shared buffers (and other boxed driver objects that are
  not data pipes) return MOJO_RESULT_INVALID_ARGUMENT.
- `get_buffer_info` reports the region size on originals and duplicates;
  zero-size and >i32::MAX creates are RESOURCE_EXHAUSTED.

Evidence: `evidence/oracle/system/*`, `evidence/candidate/system/*`
(byte-identical), `evidence/diffs/system/*`, manifests under
`evidence/manifests/system-*.json` (verified by `scripts/run_court.sh verify`).

`mojo-rs-system` now exposes the idiomatic safe Rust API for both
capabilities: ownership-enforcing producer/consumer endpoints, RAII
two-phase transactions (a dropped transaction cancels with a zero-length
commit), and RAII shared-buffer mappings (unmap-on-drop; writes are `unsafe`
because shared memory is inherently aliased across handles/processes).

### Phase 3 interop seal — bidirectional official C++ ⇄ native Rust transfer

The native Rust `ipcz-acceptor` (`mojo-rs-interop`) completes the official
ConnectNode handshake with the pinned official broker and exchanges a message
plus a wrapped descriptor in each direction through the bootstrap pipe.

`scripts/run_interop_court.sh` runs the official broker against both the
official oracle acceptor (baseline) and the native Rust acceptor, through the
wire-relay man-in-the-middle. The broker's event stream is BYTE-IDENTICAL in
the two runs, and both processes exit 0:

| Run | Broker exit | Peer exit |
|---|---|---|
| baseline (oracle acceptor) | 0 | 0 |
| interop (native Rust acceptor) | 0 | 0 |

Evidence: `evidence/interop/<stamp>/` (events, wire captures) and
`evidence/manifests/interop-<stamp>.json` (hashes). The wire shows the full
native exchange: Connect reply, StopProxyingToLocalPeer, fragment-based
AcceptParcel reply (shared-memory mailbox), RouteClosed.

The native side implements, against the pinned ipcz sources:

* `NodeConnectorForNonBrokerToBroker`: Connect greeting handling + reply.
* `NodeLinkMemory::PrimaryBuffer`: link-memory adoption, fragment resolution,
  `RouterLinkState` status bits, parcel `FragmentHeader` publish/consume.
* `Router::AcceptBypassLink`: adoption of the broker's peer-initiated bypass
  (new central link, decay of the old link, `StopProxyingToLocalPeer`).
* `AcceptParcel` delivery (inline and shared-memory fragment paths), split
  parcels (`AcceptParcelDriverObjects`), `RouteClosed` propagation.
* The official `BlockAllocator` free-list for 64-byte parcel fragments.

## Sealed

### First differential parity seal — in-process system court (10 cases)
The native candidate and the official oracle produce BYTE-IDENTICAL event
streams for every case in `courts/system/`:

| Case | Court | Residuals |
|---|---|---|
| MESSAGE_PIPE.BASIC.001 | message-pipe | 0 |
| MESSAGE_PIPE.PAYLOAD.002 | message-pipe | 0 |
| MESSAGE_PIPE.HANDLES.003 | message-pipe (handle attach/extract) | 0 |
| MESSAGE_PIPE.PEER_CLOSE.004 | message-pipe (closure + signals) | 0 |
| MESSAGE_PIPE.BIDIRECTIONAL.005 | message-pipe | 0 |
| SIGNALS.STATES.001 | signals | 0 |
| TRAP.BASIC.001 | traps | 0 |
| WAIT.BASIC.001 | waits | 0 |
| PLATFORM.FD_TRANSFER.001 | platform handles | 0 |
| HANDLE.CLOSE.001 | handle lifecycle | 0 |

Evidence: `evidence/oracle/system/*.events`, `evidence/candidate/system/*.events`
(byte-identical), `evidence/diffs/system/*.json`, manifests under
`evidence/manifests/system-*.json` (input hashes included; `scripts/run_court.sh
verify <manifest>` invalidates on any change).

Oracle-verified behavior corrections (each traced to the pinned source):
- Message-pipe signals mirror `MojoQueryHandleSignalsStateIpcz`: satisfiable
  always includes PEER_CLOSED|QUOTA_EXCEEDED; WRITABLE|PEER_REMOTE only while
  the peer is open; READABLE while the portal is not DEAD (peer closed AND
  queue empty).
- Trap events carry the trigger result: OK when satisfied, FAILED_PRECONDITION
  when unsatisfiable, CANCELLED (zero signal state) on trigger removal or
  watched-handle close — the latter delivered even when unarmed
  (TrapRemovalEventHandler semantics).
- The epoch's C API removed MojoWait/MojoDeadline; waits are poll-based with
  the same observable contract.
- MojoWriteMessage consumes the message on failure (double-destroy crashed the
  official oracle — fixed in the driver).

### Cross-process transport (candidate-internal)
`crates/mojo-rs-platform` cross_process_message_and_fd_transfer: two processes
over a socketpair exchange messages + memfds via SCM_RIGHTS and observe
peer-close EOF. Fixed: `recv_with_fds` EOF detection (zero-length read is EOF
regardless of the pre-set control length) and CLOEXEC hygiene for the parent
endpoint.

### Official invitation flow (oracle side) + wire capture
`mojo_rs_oracle_driver invite-broker / invite-acceptor` run the OFFICIAL
broker⇄acceptor flow (MojoCreateInvitation, MojoAttachMessagePipeToInvitation,
MojoSendInvitation/MojoAcceptInvitation, MojoExtractMessagePipeFromInvitation)
over a socketpair, exchanging a message + wrapped memfd in each direction.
Both processes exit 0. The wire-relay man-in-the-middle captures the raw ipcz
node-link traffic (536 + 752 bytes).

Evidence: `evidence/invitations/`, `evidence/manifests/invite-*.json`.

### Negative proof
`scripts/verify_no_oracle_dependency.sh` PASS: no external mojo/ipcz/chromium
crate in `cargo tree`, no Mojo* symbols, no mojo libs linked, no source
references to the oracle checkout. Evidence: `evidence/security/`.

## Not yet sealed (next gates)

- Routing / port transfer (Phase 5): the full portal state machines —
  portal transfer through parcels (RouterDescriptor), proxy bypass
  (BypassPeer/AcceptBypassLink), node loss (RouteDisconnected), multi-node
  graphs, and the remaining NodeLink message types (RequestMemory,
  ProvideMemory, AddBlockBuffer beyond the primary buffer). The Phase 3
  acceptor covers the direct central-link subset.
- C ABI export (Phase 6), mojom toolchain and bindings (Phase 7),
  concurrency/stress/fuzz sealing, other platforms.

Every phase gate in the master directive §14 applies; a waived gate requires a
written reason here.

## Receipts and reproduction

- `scripts/run_court.sh system` — sealed 26-case court (message pipes,
  signals, traps, waits, handle lifecycle, platform handles, data pipes,
  shared buffers; oracle baseline, candidate baseline, byte-identity, hashed
  manifest).
- `scripts/run_invite_court.sh` — official invitation flow + wire capture.
- `scripts/run_interop_court.sh` — Phase 3 interop seal (official broker ⇄
  native acceptor).
- `scripts/run_court.sh verify <manifest>` — receipt invalidation check.
- One-command reproduction from clean Docker images:
  `scripts/compose_project.sh build && scripts/run_court.sh system`
  (daemon: `scripts/start_project_docker.sh`).
