# WORKLOG

Session log for the mojo-rs implementation. Each entry follows the reporting
discipline of the master directive (§17): implemented, files changed, claims,
courts run, counts, residuals, mismatches, root causes, unsupported behavior,
next gate.

---

## Cycle 2026-08-04 — Phase 0: cartography and oracle establishment

### 1. What was actually implemented

* Repository scaffold, workspace, and storage/daemon infrastructure per the
  addendum: isolated project Docker daemon rooted at the host-specific
  `MOJO_RS_DOCKER_ROOT` (`/run/media/one/1tb_kingston1/docker/mojo-rs`).
* Chromium epoch-1 pin: tag `151.0.7922.105` (commit
  `bfa3579138998e2fbb981725570fa588c5b6f8cd`), verified against
  chromium.googlesource and chromiumdash.
* Environment resolution (precedence CLI > env > local file > portable
  defaults), daemon lifecycle, Ubuntu image selection, storage verification,
  oracle source fetch, environment receipts.
* Casefile schema v1, court manifests, atlas structure.

### 2. Which files changed

See `git log --stat` for the cycle.

### 3. Compatibility claims now supported

None yet (correctly): the project is at `Scaffolded`/`Parsed` only.

### 4. Which courts were run

None yet. The first runnable court is the wire differential court (Phase 1).

### 5. Exact pass/fail counts

n/a.

### 6. New residuals and evidence paths

n/a.

### 7. Every observed mismatch

n/a.

### 8. Root cause of each fixed mismatch

n/a.

### 9. Remaining unsupported behavior

Everything behavioral.

### 10. Next highest-value parity gate

Pin + fetch the oracle source; build the official Mojo oracle (libmojo_core +
driver); seal the first wire differential fixtures; then the message-pipe slice.

---

## Cycle 2026-08-05 — GitHub publication and crates.io release

### 1. What was actually implemented

* Pushed the repository to GitHub as `infinityabundance/mojo-rs` (public,
  default branch `main`), no source changes required.
* Published all 14 workspace crates to crates.io at v0.1.0, with `mojo-rs` as
  the workspace umbrella crate re-exporting the full product surface.
* Publication prep (commit `e435a87`):
  * Replaced the `example.invalid` repository/homepage URLs with the real
    GitHub URL.
  * Added `LICENSE-MIT` and `LICENSE-APACHE` matching the declared
    `MIT OR Apache-2.0` license.
  * Centralized internal crates in `[workspace.dependencies]` with explicit
    versions so every member satisfies crates.io publish resolution.
  * Extended the `mojo-rs` facade to re-export `wire`, `core`, `platform`,
    `system`, `c_api`, `io`, `bindings`, `mojom`, `codegen` (forensic tooling
    deliberately excluded from the facade).
  * Clippy policy fix: `#[allow(clippy::unwrap_used)]` on test-only modules in
    `mojo-rs-casefile`; runtime paths remain denied.

### 2. Which files changed

`Cargo.toml`, `LICENSE-MIT`, `LICENSE-APACHE`, `crates/*/Cargo.toml` (all 14),
`crates/mojo-rs/src/lib.rs`,
`crates/mojo-rs-casefile/src/{compare,normalizers}.rs`, `Cargo.lock`.

### 3. Compatibility claims now supported

None new. Publication does not change sealed behavior; every existing claim
remains evidence-gated per `atlas/feature-matrix.json` and `STATUS.md`.

### 4. Which courts were run

* `cargo test --workspace`: 78/78 pass.
* `cargo clippy --workspace --all-targets`: 0 errors.
* `cargo fmt --check`: clean.
* `cargo publish --dry-run` for every crate before upload (packaging +
  verification build each).
* Post-publish consumer smoke test: a scratch project depending on
  `mojo-rs`, `mojo-rs-wire`, `mojo-rs-casefile` at `0.1.0` from the real
  registry (no path deps) built and ran, exercising umbrella re-exports and
  direct dependencies.
* Registry verification: all 14 crates report `max_version 0.1.0`; the
  `mojo-rs-interop` 0.1.0 archive was downloaded and confirmed to contain
  `testdata/ipcz/*.bin` fixtures.

### 5. Exact pass/fail counts

tests 78/78; clippy errors 0; smoke build/run pass; 14/14 crates live on
crates.io; 0 failures.

### 6. New residuals and evidence paths

* Repository: <https://github.com/infinityabundance/mojo-rs>
* Umbrella crate: <https://crates.io/crates/mojo-rs>
* Crates: `mojo-rs-wire`, `mojo-rs-core`, `mojo-rs-system`, `mojo-rs-platform`,
  `mojo-rs-c-api`, `mojo-rs-bindings`, `mojo-rs-mojom`, `mojo-rs-codegen`,
  `mojo-rs-io`, `mojo-rs-oracle`, `mojo-rs-casefile`, `mojo-rs-interop`,
  `mojo-rs-test-support` (all v0.1.0).
* docs.rs builds are queued and were being verified at cycle close.

### 7. Every observed mismatch

None in behavior. Operational only: crates.io's new-crate rate limit (~1 new
crate per 10-minute refill window after the initial burst of 5) paced the 14
publishes across ~90 minutes; retry timestamps from the API were followed; no
content changes were required.

### 8. Root cause of each fixed mismatch

n/a (no behavioral mismatches this cycle).

### 9. Remaining unsupported behavior

Unchanged: see `STATUS.md` — the Phase 3 interop seal gate, routing/port
transfer (Phase 5), data pipes, shared buffers, C ABI export, mojom toolchain,
bindings, concurrency/stress, fuzzing, and additional platforms.

### 10. Next highest-value parity gate

The Phase 3 seal gate: official C++ client ⇄ native Rust server over the ipcz
node-link protocol, built against the captured wire
(`evidence/invitations/wire/`) and the pinned source.

---

## Cycle 2026-08-05 — Phase 3 interop seal: the native ipcz acceptor

### 1. What was actually implemented

The native Rust ipcz acceptor (`mojo-rs-interop/src/ipcz/`): a non-broker
node that completes the official ConnectNode handshake with the pinned
official broker and exchanges a message plus a wrapped descriptor in each
direction through the bootstrap pipe. New modules:

* `channel.rs` — the socket transport: `IpczHeader` framing, `SCM_RIGHTS`
  descriptor tracking by byte offset, poll + nonblocking drain, classified
  malformed-input errors.
* `link_memory.rs` — the shared primary buffer: layout constants, fragment
  resolution with bounds checks, `RouterLinkState` status bits (atomic),
  parcel `FragmentHeader` publish/consume (release/acquire), the official
  `BlockAllocator` free-list for 64-byte blocks.
* `messages.rs` — full decode of the NodeLink message types the acceptor
  handles (Connect, AddBlockBuffer, AcceptParcel, AcceptParcelDriverObjects,
  RouteClosed/Disconnected, BypassPeerWithLink, StopProxyingToLocalPeer,
  FlushRouter, RequestMemory/ProvideMemory) plus byte-exact encoders
  (golden tests against the captured official wire).
* `acceptor.rs` — the state machine: Connect handshake, initial portals,
  AcceptParcel delivery (inline and shared-memory fragment paths), split
  parcels, peer-initiated bypass adoption (`AcceptBypassLink` semantics),
  RouteClosed propagation, unsupported types rejected explicitly.
* `bin/ipcz-acceptor.rs` — the harness (oracle-compatible
  `<socket-fd> <events.jsonl>` interface).
* `scripts/run_interop_court.sh` — the Phase 3 gate court: official broker
  against the oracle acceptor (baseline) and the native acceptor (interop),
  with wire capture and hashed receipts.
* `mojo-rs-platform`: `SharedMemory::from_raw_fd` (adopt a transferred
  memfd), `sys::fstat` wrapper, `Mapping::DerefMut`.

### 2. Which files changed

`crates/mojo-rs-interop/src/ipcz/{mod,wire,messages,channel,link_memory,acceptor}.rs`,
`crates/mojo-rs-interop/src/bin/ipcz-acceptor.rs`,
`crates/mojo-rs-platform/src/{shm,sys}.rs`,
`scripts/run_interop_court.sh`, `atlas/feature-matrix.json`, `STATUS.md`.

### 3. Compatibility claims now supported

* `interop.cpp` capability moved from Scaffolded to **Interoperable**: the
  official broker's event stream is byte-identical whether its peer is the
  official oracle acceptor or the native Rust acceptor; both peers exit 0.
* The Connect handshake, link-memory mailbox (fragment-based AcceptParcel),
  bypass adoption, and closure propagation are demonstrated on the official
  wire.

### 4. Which courts were run

* `cargo test --workspace`: 93/93 (23 in mojo-rs-interop: golden byte-identical
  encode tests against the captured official wire, decode/validation tests,
  channel tests, link-memory tests).
* `cargo clippy --workspace --all-targets`: 0 errors.
* `cargo fmt --check`: clean.
* `scripts/run_interop_court.sh`: PASS x4 (broker event streams byte-identical;
  baseline and interop pairs both exit 0).
* `scripts/verify_no_oracle_dependency.sh`: PASS.

### 5. Exact pass/fail counts

tests 93/93; clippy 0; fmt 0 diffs; interop court 4/4 passes; negative proof PASS.

### 6. New residuals and evidence paths

`evidence/interop/<stamp>/` (baseline + interop broker/acceptor events, wire
captures) and `evidence/manifests/interop-<stamp>.json`. The native
acceptor's wire: Connect reply (80B), StopProxyingToLocalPeer (64B),
fragment-based AcceptParcel reply (168B, shared-memory mailbox),
RouteClosed (64B).

### 7. Every observed mismatch

* Broker vs oracle baseline message sequence: the oracle acceptor initiates
  its own bypass (the broker then sends StopProxyingToLocalPeer before its
  own BypassPeerWithLink); the native acceptor does not initiate bypass, so
  the broker sends only BypassPeerWithLink. This is a permitted divergence
  (the routing messages are optimizations), verified by the byte-identical
  broker EVENT streams.
* Fixed during the cycle: (a) AcceptParcel params were 80 bytes (stray
  fragment padding field) — desynchronized array offsets, broker rejected the
  parcel; (b) blocking sockets deadlocked the drain (recvmsg never returns
  WouldBlock) — switched to poll + nonblocking; (c) parcel fragment
  `FragmentHeader` field order was reversed (size at offset 4 instead of 0) —
  broker read size 0; (d) fragment size was published before the data
  (release-sequence violation) — data now written first; (e) reply memfd was
  4096 bytes (ftruncate) instead of exactly the content — trailing zeros
  failed the oracle's content comparison.

### 8. Root cause of each fixed mismatch

See above: each was a wire-layout or memory-ordering deviation from the
pinned sources, found by decoding the captured wire and comparing
byte-for-byte.

### 9. Remaining unsupported behavior

Routing/port transfer beyond the direct central link (Phase 5), data pipes,
shared buffers (Mojo API), C ABI export, mojom toolchain, bindings,
concurrency/stress, fuzzing, other platforms. The acceptor rejects portal
transfers (kPortal) and multi-subparcel parcels explicitly.

### 10. Next highest-value parity gate

Phase 5 routing: portal transfer via `RouterDescriptor` parcels and the
remaining NodeLink message types (BypassPeer/AcceptBypassLink proxy bypass,
RequestMemory/ProvideMemory), building the full router state machine against
the pinned sources and the captured wire.

---

## Cycle 2026-08-05 — Phase 4 differential parity seal: data pipes and shared buffers

### 1. What was actually implemented

* `mojo-rs-platform::shm`: `SharedMemory::duplicate()` and page-unaligned
  `map()` (align-down + pointer adjustment, mirroring base `MapAt`); `Mapping`
  now records the aligned base/total length for `munmap` and is `Send + Sync`
  under the documented external-synchronization invariant.
* `mojo-rs-core::ring_buffer`: `RingBuffer` mirroring the official
  `ipcz_driver/ring_buffer.{h,cc}` — circular `Range`, chunked `MapRange`
  semantics, `Write/WriteAll/Read/ReadAll/Peek/PeekAll/Discard/DiscardAll/
  ExtendDataRange`, `DirectWriter`/`DirectReader`, `SerializedState`.
* `mojo-rs-core::data_pipe`: the producer/consumer pair over a shared region
  with two per-endpoint ring views (the official model), per-direction
  control-message queues (parcel presence is the signal), `has_new_data`
  latch, exact error-code/ordering per the pinned `data_pipe.cc`, and
  watch/trap integration (fires on parcel arrival and peer close; CANCELLED on
  local close).
* `mojo-rs-core::shared_buffer`: region-mode state machine (Writable →
  Unsafe/ReadOnly on first duplicate, then immutable), `map` with `MapAt`
  failure semantics (RESOURCE_EXHAUSTED), the address-keyed `MappingTable`.
* Oracle driver: 15 new ops (`data_pipe_create`, `write_data`, `read_data`,
  `begin_write_data`, `end_write_data`, `begin_read_data`, `end_read_data`,
  `shared_buffer_create`, `duplicate_buffer_handle`, `map_buffer`,
  `unmap_buffer`, `read_mapping`, `get_buffer_info`) plus the `num_bytes`/
  `size` event fields; `MojoArmTrap` now passes blocking-event capacity.
* Candidate harness: the matching ops, `num_bytes`/`size` event fields,
  shared-buffer mapping tokens, and signal-query rejection for non-data-pipe
  boxed objects.
* `mojo-rs-system`: idiomatic safe APIs — RAII two-phase data-pipe
  transactions (drop cancels with a zero-length commit), RAII shared-buffer
  mappings (unmap-on-drop), `SystemError` taxonomy.

### 2. Which files changed

`crates/mojo-rs-platform/src/shm.rs`; `crates/mojo-rs-core/src/{ring_buffer,
data_pipe,shared_buffer,lib,trap}.rs`; `crates/mojo-rs-system/src/{error,
data_pipe,shared_buffer,lib}.rs`; `crates/mojo-rs-casefile/src/{events,
normalizers}.rs`; `crates/mojo-rs-interop/src/bin/candidate-harness.rs` and
`src/ipcz/acceptor.rs`; `oracle/driver/oracle_driver.cc`; `casefiles/schema/
{events,casefile}.schema.json`; `courts/system/*` (16 new casefiles +
manifest); `atlas/feature-matrix.json`; `STATUS.md`; `WORKLOG.md`.

### 3. Compatibility claims now supported

* `data-pipe` and `shared-buffer` capabilities upgraded from `Scaffolded` to
  `Oracle-compared` in `atlas/feature-matrix.json`, backed by 16 new
  byte-identical cases in the sealed 26-case system court.

### 4. Which courts were run

* `scripts/run_court.sh system`: 26/26 PASS, byte-identical (10 sealed
  message-pipe/signal/trap/wait/handle cases unchanged, 16 new Phase 4 cases).
* `scripts/run_invite_court.sh`: PASS (re-sealed with the rebuilt driver).
* `scripts/run_interop_court.sh`: PASS (re-sealed with the rebuilt driver).

### 5. Exact pass/fail counts

* `cargo test --workspace`: 32 suites, 0 failures (core 49, casefile 6+,
  system 9, platform 13, interop 34, ...).
* `cargo clippy --workspace --all-targets`: 0 errors.
* `cargo fmt --check`: clean.
* System court: 26 passed, 0 failed; receipt verified.

### 6. New residuals and evidence paths

`evidence/oracle/system/*.events`, `evidence/candidate/system/*.events`
(byte-identical for all 26), `evidence/diffs/system/*.json` (empty
residuals), `evidence/manifests/system-*.json` (hashed inputs).

### 7. Every observed mismatch

* First run: 4 shared-buffer cases failed with a `token`/`handle` key
  mismatch — the C++ driver emitted `"token"` which the Rust `Event` struct
  (no such field) dropped on parse, so the comparison saw the token vanish.
* The candidate's `Trap::arm` returned `FailedPrecondition` when re-armed
  while armed; the official returns OK.
* The oracle driver's `OpTrapArm` passed `num_events = 0`, so the official
  `MojoArmTrap` never filled blocking events on a failed arm — immediate
  events were silently lost.
* The candidate harness's `query_signals_state` returned OK for shared-buffer
  handles; the official rejects them with INVALID_ARGUMENT.
* A latent single-ring design bug (producer/consumer sharing one data range)
  double-counted flushes — caught by unit tests before the court run.

### 8. Root cause of each fixed mismatch

* Key naming: the event schema models `handle`, not `token` — the driver now
  emits `handle`.
* Re-arm semantics: official `MojoTrap::Arm` short-circuits `if (armed_)
  return OK` — the candidate now mirrors it.
* Blocking events: `MojoArmTrap` validates `blocking_events[0].struct_size`
  and only fills events when `event_capacity > 0` — the driver now passes
  capacity 4 with initialized struct_size.
* Signal queries: `MojoQueryHandleSignalsStateIpcz` rejects boxed driver
  objects that are not data pipes — the harness now classifies by dispatcher
  type.
* Ring model: the official gives each endpoint its own `RingBuffer` over its
  own mapping of the same region; a single shared range would extend twice
  for the same bytes. The pair now holds two ring views.

### 9. Remaining unsupported behavior

Phase 5 routing/port transfer (full router state machines), Phase 6 C ABI
export, Phase 7 mojom/bindings, concurrency/stress/fuzz sealing, other
platforms. Data pipes/shared buffers are sealed for the in-process Mojo API
surface; cross-process data-pipe/shared-buffer transfer is part of the Phase 5
routing work.

### 10. Next highest-value parity gate

Per the directive's sequence, Phase 5 routing: portal transfer via
`RouterDescriptor` parcels, proxy bypass (`BypassPeer`/`AcceptBypassLink`),
`RequestMemory`/`ProvideMemory`, multi-node graphs, against the pinned sources
and the captured wire.
