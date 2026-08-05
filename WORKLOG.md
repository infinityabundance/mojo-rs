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
