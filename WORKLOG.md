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

---

## Cycle 2026-08-05 — crates.io release v0.2.0 (Phase 4 surface)

### 1. What was actually implemented

* Lockstep workspace version bump 0.1.0 → 0.2.0 (all 14 crates).
* Published all 14 crates to crates.io at v0.2.0 in dependency order
  (platform → wire → mojom → casefile → codegen → core → c-api → io →
  bindings → system → oracle → test-support → interop → umbrella `mojo-rs`).
* Post-publish consumer smoke test: a scratch project depending on
  `mojo-rs = "0.2.0"` from the real registry (no path deps) built and ran,
  exercising the Phase 4 surface through the umbrella re-exports: one-phase
  and two-phase data pipes, shared-buffer duplicate/map/cross-handle
  visibility, and the core region-mode state machine.

### 2. Which files changed

`Cargo.toml`, `Cargo.lock`, `STATUS.md`, `WORKLOG.md`.

### 3. Compatibility claims now supported

None new (publication does not change sealed behavior); the published
artifacts now match the Phase 4 repository.

### 4. Which courts were run

Post-publish smoke build/run (pass). The sealed courts are unchanged by
publication.

### 5. Exact pass/fail counts

14/14 publishes succeeded (no rate-limit pacing for version updates);
smoke test pass; 0 failures.

### 6. New residuals and evidence paths

crates.io: `mojo-rs` 0.2.0 and all 13 members at 0.2.0.

### 7. Every observed mismatch

None.

### 8. Root cause of each fixed mismatch

n/a.

### 9. Remaining unsupported behavior

Unchanged: Phase 5 routing, Phase 6 C ABI export, Phase 7 mojom/bindings,
concurrency/stress/fuzz, other platforms.

### 10. Next highest-value parity gate

Phase 5 routing (per the directive's sequence).

## Cycle 2026-08-05 — Phase 5 routing seal (partial): portal transfer + proxy bypass

### 1. What was actually implemented

* `crates/mojo-rs-interop/src/ipcz/router.rs` (new): the ipcz `Router` state
  machine for a non-broker node — `LinkKind`/`LinkSide`, `Edge` with primary
  and decaying links (sequence-length bounds, deferred decay), `Parcel`,
  `ParcelQueue` (sequenced out-of-order buffer + final length), `Portal`,
  `Router` (terminal/proxy, collect/forward, decay finishing, side-stable
  marking).
* `crates/mojo-rs-interop/src/ipcz/routing.rs` (new): `RoutingAcceptor` — the
  full non-broker node over one NodeLink: Connect handshake, the
  shared-memory-service client handshake on portal 0 (byte-exact against the
  golden fixture), `Router::Deserialize` (incl. `proxy_already_bypassed`
  decaying-link setup), `SerializeNewRouterAndConfigureProxy` +
  `BeginProxyingToNewRouter`, `AcceptBypassLink` semantics for the broker's
  `BypassPeerWithLink`, `StopProxying` teardown, `RouteClosed` propagation,
  `TryLockForClosure` via `RouterLinkState::TryLock`, parcel transmit with
  portal serialization, per-link sequence numbering, `early_parcels`
  buffering, deactivated-sublink message dropping (official `GetRouter`
  semantics).
* `crates/mojo-rs-interop/src/ipcz/link_memory.rs`: sublink allocation
  (shared `next_sublink_id`), `RouterLinkState` fragment allocation,
  `SetSideStable`/`TryLock`/`Unlock`/`ResetWaitingBit`/`read|write_link_status`
  with correct CAS-refresh semantics; regression tests for every CAS loop.
* `crates/mojo-rs-interop/src/ipcz/messages.rs`: `RouterDescriptor`
  (96-byte wire layout, flag byte at offset 64), `BypassPeer`,
  `AcceptBypassLink`, `StopProxying`, `ProxyWillStop` decode/encode; the
  portal-0 AcceptParcel golden test; `FragmentDescriptor::default()` now
  yields the null descriptor (`kInvalidBufferId`).
* `src/bin/routing-acceptor.rs` (new binary), wired into Cargo.toml.
* Oracle driver: `invite-broker-routing` / `invite-acceptor-routing` modes
  (portal transfer both directions + proxy bypass + local re-extraction +
  closure), logging-flags support, `RecvMessageWithHandle` diagnostics.
* `scripts/run_routing_court.sh` (new): baseline (oracle⇄oracle) vs interop
  (oracle⇄native) with byte-identical broker-event comparison and hashed
  manifest.
* `evidence/routing/WIRE-ANALYSIS.md`, `courts/curated/phase5-routing-bridge-bypass.md`.

### 2. Which files changed

See `git log --stat` for the cycle. Key files: the four ipcz modules above,
the new binary, `oracle/driver/oracle_driver.cc`, `scripts/run_routing_court.sh`,
`atlas/feature-matrix.json`, `STATUS.md`, `WORKLOG.md`.

### 3. Compatibility claims now supported

* Routing capability `Scaffolded` → `Oracle-compared` (feature-matrix).
* Portal transfer in both directions (WithLocalPeer deserialization; proxy
  serialization with central-link lock and `proxy_peer_*` recording).
* Proxy bypass completion (`StopProxying` final-length bookkeeping, proxy
  teardown) against the official broker's `BypassPeerWithNewLocalLink`.
* `BypassPeerWithLink` adoption (`AcceptBypassLink` semantics) with
  `StopProxyingToLocalPeer` reply and side-B stable marking.
* Closure propagation (`RouteClosed` with exact sequence lengths) and
  `TryLockForClosure` semantics.
* The shared-memory-service client handshake on portal 0 — byte-identical
  wire encoding (golden fixture).
* `RouterDescriptor` and `RouterLinkState` wire layouts verified against the
  pinned sources.

### 4. Which courts were run

* `scripts/run_routing_court.sh` — 4 consecutive PASS runs (byte-identical
  broker events; broker=0, native acceptor=0).
* `scripts/run_court.sh system` — 26/26 PASS (re-sealed after the changes).
* `scripts/run_interop_court.sh` — PASS.
* `scripts/run_invite_court.sh` — PASS.
* `cargo test --workspace` — 33 suites, 0 failures (28 in mojo-rs-interop,
  incl. the new CAS-loop and portal-0 golden tests).
* `cargo clippy --workspace` — 0 errors. `cargo fmt --check` — clean.
* `scripts/verify_no_oracle_dependency.sh` — PASS (2 binaries).

### 5. Exact pass/fail counts

Routing court: 4/4 PASS. System court: 26/26. Interop: 1/1. Invite: 1/1.
Workspace tests: 33 suites / 0 failures.

### 6. New residuals and evidence paths

`evidence/routing/<stamp>/` (baseline + interop events and wire captures),
`evidence/manifests/routing-<stamp>.json`, `evidence/routing/WIRE-ANALYSIS.md`,
`courts/curated/phase5-routing-bridge-bypass.md`. Failed runs preserved:
`20260805T054918Z`, `20260805T055902Z`, `20260805T060330Z` (forensic
receipts).

### 7. Every observed mismatch

1. `w1` arrives on the DECAYING sublink (the broker's local peer forwards its
   already-queued parcel over the decaying link), not on the central sublink.
2. The broker's `BypassPeerWithLink` arrives AFTER `w1`; the transfer-back
   must go out on the bootstrap router's migrated primary sublink.
3. `RouterLinkState` CAS loops never refreshed `expected` (Rust semantics) —
   spin-forever when the peer's bits were set.
4. The transfer-back's peripheral descriptor carried a non-null
   `new_link_state_fragment` — the broker's `Router::Deserialize` rejected it
   and tore down the NodeLink.
5. The oracle acceptor runs its own bridge-bypass chain; the native acceptor
   replies only to the broker's bypass (documented, permitted divergence).

### 8. Root cause of each fixed mismatch

1. `SerializeNewRouterWithLocalPeer` forwards the local peer's queued parcels
   over `new_decaying_sublink`; the court predicate and b1 sublink
   bookkeeping now cover both sublinks.
2. `MaybeStartBridgeBypass` fires on the broker's R_remote flush after the
   transfer transmission; the acceptor must drain and process the bypass
   before further bootstrap transmits.
3. `compare_exchange_weak` does not update `expected` (unlike the C++
   reference parameter); all four loops now refresh from `Err(actual)`.
4. The official `FragmentDescriptor` default constructor sets
   `buffer_id = kInvalidBufferId`; the Rust default now matches.
5. Not a defect — the bridge chain is a documented scope boundary; sealed by
   byte-identical broker events (same divergence as Phase 3).

### 9. Remaining unsupported behavior

Per the feature matrix: the acceptor-initiated bridge-bypass chain,
`BypassPeer`/`AcceptBypassLink` outbound, `RequestMemory`/`ProvideMemory`,
multi-subparcel/split parcels, node loss beyond the single link, multi-node
graphs, Phase 6 C ABI export, Phase 7 mojom/bindings, stress/fuzz, other
platforms.

### 10. Next highest-value parity gate

Per the directive's sequence: Phase 4 (data pipes and shared buffers) is
sealed; the routing seal continues with the acceptor-initiated bridge bypass
(the bridge chain `MaybeStartBridgeBypass`/`StartBridgeBypassFromLocalPeer`
machinery) to remove the one documented wire divergence — then
`RequestMemory`/`ProvideMemory` and multi-node graphs.

## Cycle 2026-08-05 — crates.io release v0.3.0 (Phase 5 routing surface)

### 1. What was actually implemented

* Lockstep workspace version bump 0.2.0 → 0.3.0 (all 14 crates; the path
  dependency specs in the umbrella `Cargo.toml` were bumped in lockstep).
* Published all 14 crates to crates.io at v0.3.0 in dependency order
  (platform → wire → mojom → casefile → codegen → core → c-api → io →
  bindings → system → oracle → test-support → interop → umbrella `mojo-rs`).
* Post-publish consumer smoke test: a scratch project depending on
  `mojo-rs = "0.3.0"` from the real registry (no path deps) built and ran,
  exercising the data-pipe round trip and shared-buffer cross-handle
  visibility through the umbrella re-exports.

### 2. Which files changed

`Cargo.toml`, `Cargo.lock`, `STATUS.md`, `WORKLOG.md`.

### 3. Compatibility claims now supported

None new (publication does not change sealed behavior); the published
artifacts now match the Phase 5 routing repository, including the
`mojo-rs-interop` `router`/`routing` modules and the `routing-acceptor`
binary.

### 4. Which courts were run

Post-publish smoke build/run (pass). The sealed courts are unchanged by
publication.

### 5. Exact pass/fail counts

14/14 publishes succeeded; smoke test pass; 0 failures.

### 6. New residuals and evidence paths

crates.io: `mojo-rs` 0.3.0 and all 13 members at 0.3.0.

### 7. Every observed mismatch

None.

### 8. Root cause of each fixed mismatch

n/a.

### 9. Remaining unsupported behavior

Unchanged: the remaining Phase 5 routing state machines, Phase 6 C ABI
export, Phase 7 mojom/bindings, concurrency/stress/fuzz, other platforms.

### 10. Next highest-value parity gate

The acceptor-initiated bridge bypass (the bridge chain
`MaybeStartBridgeBypass`/`StartBridgeBypassFromLocalPeer` machinery) to
remove the one documented routing wire divergence, then
`RequestMemory`/`ProvideMemory` and multi-node graphs.

---

## Cycle 2026-08-05 — Phase 5 routing seal: acceptor-initiated bridge bypass (divergence closed)

### 1. What was actually implemented

* **Bridge chain model** in the native routing acceptor
  (`crates/mojo-rs-interop/src/ipcz/router.rs`, `routing.rs`):
  - `LinkKind::Bridge`; `Link` now carries a `local_peer` (router identity)
    and a shared in-process `LocalLinkState` (`Rc<RefCell<>>`) mirroring the
    official `LocalRouterLink::SharedState` — local central links born
    `kStable`, local bridge links born `kUnstable`, with `TryLock` /
    `SetSideStable` / `Unlock` / `ResetWaitingBit` simulations of the pinned
    CAS loops;
  - `Router::bridge: Option<Edge>`; bridge parcel collection
    (`collect_bridge`), bridge decay (`finish_bridge_decay`), `bridge_link`
    capture, `finish_decays` now reports which edges decayed;
  - `MergeRoute`-equivalent bootstrap chain setup
    (`setup_bootstrap_bridge`): attachment ⟷ R_bridge (local central,
    `kStable`) ⟷ R_remote (local bridge, `kUnstable`) ⟷ [sublink 1];
  - `maybe_start_bridge_bypass` (all three cases: neither/one/both local
    outward peers, `TryLockForBypass` ordering and unlock-on-failure),
    `start_bridge_bypass_from_local_peer` (five-edge decay,
    `BypassPeerWithLink` outbound, local-peer adoption of the new central
    link as side A), bridge-aware `AcceptBypassLink` /
    `StopProxyingToLocalPeer` / `AcceptRouteClosureFrom`, the bridge-aware
    `Flush` (decay-gated `MarkSideStable`, dead-bridge closure forwarding,
    `MaybeStartBridgeBypass` precondition, `FlushOtherSideIfWaiting` tail
    gated on `dropped_last_decaying_link` / force), and local-link parcel /
    closure / disconnection delivery.
* **Timing fix for the bypass lock race**: the baseline oracle marks its
  initial links side-B stable only after the broker processes the Connect
  reply, so the broker's early bridge-bypass lock attempts defer and the
  acceptor wins the lock at the first parcel. The candidate marks the
  initial links stable at the same point in the exchange
  (`mark_initial_links_stable`, called when the transfer parcel arrives) and
  waits for the broker's own `BypassPeerWithLink(14→15)` before the
  transfer-back, reproducing the baseline's ordering deterministically.
* **Forensic tooling**: `wire-dump` binary decodes wire captures into a
  message inventory (message ids, sequence numbers, sublinks, fragment
  descriptors, payloads, `RouterDescriptor` fields).

### 2. Which files changed

`crates/mojo-rs-interop/src/ipcz/router.rs`,
`crates/mojo-rs-interop/src/ipcz/routing.rs`,
`crates/mojo-rs-interop/src/bin/wire-dump.rs` (new),
`evidence/routing/WIRE-ANALYSIS.md`, `courts/curated/phase5-routing-bridge-bypass.md`,
`atlas/feature-matrix.json`, `STATUS.md`, this log.

### 3. Compatibility claims now supported

The routing court's one documented wire divergence (the acceptor's own
bridge bypass) is **closed**: the native acceptor→broker wire matches the
baseline message-for-message (`BypassPeerWithLink(1→14)`,
`FlushRouter(14)`, `StopProxyingToLocalPeer(14, 0)`, `r1` on sublink 12,
`transfer-back` on sublink 15 with descriptor `new=16`), with only the
permitted normalizations (node names; inline-vs-fragment parcel data;
per-direction sequence numbers).

### 4. Which courts were run

`scripts/run_routing_court.sh` (5 consecutive passes, broker events
byte-identical), `scripts/run_court.sh system` (26/26), 
`scripts/run_interop_court.sh` (PASS), `scripts/run_invite_court.sh` (PASS),
`scripts/verify_no_oracle_dependency.sh` (PASS).

### 5. Exact pass/fail counts

Routing court: 5/5 PASS. System court: 26/26. Workspace tests: 34 suites,
0 failures (38 interop-lib tests, 8 new router state-machine tests).

### 6. New residuals and evidence paths

`evidence/routing/20260805T094338Z/`, `20260805T094359Z` (sealed
post-fix runs), `evidence/routing/WIRE-ANALYSIS.md` (updated).

### 7. Every observed mismatch

1. The shared sub-1 `RouterLinkState` status word was observed at `0xd`
   (side-A stable + both waiting bits, side-B stable absent) during the
   broker's concurrent Connect-reply/put processing. A reachability
   experiment (pinned `TryLock`/`SetSideStable`/`Unlock`/`ResetWaitingBit`
   against a real shared memfd) proved `0xd` unreachable via valid CAS
   sequences from the native's observed states — an unexplained broker-side
   artifact of the oracle build's threaded interleaving. The timing fix
   avoids the state entirely (side-B stable is marked after the broker's
   early attempts have deferred).
2. The transfer-back initially rode on the native's own bypass sublink (14)
   with the `StopProxyingToLocalPeer` reply emitted after it, because the
   single-threaded native did not process the broker's
   `BypassPeerWithLink(14→15)` before the application's puts (the oracle's
   IO thread does). Fixed by waiting for the broker's bypass before the
   transfer-back.

### 8. Root cause of each fixed mismatch

1. The native marked its initial links side-B stable at Connect time, which
   the broker's early bypass attempts could observe; the baseline oracle
   marks them later in the exchange. Fixed by moving the marks to the same
   point as the oracle's observable ordering.
2. The run() scenario is single-threaded; the oracle's routing is
   multi-threaded. The scenario now deterministically waits for the
   broker's bypass response to the `FlushRouter` it sent.

### 9. Remaining unsupported behavior

Per `STATUS.md`: `BypassPeer`/`AcceptBypassLink` outbound,
`RequestMemory`/`ProvideMemory`, multi-subparcel and split parcels,
multi-node graphs, node loss beyond the single link; Phase 6 C ABI export,
Phase 7 mojom/bindings, concurrency/stress/fuzz sealing, other platforms.

### 10. Next highest-value parity gate

Per the directive's sequence: the remaining Phase 5 routing work
(`RequestMemory`/`ProvideMemory`, then multi-node graphs) — or, per the
user's earlier direction, Phase 6 (C ABI export) after the routing thread.

## Cycle 2026-08-05 — Phase 5 memory court: parcel-fragment allocator seal + RequestMemory/ProvideMemory machinery

### 1. What was actually implemented

* **Generalized block allocator** (`crates/mojo-rs-interop/src/ipcz/link_memory.rs`):
  - the primary buffer's fixed block regions for 64/256/512/1k/2k/4k are now
    modeled (`PRIMARY_BLOCK_REGIONS`), with a CAS-based free-list pop
    (`BlockAllocator::Allocate` port: `expected` refreshed from the observed
    front header on every failed CAS) and push (`BlockAllocator::Free` port:
    LIFO head insertion) over the shared 4-byte `BlockHeader {version, next}`;
  - `try_allocate_fragment` (`AllocateFragment` port): ideal block size via
    `GetBlockSizeForFragmentSize` (`max(64, bit_ceil)`), `lower_bound` pool
    selection, active-allocator-first iteration (extras newest-first, then
    the primary region);
  - `AddBlockBuffer` support fixed on both sides: the receive side
    (`add_block_buffer`) now adopts the descriptor at its real size
    (`fstat`, via the new `SharedMemory::from_fd`) and registers the block
    size, instead of the previous latent bug of mapping `block_size` bytes;
    the send side (`register_block_buffer`) initializes the region
    (`InitializeRegion` port) before sharing;
  - `allocate_new_buffer_id` (shared-header atomic fetch-add),
    `get_total_block_capacity` / `can_expand_block_capacity`,
    `free_block`, and the general `write_parcel_fragment` (any block size,
    FragmentHeader release publish).
* **RequestMemory/ProvideMemory machinery** (`routing.rs`):
  - `pending_memory_requests` FIFO per buffer size (the official
    `pending_memory_requests_` map), `capacity_pending` (one in-flight
    request per block size, the `need_new_request` semantics),
    `request_block_capacity` (page-rounded buffer size), `on_provide_memory`
    (adopt → `AllocateNewBufferId` → transmit `AddBlockBuffer` BEFORE local
    registration, matching the official share-then-register order),
    `pending_fragments` (deferred parcels whose fragment buffer has not
    arrived — `WaitForParcelFragmentToResolve`), and `send_link_message_with_fds`;
  - new encoders (`encode_request_memory`, `encode_provide_memory`,
    `encode_add_block_buffer`, `encode_accept_parcel_full`) and a decoder fix
    for `ProvideMemory`'s driver object; a `MessageBuilder::append_driver_objects`
    fix for zero-byte objects (`driver_data_array = 0`, matching the mojo
    driver's memfd serialization with no data bytes);
  - the `ProvideMemory`/`RequestMemory` dispatch, and the `AddBlockBuffer`
    dispatch now resolves deferred parcels.
* **Parcel-fragment transmit** (`router.rs`, `routing.rs`): `Parcel` carries
  an optional fragment descriptor; `put` allocates the parcel data at put
  time based on the outward primary link (remote → shared-memory fragment,
  local/absent → inline), matching `Router::AllocateOutboundParcel`; the
  `encode_accept_parcel_arrays` fix (inline data array omitted when a
  fragment is present — the official encodes one or the other, never both).
* **Epoch discovery**: the pinned mojo embedder sets
  `IPCZ_MEMORY_FIXED_PARCEL_CAPACITY` (`mojo/core/ipcz_api.cc`, TODO
  crbug.com/40876289), so parcel-data-driven expansion is disabled; the
  native mirrors this (`write_parcel_fragment_or_inline` falls back to
  inline without lobbying).
* **The memory court**: `invite-broker-memory` / `invite-acceptor-memory`
  oracle-driver modes, the native `memory-acceptor` binary
  (`RoutingAcceptor::run_memory`), and `scripts/run_memory_court.sh` — the
  m0..m8 (200-byte parcels) / sync / transfer-back / w3 / m9-m10 scenario
  that seals the fragment allocator and the LIFO free-list reuse.

### 2. Which files changed

`crates/mojo-rs-platform/src/shm.rs`, `crates/mojo-rs-interop/src/ipcz/{link_memory,router,routing,messages,wire}.rs`,
`crates/mojo-rs-interop/src/ipcz/acceptor.rs` (AddBlockBuffer call-site fix),
`crates/mojo-rs-interop/src/bin/memory-acceptor.rs` (new),
`oracle/driver/oracle_driver.cc` + `oracle/patches/mojo-rs-oracle-driver.patch`
(new memory modes), `scripts/run_memory_court.sh` (new),
`courts/curated/phase5-memory-court.md` (new), `evidence/routing/WIRE-ANALYSIS.md`,
`atlas/feature-matrix.json`, `STATUS.md`, this log.

### 3. Compatibility claims now supported

The native memory acceptor's parcel-fragment allocation, 256-byte pool
exhaustion (inline fallback), and LIFO free-list reuse are byte-observable on
the acceptor→broker wire and match the oracle baseline **byte-for-byte**
(modulo node names and per-link sequence numbers): m0..m7 at offsets
96256..98048, m8 inline, sync at 1152, transfer-back at 1280, m9 at 98048 and
m10 at 97792. The broker's event streams are byte-identical. The
`RequestMemory`/`ProvideMemory`/`AddBlockBuffer` round trip is implemented
and unit-tested but not differentially triggered in this epoch (see §9).

### 4. Which courts were run

`scripts/run_memory_court.sh` (3+ consecutive PASS, wire-identical),
`scripts/run_routing_court.sh` (PASS, broker events byte-identical),
`scripts/run_court.sh system` (26/26), `scripts/verify_no_oracle_dependency.sh`
(PASS), `scripts/verify_storage_layout.sh` (PASS).

### 5. Exact pass/fail counts

Memory court: 3/3 PASS. Routing court: 1/1 (post-change). System court: 26/26.
Workspace tests: 155 passed, 0 failures (44 in mojo-rs-interop, including 6
new allocator tests). Clippy: 0 errors. fmt: clean.

### 6. New residuals and evidence paths

`evidence/memory/<stamp>/` (baseline + interop events and wire captures;
byte-identical broker streams; wire-identical acceptor→broker captures),
`evidence/manifests/memory-<stamp>.json`.

### 7. Every observed mismatch

1. The first memory-court interop run timed out: the transfer-back message
   was malformed (the inline data array was still appended when a fragment
   was present, shifting the handle-types/new-routers array offsets).
2. The initial m8-triggered-expansion court design was invalidated by the
   epoch: the mojo embedder disables parcel-data expansion, so the baseline
   produces NO `RequestMemory` traffic (m8 inline, m9/m10 reuse freed primary
   blocks). The court was redesigned around the actual behavior.
3. The `free_block` loop rejected freeing into an EMPTY free-list (front
   pointing at block 0) — the official `is_index_valid` covers index 0.
4. `try_allocate_fragment` tried the primary region before the extras; the
   official `BlockAllocatorPool::Allocate` tries the active (newest) buffer
   first.
5. A `ProvideMemory` without exactly one descriptor and a stale
   `can_expand_block_capacity` doc claim were corrected.

### 8. Root cause of each fixed mismatch

1. `encode_accept_parcel_arrays` computed `pd_off = 0` for a fragment-backed
   parcel but still appended the inline array; fixed by gating the append on
   the fragment being absent.
2. Empirical epoch behavior (the embedder's `IPCZ_MEMORY_FIXED_PARCEL_CAPACITY`)
   overrides the general ipcz semantics; the court now seals what the epoch
   actually does.
3. The free-list head can validly be block 0 (empty list); the freed block
   becomes the new head.
4. Pool iteration order follows the official active-first hint.
5. Dispatch validation and doc accuracy.

### 9. Remaining unsupported behavior

The `RequestMemory`/`ProvideMemory`/`AddBlockBuffer` round trip is
implemented and unit-tested but not oracle-compared: its only reachable
trigger in this epoch is `RouterLinkState` pool exhaustion (1484 blocks),
which requires the proxy-bypass machinery (`BypassPeer` outbound /
`AcceptBypassLink` inbound) — the next Phase 5 gate. Also remaining per
`STATUS.md`: `BypassPeer`/`AcceptBypassLink` outbound, multi-subparcel and
split parcels, multi-node graphs, node loss beyond the single link; Phase 6 C
ABI export, Phase 7 mojom/bindings, concurrency/stress/fuzz sealing, other
platforms.

### 10. Next highest-value parity gate

The proxy-bypass machinery (`BypassPeer` outbound / inbound `AcceptBypassLink`)
— the prerequisite for the `RouterLinkState`-exhaustion court that seals the
`RequestMemory`/`ProvideMemory` round trip differentially — then multi-node
graphs.

## Cycle 2026-08-05 — Phase 5 exhaustion court: AddBlockBuffer receive side + forensic fd-association fix

### 1. What was actually implemented

* **The exhaustion court** (`invite-broker-exhaust` / `invite-acceptor-exhaust`
  oracle-driver modes, the native `exhaust-acceptor` binary
  (`RoutingAcceptor::run_exhaust`), and `scripts/run_exhaust_court.sh`): 1486
  portal transfers through the bootstrap pipe (all pairs held) exhaust the
  primary buffer's 64-byte `RouterLinkState` pool mid-stream; the failing
  transfer falls back to the plain proxy path; the broker lobbies
  `RequestBlockCapacity(64)` (the unconditional `TryAllocateRouterLinkState`
  lobby), allocates a 64 KiB buffer locally, and shares it via
  `AddBlockBuffer`; the native adopts it and resolves the remaining transfers'
  link states from the new buffer (cross-buffer fragment resolution). The
  broker's IO thread flushes asynchronously, so the transfers arrive OUT OF
  route-sequence order and migrate across sublinks; the native reorders via
  its sequenced queue and verifies each payload against its route sequence
  number (`rseq 0` → `transfer-b1`, `rseq k` → `transfer-{k+1}`).
* **`router_deserialize` early-parcel fix**: parcels for the decaying sublink
  of a not-yet-deserialized router (e.g. the w1 arriving before the
  transfer-b1) were not drained; both the new and the decaying sublink's
  early parcels are now accepted when the router is established.
* **Forensic read-sizing fix** (`channel.rs`, `wire-relay.rs`): the native
  channel and the wire relay read with large fixed buffers, so a single
  `recvmsg` coalesced several messages and `SCM_RIGHTS` attached the
  descriptors to the read's first byte — associating a message's fd with the
  wrong message (observed as `BadDriverObjects` under the exhaustion court's
  dense traffic). Both now read exactly the bytes needed to complete the
  message at the front of the buffer, matching the official
  `ChannelPosix::OnFdReadable`'s `next_read_size`. The relay additionally
  forwards each complete message as one write. Covered by the new
  `fd_association_survives_dense_stream` channel test.

### 2. Which files changed

`crates/mojo-rs-interop/src/ipcz/{routing,channel}.rs`,
`crates/mojo-rs-interop/src/bin/{wire-relay,exhaust-acceptor}.rs`
(`exhaust-acceptor` new),
`oracle/driver/oracle_driver.cc` + `oracle/patches/mojo-rs-oracle-driver.patch`
(new exhaust modes), `scripts/run_exhaust_court.sh` (new),
`courts/curated/phase5-exhaust-court.md` (new),
`evidence/routing/WIRE-ANALYSIS.md`, `atlas/feature-matrix.json`,
`STATUS.md`, this log.

### 3. Compatibility claims now supported

The `AddBlockBuffer` receive side of the block-capacity expansion is sealed
against the official broker: the native adopts the broker's new 64-byte
block buffers (id 1 and, in the interop run, id 2), registers them, and
resolves the post-expansion transfers' `RouterLinkState` fragments from them
(cross-buffer fragment resolution). The broker's event streams (2979 events)
are byte-identical between the baseline and the interop run; both acceptors
deliver the complete route sequence (rseq 0..1485) and exit 0.

### 4. Which courts were run

`scripts/run_exhaust_court.sh` (3 consecutive PASS), `run_memory_court.sh`
(PASS, wire-identical), `run_routing_court.sh` (PASS),
`run_interop_court.sh` (PASS), `run_invite_court.sh` (PASS),
`run_court.sh system` (26/26), `verify_no_oracle_dependency.sh` (PASS).

### 5. Exact pass/fail counts

Exhaust court: 3/3 PASS. All other courts PASS. Workspace tests: 156 passed,
0 failures (45 in mojo-rs-interop, including the new channel fd-association
test). Clippy: 0 errors. fmt: clean.

### 6. New residuals and evidence paths

`evidence/exhaust/<stamp>/` (baseline + interop events and wire captures;
byte-identical broker streams), `evidence/manifests/exhaust-<stamp>.json`.

### 7. Every observed mismatch

1. The native's first exhaustion run failed on a payload mismatch: the
   transfers arrive out of route-sequence order (the broker's IO thread
   flushes asynchronously), so the first portal-bearing parcel was not
   transfer-b1.
2. `BadDriverObjects` under dense traffic: a transfer inherited the
   `AddBlockBuffer`'s memfd because the relay/channel reads coalesced several
   messages and `SCM_RIGHTS` attaches the descriptors to the read's first
   byte.
3. The relay treated an incomplete frame as fatal (`TruncatedMessage`).
4. Documented residual: the exhaustion point differs between the runs
   (baseline ~transfer 1330 with one `AddBlockBuffer`; interop ~transfer 750
   with two) — the native retains decayed `RouterLinkState` blocks (the
   free-timing boundary shared with the routing court's fragment-offset
   normalization).

### 8. Root cause of each fixed mismatch

1. Verification by arrival order is wrong; the native now verifies each
   payload against its route sequence number and lets the sequenced queue
   reorder.
2. Read-sizing (matching the official channel) fixes the fd-to-message
   association on both the relay and the native channel.
3. An incomplete frame is a deferral, not an error.
4. The native does not free a link's `RouterLinkState` when its decay
   completes; sealing that (a `RefCountedFragment` refcount model) is the
   next gate.

### 9. Remaining unsupported behavior

The `RequestMemory`/`ProvideMemory` send round trip (the acceptor as
allocation delegate) remains implemented-but-not-differentially-sealed: its
only reachable trigger in this epoch is `RouterLinkState` pool exhaustion on
the ACCEPTOR side, which requires the link-state free/refcount model and the
proxy-bypass machinery. Also remaining per `STATUS.md`: the link-state free
on decay, `BypassPeer`/`AcceptBypassLink` outbound, multi-subparcel and split
parcels, multi-node graphs, node loss beyond the single link; Phase 6 C ABI
export, Phase 7 mojom/bindings, concurrency/stress/fuzz sealing, other
platforms.

### 10. Next highest-value parity gate

The link-state free on decay (the `RefCountedFragment` refcount model for
`RouterLinkState` fragments) — closing the free-timing residual shared by the
routing and exhaustion courts — then the proxy-bypass machinery and multi-node
graphs.

## Cycle 2026-08-05 — Phase 5: RouterLinkState refcount lifecycle (link-state free on decay)

### 1. What was actually implemented

* **The `RefCountedFragment` refcount model** for shared `RouterLinkState`s
  (`crates/mojo-rs-interop/src/ipcz/link_memory.rs`, `routing.rs`):
  - `try_allocate_link_state` initializes the shared ref count (the first 4
    bytes of the fragment) to 1;
  - the native's own bridge bypass takes a second ref (`AddRemoteRouterLink`
    copies the `FragmentRef`, AddRef);
  - ADOPTION does NOT increment: `AdoptFragmentRefIfValid` takes the sender's
    `release()`d ref — the native's earlier AddRef-on-adoption inflated the
    counts;
  - link releases (decay completions in `finish_decays`, `remove_router`,
    closure) decrement via `release_link_state_ref` (acq-rel fetch-sub); the
    LAST ref frees the block back to the shared pool;
  - the fixed initial-portal states are unmanaged (`is_initial_link_state`:
    never refcounted or freed), matching `GetInitialRouterLinkState`'s
    `kUnmanagedRef`.
* **Corruption fix**: the `RefCountedFragment` ref-count word occupies the
  first 4 bytes of a `RouterLinkState` — the same word the
  `FragmentHeader.size` uses when the freed block is reused as a parcel
  fragment. The old code never released states, so the aliasing never fired;
  the first refcount implementation released the native's own transferred
  state (a ref it did not hold), and the broker's late release decremented
  the reused block's fragment size (the broker read "r" for "r1"). Fixed by
  the corrected model above.

### 2. Which files changed

`crates/mojo-rs-interop/src/ipcz/{link_memory,router,routing}.rs`,
`evidence/routing/WIRE-ANALYSIS.md`, `atlas/feature-matrix.json`,
`STATUS.md`, this log.

### 3. Compatibility claims now supported

The shared `RouterLinkState` lifecycle is sealed by the routing, memory, and
exhaustion courts (all PASS; the broker's event streams byte-identical): the
frees now happen at the official points (decay completions, router removal,
closure), and the block reuse is observable on the wire. The residual is
reduced from "states never freed" to a scheduler-dependent reuse ORDER (the
peer's IO-thread releases interleave differently — the routing court's r1
reuses block 1152 in the baseline but 1280 in the interop; the exhaustion
point differs and the interop can trigger a second AddBlockBuffer).

### 4. Which courts were run

`run_routing_court.sh` (PASS), `run_exhaust_court.sh` (PASS),
`run_memory_court.sh` (PASS, wire-identical), `run_interop_court.sh` (PASS),
`run_invite_court.sh` (PASS), `run_court.sh system` (26/26),
`verify_no_oracle_dependency.sh` (PASS).

### 5. Exact pass/fail counts

All courts PASS. Workspace tests: 156 passed, 0 failures. Clippy: 0 errors.
fmt: clean.

### 6. New residuals and evidence paths

None new (the residual is reduced; the evidence re-stamps under the same
paths).

### 7. Every observed mismatch

1. The first refcount implementation corrupted the routing court: the broker
   read the r1 fragment as "r" (size 1) — the broker's late `ReleaseRef`
   decremented the reused block's `FragmentHeader.size` (the shared word).
2. The native released the fixed initial-portal states (a decrement on an
   unmanaged state).
3. The native's adoption AddRef'd (inflating the counts to 3).

### 8. Root cause of each fixed mismatch

1. Adoption must NOT increment (`AdoptFragmentRefIfValid` takes the sender's
   released ref); the native's own transferred states must not be released by
   the native; the frees only happen at the LAST ref.
2. The fixed initial states are unmanaged (`kUnmanagedRef`, `memory == null`
   in `GenericFragmentRef::reset` — never decremented).
3. See 1.

### 9. Remaining unsupported behavior

The scheduler-dependent free-list reuse ORDER (the peer's IO-thread releases
interleave differently) is a documented normalization — the broker's event
stream is unaffected. Also remaining per `STATUS.md`: `BypassPeer`/
`AcceptBypassLink` outbound, the `RequestMemory`/`ProvideMemory` send round
trip (acceptor-side exhaustion), multi-subparcel and split parcels, multi-node
graphs, node loss beyond the single link; Phase 6 C ABI export, Phase 7
mojom/bindings, concurrency/stress/fuzz sealing, other platforms.

### 10. Next highest-value parity gate

The proxy-bypass machinery (`BypassPeer` outbound / inbound `AcceptBypassLink`)
and the acceptor-side `RouterLinkState` exhaustion (which triggers the
`RequestMemory`/`ProvideMemory` send round trip) — then multi-node graphs.

---

## Cycle 2026-08-05 — Phase 5 bypass court: RequestMemory/ProvideMemory/AddBlockBuffer SEND side + WithLocalPeer serialization

### 1. What was actually implemented

* **The WithLocalPeer SEND-side serialization** (`crates/mojo-rs-interop/src/ipcz/routing.rs`):
  `serialize_router` now implements the full `Router::SerializeNewRouter` —
  the local-peer detection, the `TryLockForBypass`, and the preferred
  `SerializeNewRouterWithLocalPeer` path (`serialize_router_with_local_peer`):
  a new central link with a fresh shared `RouterLinkState` (pool allocation
  ref + link ref) plus an adjacent decaying peripheral sublink
  (`proxy_already_bypassed=true`, `new_decaying_sublink = new_sublink + 1`),
  the local peer's outward edge released, the proxy's inward edge armed with
  a deferred decay; on pool exhaustion it fires the unconditional
  `TryAllocateRouterLinkState` lobby (`request_block_capacity` →
  `RequestMemory`) and falls back to the plain proxy path with the LOCAL
  outward link unlocked (the official `SerializeNewRouterAndConfigureProxy`
  behavior — the old code wrongly rolled proxy-bypass fields for local links).
  `begin_proxying` now handles the `proxy_already_bypassed` case
  (`BeginProxyingToNewRouter`): the proxy's outward edge releases its old
  local link, the inward edge adopts the decaying peripheral link, the local
  peer adopts the new central link (`SetOutwardLink`, side-A stable mark);
  the fresh-pair proxy decays and drops in the same flush.
* **`open_portals`**: local pair creation (local central link, born stable),
  mirroring `OpenPortals`.
* **`run_bypass`** + the `bypass-acceptor` binary: the acceptor-initiated
  block-capacity exhaustion scenario (1520 WithLocalPeer transfers, the
  `RequestMemory` → `ProvideMemory` → `AddBlockBuffer` SEND round trip, the
  `sync` marker, closure).
* **Oracle driver modes** `invite-broker-bypass` / `invite-acceptor-bypass`
  (`oracle/driver/oracle_driver.cc`; patch regenerated): the routing-court
  prelude + 1520 acceptor-initiated transfers + sync + close.
* **The mojo-driver SharedBuffer object encoding** (`messages.rs`):
  `encode_shared_buffer_object` (40 bytes: `ObjectHeader{size=8,
  type=kSharedBuffer=1}` + `BufferHeader{size=32, buffer_size, mode=kUnsafe=2,
  padding=0, guid_low, guid_high}`) used by `encode_add_block_buffer` /
  `encode_provide_memory`, with a deterministic non-zero GUID.
* **`scripts/run_bypass_court.sh`** + the casefile
  `courts/curated/phase5-bypass-court.md`; the golden fixture
  `testdata/ipcz/broker-add-block-buffer.bin` (the official broker's
  `AddBlockBuffer`, extracted from the exhaustion baseline capture).

### 2. Which files changed

`crates/mojo-rs-interop/src/ipcz/{routing,messages,link_memory}.rs`,
`crates/mojo-rs-interop/src/bin/bypass-acceptor.rs` (new),
`crates/mojo-rs-interop/testdata/ipcz/broker-add-block-buffer.bin` (new),
`oracle/driver/oracle_driver.cc`, `oracle/patches/mojo-rs-oracle-driver.patch`,
`scripts/run_bypass_court.sh` (new), `courts/curated/phase5-bypass-court.md`
(new), `atlas/feature-matrix.json`, `STATUS.md`, `evidence/bypass/`, this log.

### 3. Compatibility claims now supported

The **SEND side of the block-capacity expansion** is sealed: the native
acceptor's `RequestMemory` → the broker's `ProvideMemory` → the native's
`AddBlockBuffer` round trip, triggered by the acceptor exhausting its own
shared 64-byte `RouterLinkState` pool via WithLocalPeer transfers. The
broker's event stream is BYTE-IDENTICAL between the oracle-acceptor baseline
and the native; both wires carry all 1520 transfers + sync; the send-side
round trip is visible on the acceptor→broker wire in both runs; both
acceptors exit 0. 5+ consecutive passes.

### 4. Which courts were run

`run_bypass_court.sh` (PASS ×5), `run_routing_court.sh` (PASS),
`run_memory_court.sh` (PASS, wire-identical), `run_exhaust_court.sh` (PASS),
`run_court.sh system` (26/26), `run_interop_court.sh` (PASS),
`run_invite_court.sh` (PASS), `verify_no_oracle_dependency.sh` (PASS),
`verify_storage_layout.sh` (PASS).

### 5. Exact pass/fail counts

All courts PASS. Workspace tests: 161 passed, 0 failures (5 new: 3 routing
state-machine tests + 2 message-encoding tests). Clippy: 0 errors. fmt: clean.

### 6. New residuals and evidence paths

* The exhaustion POINT differs between the runs (the oracle's concurrent IO
  thread frees payload fragments — net ~1 block/transfer; the native's
  single-threaded loop does not wait — net ~2/transfer). Internal block
  accounting, normalized; the broker's event stream is unaffected. See the
  casefile.
* Evidence: `evidence/bypass/<stamp>/` (events, wire), the manifests
  `evidence/manifests/bypass-*.json` (the first, `20260805T185330Z`, is a
  FAIL receipt — the empty-driver-object bug), the casefile
  `courts/curated/phase5-bypass-court.md`.

### 7. Every observed mismatch

1. The first court run FAILED: the native died at transfer ~1466 with
   `Connection reset by peer`; the broker saw `PEER_CLOSED` on the bootstrap
   pipe mid-transfer and exited 1. The broker's NodeLink had dropped.
2. The interop wire carried only 1469 transfers; the baseline (oracle
   acceptor) passed completely (1520 transfers, RequestMemory + AddBlockBuffer
   on the wire).

### 8. Root cause of each fixed mismatch

1+2. The native's `encode_add_block_buffer`/`encode_provide_memory` attached
   an EMPTY driver object (no serialized data). The mojo driver's
   `SharedBuffer::Deserialize` (`mojo/core/ipcz_driver/shared_buffer.cc`)
   requires the 40-byte serialization; an empty object fails deserialization,
   `OnAddBlockBuffer` returns false, and the official broker drops the
   NodeLink (all routes close). This path had never been differentially
   exercised (the exhaustion court sealed only the RECEIVE side). Fixed by
   encoding the full SharedBuffer object; pinned byte-identical to the
   official broker's capture by the golden test.

### 9. Remaining unsupported behavior

Per `STATUS.md`: `BypassPeer`/`AcceptBypassLink` OUTBOUND (only reachable in
3-node graphs — in 2 nodes the broker's `MaybeStartSelfBypass` always takes
`StartSelfBypassToLocalPeer`), multi-node graphs (message ids 2–13), node loss
beyond the single link, multi-subparcel/split parcels; Phase 6 C ABI export,
Phase 7 mojom/bindings, stress/fuzz, other platforms.

### 10. Next highest-value parity gate

**Multi-node graphs** (message ids 2–13: `ReferNonBroker`,
`ConnectToReferredBroker/NonBroker`, `RequestIntroduction`,
`AcceptIntroduction`, `EstablishLink`, and the broker-to-broker connect) —
which is where the native-outbound `BypassPeer`/`AcceptBypassLink` via
`EstablishLink` becomes reachable (the remaining Phase 5 routing work), then
Phase 6 (C ABI export) per the directive sequence.

## Cycle 2026-08-05 — Phase 5 multi-node referral court: broker + referrer A + referred B (outbound AcceptBypassLink)

### 1. What was actually implemented

* The multi-link NodeLink refactor of the routing core (link-scoped state):
  `owners`/`early_parcels` keyed by `(link_id, sublink)`; `link_id` threaded
  through `dispatch`, `recv_until_on`, `process_accept_parcel`,
  `on_bypass_peer_with_link`, `serialize_router`,
  `serialize_router_with_local_peer`, `begin_proxying`, and the
  `transmit_parcel` serialization chain; `remove_router` sweeps
  `(link_id, sublink)` pairs from the router's edges. The sealed 2-node
  courts re-passed unchanged after the refactor.
* The wire layer for the multi-node message ids 2–13 (`ReferNonBroker`,
  `ConnectToReferredBroker/NonBroker`, `NonBrokerReferralAccepted/Rejected`,
  `ConnectFromBrokerToBroker`, `RequestIntroduction`/`AcceptIntroduction`/
  `RejectIntroduction`, `RequestIndirectIntroduction`) with exact V0 layouts,
  the mojo `Transport` object encoding, and wire-dump describe arms.
* The oracle driver's three 3-node modes (`invite-broker-3node`,
  `invite-node-a-3node`, `invite-node-b-3node` with
  `MOJO_ACCEPT_INVITATION_FLAG_INHERIT_BROKER`) and the baseline 3-node wire
  capture.
* `RoutingAcceptor::run_3node` + the `3node-acceptor` binary: the referred
  node B — `ConnectToReferredBroker` greeting (raw transmit, no NodeLink
  sequence), `ConnectToReferredNonBroker` acceptance (broker link + referrer
  link adoption), `EstablishWaitingRouters` on the referrer link with the
  shared-memory client and the bootstrap attachment bridge, the b2a portal
  re-transfer, the outbound `BypassPeer` → `BypassPeerWithNewRemoteLink` →
  `AcceptBypassLink`, the hello/world round trip, `ProxyWillStop`,
  `RouteClosed` peer closure, and the local closes.
* `scripts/run_3node_court.sh` — the 3-node differential court with two
  wire relays (broker↔A and the referral transport) and structural wire
  comparison (node-name GUIDs normalized).

### 2. Which files changed

`crates/mojo-rs-interop/src/ipcz/{routing,router,messages,acceptor}.rs`,
`crates/mojo-rs-interop/src/bin/{3node-acceptor,wire-dump,wire-relay}.rs`,
`oracle/driver/oracle_driver.cc` + `oracle/patches/mojo-rs-oracle-driver.patch`,
`scripts/run_3node_court.sh`, `courts/curated/phase5-multinode-court.md`,
`STATUS.md`, `atlas/feature-matrix.json`, `WORKLOG.md`.

### 3. Compatibility claims now supported

The native B is wire-indistinguishable from the official B on every captured
link: the broker's AND A's event streams are byte-identical; all four wire
directions are structurally identical (modulo node-name GUIDs); the outbound
`AcceptBypassLink` (id 31) — previously unreachable in 2-node graphs — is
sealed. All sealed courts continue to pass (system 26/26 byte-identical,
routing, memory, exhaust, interop, invite, bypass).

### 4. Courts run

`run_court.sh system` (26/26), `run_routing_court.sh`, `run_memory_court.sh`,
`run_exhaust_court.sh`, `run_interop_court.sh`, `run_invite_court.sh`,
`run_bypass_court.sh`, `run_3node_court.sh` — all PASS; `cargo test
--workspace` 161 passed / 0 failed.

### 5–8. Residuals, mismatches, root causes, fixes

1. **Multi-link refactor build break**: the `dispatch`/`process_accept_parcel`/
   `begin_proxying`/serialization call sites had stale arity and the owners
   map keying was half-migrated; fixed mechanically and re-verified.
2. **Court harness relay topology**: the relays were initially connected to
   the SAME sockets the processes held, so the process traffic never passed
   through the relays (the referral transport flooded with ~24M repeated
   `ConnectToReferredBroker` greetings and the broker↔A link broke). Fixed by
   giving each relay two dedicated socketpairs.
3. **Outbound bypass owners registration**: `bypass_peer_with_new_remote_link`
   never registered `owners[(target link, new sublink)]` (the inbound
   `BypassPeerWithLink` path did), so the broker's parcels on the new central
   sublink would have been misclassified; fixed.
4. **Per-link fragment resolution**: `parcel_data` and the deferred-fragment
   queue were hard-coded to the broker link memory; each NodeLink has its own
   primary buffer with its own buffer id 0, so referrer-link parcels must
   resolve against the referrer memory; the queue is now keyed by
   `(link, buffer)`.
5. **`ProxyWillStop` (id 33)**: previously rejected as unsupported; the
   multi-node court exercises it (the broker's response to B's
   `AcceptBypassLink`); `router_proxy_will_stop` implemented.
6. **Greeting sequence number**: the official connector's greeting is a raw
   transport transmit (seq stays 0) and does not advance the NodeLink
   counter; the native must not consume a sequence for it.
7. **Wire-relay EPIPE**: B's teardown `RouteClosed` arrives after the broker
   exits; the relay now tolerates EPIPE/ECONNRESET on forward (the capture is
   already written).
8. **wire-dump name arms**: adding the multi-node names accidentally dropped
   the `RequestMemory`/`ProvideMemory` arms, which broke the bypass court's
   wire detection; restored.

### 9. Remaining unsupported behavior

Per `STATUS.md`: `BypassPeer` outbound over a newly `EstablishLink`-ed link
(the 3-node court seals the already-linked case), `BypassPeerWithNewLocalLink`,
the broker/referrer-side referral roles, introductions (ids 10–13),
broker-to-broker connect, node loss beyond a single link, multi-node graphs
beyond one referral, split/multi-subparcel parcels; Phase 6 C ABI export,
Phase 7 mojom/bindings, stress/fuzz, other platforms.

### 10. Next highest-value parity gate

Per the directive sequence, Phase 5's remaining routing work: the native
broker/referrer roles and the introduction machinery (ids 10–13), or the
native broker side of a referral (Rust referrer A → C++ oracle B), which
reuses the entire wire layer. After that the directive sequence continues at
Phase 6 (C ABI export).

## Cycle 2026-08-05 — Phase 5 multi-node referral court: the native referrer A (second mixed-language pairing)

### 1. What was actually implemented

* The referrer role (`RoutingAcceptor::run_3node_a` + the `3node-referrer`
  binary): the native node A — the Connect handshake WITHOUT the
  shared-memory client send (the official referrer transmits `ReferNonBroker`
  first), the `ReferNonBroker` send with the 16-byte `Transport` object,
  `NonBrokerReferralAccepted` acceptance (A↔B link adoption,
  `EstablishWaitingRouters` on the referrer link, the service portal 0 and the
  a2b attachment bridge on portal 1), the transfer-y receipt, the re-transfer
  with `proxy_peer_node_name` = the broker, the un-consumed `hello`
  forwarding, and the acceptor-side bootstrap bridge bypass with the
  waiting-bit wakeup.
* `scripts/run_3node_court.sh` now runs THREE configurations: baseline (all
  official), interop-b (native B), and interop-a (native A), comparing the
  broker's and the counterpart node's event streams byte-identically plus the
  structural wire comparison for both pairings.

### 2. Which files changed

`crates/mojo-rs-interop/src/ipcz/{routing,router,messages,link_memory}.rs`,
`crates/mojo-rs-interop/src/bin/{3node-referrer,wire-relay,wire-dump}.rs`,
`scripts/run_3node_court.sh`, `courts/curated/phase5-multinode-court.md`,
`STATUS.md`, `atlas/feature-matrix.json`, `WORKLOG.md`.

### 3. Compatibility claims now supported

Both mixed-language pairings of the multi-node referral court are sealed:
the native B is wire-indistinguishable from the official B, and the native A
from the official A. The broker's and the counterpart node's event streams
are byte-identical in both pairings; all four captured wire directions are
structurally identical. All sealed courts continue to pass (system 26/26,
routing, memory, exhaust, interop, invite, bypass, 3node); 161 workspace
tests pass.

### 4. Courts run

`run_court.sh system` (26/26), `run_routing_court.sh`, `run_memory_court.sh`,
`run_exhaust_court.sh`, `run_interop_court.sh`, `run_invite_court.sh`,
`run_bypass_court.sh`, `run_3node_court.sh` (both pairings) — all PASS.

### 5–8. Residuals, mismatches, root causes, fixes

1. **`encode_transport_object` 20 vs 16 bytes**: a spurious
   `TransportHeader.size` field made the `ReferNonBroker` transport object
   unreadable (the broker's `OnReferNonBroker` silently dropped the referral);
   the official `TransportHeader` is `{destination_type u32, 4 flag bytes}`.
2. **Referrer send order**: the official referrer transmits `ReferNonBroker`
   (seq 0) BEFORE the shared-memory client handshake (seq 1); the Connect
   handshake for the referrer skips the client send.
3. **Bypass lock source**: `serialize_router` recorded only the low half of
   the broker's name in `allowed_bypass_request_source`; the broker's
   `CanNodeRequestBypass` compares the full 16-byte NodeName, so the referred
   node's `AcceptBypassLink` would be rejected. The lock now records the
   serialization TARGET's full name (`remote_name_for(link_id)`), and
   `write_allowed_bypass_source`/`read_allowed_bypass_source` handle the full
   NodeName.
4. **Per-link memory scope**: `put`'s payload-fragment allocation and
   `serialize_router`'s sublink allocation used the broker memory; the a2b
   chain's collapse made the attachment's outward remote, so the transfer-y2b
   fragment was allocated from the wrong buffer (B received an empty payload).
   Both now use the target link's memory; `transmit_parcel` sends on the
   link's own channel.
5. **The un-consumed `hello`**: the forwarded parcel was delivered to the
   terminal portal and lost when Y' became a proxy; the official's app never
   reads it. The keep-queued flush (a `process_accept_parcel_keep_queued`
   variant) leaves it in the route's queue, so the descriptor's
   `next_incoming_sequence_number` stays at the unconsumed count and the
   proxy forwards it.
6. **`GetCurrentSequenceLength`**: the native counted only the popped span;
   the official advances at append (contiguous pushed span). The decay checks
   (and the side-B stable marks that unblock the re-transfer's bypass lock)
   depend on it.
7. **The bootstrap bridge bypass / closure**: the native A's bypass flush
   could not fire the waiting-bit wakeup (no decay completes in it), so the
   broker's second-stage bypass stalled and the pipe_a closure never arrived;
   a forced attachment flush after the stable marks sends the `FlushRouter`
   wakeup. The closure predicate is evaluated per message because the bypass
   migrates the attachment's sublink.
8. **`bootstrap_rid` overwrite**: the a2b bridge setup reassigned the shared
   `bootstrap_rid`; the referrer saves/restores the pipe_a attachment identity.

### 9. Remaining unsupported behavior

Per `STATUS.md`: `BypassPeer` outbound over a newly `EstablishLink`-ed link,
`BypassPeerWithNewLocalLink`, the broker-side referral roles, introductions
(ids 10–13), broker-to-broker connect, node loss beyond a single link,
multi-node graphs beyond one referral, split/multi-subparcel parcels; Phase 6
C ABI export, Phase 7 mojom/bindings, stress/fuzz, other platforms.

### 10. Next highest-value parity gate

The introduction machinery (ids 10–13: `RequestIntroduction`,
`AcceptIntroduction`, `RejectIntroduction`, `RequestIndirectIntroduction`) —
the `EstablishLink` path for `BypassPeer` when the bypass target has no direct
link — and the broker-side referral roles. After that the directive sequence
continues at Phase 6 (C ABI export).
## Cycle 2026-08-06 — Phase 5 introduction court: broker + referrer A + referred B + introduced C (ids 10–13), both mixed-language pairings

### 1. What was actually implemented

* The wire layer for the introduction machinery: `encode_accept_introduction`
  (packed V0 with single-byte `link_side`/`remote_node_type` at offset 16,
  `remote_protocol_version` at 20, transport + memory driver objects, and the
  V1 `remote_features` OFFSET field — the first attempt wrote the bitfield
  value directly, producing an unreadable capture), `encode_reject_introduction`,
  `encode_request_indirect_introduction`, and the fixed `AcceptIntroduction`
  decode (the old decoder read the two single-byte fields as u32s at the wrong
  offsets — the forensic wire dump showed `link_side=257`).
* Golden wire tests pinned to the official 4-node captures
  (`testdata/ipcz/4node-*.bin`): accept/decode/encode and request-encode tests
  byte-identical to the official broker's capture (16/16).
* The core multi-link generalization of `RoutingAcceptor`: the
  `links: HashMap<u64, NodeLinkState>` map (link 0 = broker, 1 = direct, 2 =
  introduced), per-link memory scoping for the stable marks, closure locks,
  bypass locks, and the memory-request machinery, and per-link sublink
  allocation.
* `router_bypass_peer` with the no-direct-link fallback to `establish_link`,
  the `pending_introductions` queue, `on_accept_introduction` (transport +
  buffer adoption, the introduced link registered with its remote name),
  `on_accept_bypass_link` with the `CanNodeRequestBypass` check against the
  shared `allowed_bypass_request_source`, and the closure tolerance for
  double `RouteClosed` (the `AcceptRouteClosureFrom` behavior).
* `RoutingAcceptor::run_4node_c` + `4node-acceptor` (native C) and
  `RoutingAcceptor::run_4node_a` + `4node-referrer` (native A), mirroring the
  oracle driver's `RunFourNodeC`/`RunFourNodeA` event ops.
* The oracle driver's four 4-node modes (`invite-broker-4node`,
  `invite-node-a-4node`, `invite-node-b-4node`, `invite-node-c-4node`) and the
  all-official baseline capture (preserved at `evidence/4node/probe/`).
* `scripts/run_4node_court.sh` — the 4-node differential court: baseline
  (all official) + interop-c (native C) + interop-a (native A), all three
  broker links relayed, byte-identical event comparisons per pairing, and
  structural wire comparison with the documented pipe_a-bypass-race
  normalization.
* `NodeName` gained `Hash` (the `pending_introductions` map key).

### 2. Files changed

`oracle/driver/oracle_driver.cc` (+ 4-node modes, working-tree addition on top
of the pinned oracle driver patch), `oracle/patches/mojo-rs-oracle-driver.patch`,
`crates/mojo-rs-interop/src/ipcz/messages.rs`, `crates/mojo-rs-interop/src/ipcz/routing.rs`,
`crates/mojo-rs-interop/src/bin/4node-acceptor.rs` (new),
`crates/mojo-rs-interop/src/bin/4node-referrer.rs` (new),
`crates/mojo-rs-interop/testdata/ipcz/4node-*.bin` (new fixtures),
`scripts/run_4node_court.sh` (new), `STATUS.md`, `WORKLOG.md`,
`atlas/feature-matrix.json`, `courts/curated/phase5-multinode-court.md`.

### 3. Compatibility claims now supported

* `RequestIntroduction` (id 10) send; `AcceptIntroduction` (id 11) receive +
  transport/buffer adoption (packed V0 wire-identical to the official);
  `AcceptBypassLink` (id 31) over an introduced link; `EstablishLink` →
  `BypassPeerWithNewRemoteLink` when the bypass target has no direct link;
  multi-link graphs (broker + direct + introduced links with per-link memory
  and sublink id spaces); the WithLocalPeer transfer over a direct link with
  the bridge self-bypass migrating the route's sublink; the introduction
  round trip over the new link in both mixed-language pairings.

### 4. Courts run

`run_4node_court.sh` (10+ consecutive passes), `run_3node_court.sh` (5/5),
`run_bypass_court.sh`, `run_exhaust_court.sh`, `run_memory_court.sh`,
`run_routing_court.sh`, `run_interop_court.sh`, `run_invite_court.sh`,
`run_court.sh system`, `cargo test --workspace` (164 passed / 0 failed),
`cargo fmt --check` (pass), clippy (no new warnings).

### 5. Exact pass/fail counts

4-node court: pass (broker/A/B/C event streams byte-identical in both
pairings; wire comparisons pass with the documented normalization; native
nodes exit 0). 3-node court: pass (re-sealed after the regression fixes).
All other sealed courts re-pass. Workspace tests: 164/164.

### 6. New residuals and evidence paths

`evidence/4node/<stamp>/` (twelve event streams, eighteen wire captures,
receipt) + `evidence/manifests/4node-<stamp>.json`;
`evidence/3node/<stamp>/` (re-seal); `crates/mojo-rs-interop/testdata/ipcz/4node-*.bin`.

### 7. Every observed mismatch

1. The transfer-y2 parcel arrived on sublink 12, not the hard-coded
   BOOTSTRAP_SUBLINK (1): the c2b route's bridge self-bypass migrates the
   route before the transfer arrives. Same for the 3-node b2a route.
2. The 3-node interop-a regression: the referrer's proxy-peer descriptor
   named `{0,0}` (invalid), so the referred node never bypassed and the
   world round trip stalled.
3. The pipe_a bridge-bypass stage-1 lock race: the all-official baseline
   itself produces both variants (A-initiated or broker-initiated) with
   byte-identical events.
4. The "world" reply sometimes arrived over the a2b link through B's proxy
   (C replied before its bypass settled); the native's step-4 predicate
   wrongly required the introduced link.
5. Teardown closes hit EPIPE when a peer had already exited.

### 8. Root cause of each fixed mismatch

1. The transfer sublink is dynamic (the bridge self-bypass allocation); the
   predicate now matches the PORTAL handle type on any sublink.
2. The multi-link refactor's `remote_name_for` reads `links[id].remote_name`,
   but the referrer's `connect_handshake` never set the broker link's
   `remote_name` (the old code returned `self.broker_name`); fixed by
   recording `connect.broker_name` in the link state.
3. Genuine official nondeterminism (the shared `RouterLinkState` CAS): the
   court normalizes the exchange on the broker↔A directions (documented in
   the runner and receipt); the fixed-point done/closure traffic is still
   compared exactly. The candidate itself is deterministic: the stage-1 win
   is forced by waiting for the broker's side-A stable bit to be observable
   in the shared memory before marking (the oracle acceptor wins only
   because its side-B stable lands after the broker's ConnectReply
   processing).
4. The official `RecvPayload` is a link-agnostic Mojo read; the candidate
   now accepts the no-handle reply on whichever link delivers it.
5. `CloseRoute` on a broken transport is not an error at teardown (the
   official driver's `Transmit` failure is asynchronous).

### 9. Remaining unsupported behavior

`BypassPeerWithNewLocalLink`, the broker-side referral roles
(`NodeConnectorForBrokerReferral`, a native broker serving referrals and
introductions), `RejectIntroduction`/`RequestIndirectIntroduction` (ids 12–13)
and broker-to-broker connect (id 7), node loss beyond a single link, graphs
beyond a single referral chain, split/multi-subparcel parcels; Phase 6 C ABI
export, Phase 7 mojom/bindings, stress/fuzz, other platforms.

### 10. Next highest-value parity gate

The broker-side roles (the native as broker: `ReferNonBroker` receive,
`NonBrokerReferralAccepted` send, `HandleIntroductionRequest` /
`IntroduceRemoteNodes`, `ConnectFromBrokerToBroker`) — the mirrored half of
the referral/introduction state machines, sealed by a court with the native
broker serving official nodes; after that the directive sequence continues at
Phase 6 (C ABI export).
