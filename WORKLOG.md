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
