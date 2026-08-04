# mojo-rs — Implementation Status

Status as of: 2026-08-04 (epoch 1, Chromium 151.0.7922.105, commit
bfa3579138998e2fbb981725570fa588c5b6f8cd, CoreIpcz architecture).

The capability ladder (atlas/feature-matrix.json) is authoritative. A status
below is a CLAIM only when the cited evidence exists and verifies.

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

- interop.cpp (C++ client ⇄ Rust server): the candidate must speak the ipcz
  node-link protocol (captured in evidence/invitations/wire/) to accept an
  official invitation. This is the Phase 3 seal gate.
- Routing / port transfer (Phase 5): the ipcz message layer
  (MessageHeader, node messages, shared-memory mailboxes) plus portal state
  machines, developed against the captured wire and the pinned source.
- Data pipes, shared buffers, C ABI export, mojom toolchain, bindings,
  concurrency/stress, fuzzing, other platforms.

Every phase gate in the master directive §14 applies; a waived gate requires a
written reason here.

## Receipts and reproduction

- `scripts/run_court.sh system` — sealed 10-case court (oracle baseline,
  candidate baseline, byte-identity, hashed manifest).
- `scripts/run_invite_court.sh` — official invitation flow + wire capture.
- `scripts/run_court.sh verify <manifest>` — receipt invalidation check.
- One-command reproduction from clean Docker images:
  `scripts/compose_project.sh build && scripts/run_court.sh system`
  (daemon: `scripts/start_project_docker.sh`).
