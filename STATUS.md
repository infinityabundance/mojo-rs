# mojo-rs — Implementation Status

Status as of: 2026-08-05 (epoch 1, Chromium 151.0.7922.105, commit
bfa3579138998e2fbb981725570fa588c5b6f8cd, CoreIpcz architecture).

The capability ladder (atlas/feature-matrix.json) is authoritative. A status
below is a CLAIM only when the cited evidence exists and verifies.

## Distribution

* Repository: <https://github.com/infinityabundance/mojo-rs> (public, `main`).
* Crates.io: `mojo-rs` umbrella + 13 `mojo-rs-*` crates, all v0.3.0
  (published 2026-08-05, lockstep bump after the Phase 5 routing seal). The
  umbrella re-exports the runtime and language-toolchain crates; forensic
  tooling is published separately.
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

### Phase 5 routing seal — portal transfer in both directions + proxy bypass + acceptor-initiated bridge bypass

The routing court (`scripts/run_routing_court.sh`) runs the official broker
(`invite-broker-routing`) against the official oracle acceptor (baseline) and
against the native Rust `routing-acceptor` (interop), both through the
wire-relay man-in-the-middle:

1. the broker creates a message pipe `(A, B)` and sends `B` through the
   bootstrap pipe (the `SerializeNewRouterWithLocalPeer` path — the new
   router is created on the acceptor with a central link plus a decaying
   peripheral link and a shared `RouterLinkState`);
2. the broker writes `w1` on `A` — routed over the wire (via the decaying
   link, because the parcel was queued when the pair was split);
3. the acceptor writes `r1` on `B'` — routed back to the broker's `A`;
4. the acceptor sends `B'` back through the bootstrap pipe (the
   `SerializeNewRouterAndConfigureProxy` path — locking the central link,
   recording the proxy peer, and leaving a proxy behind); the broker
   deserializes `B''`, completes the bypass with a new local link
   (`BypassPeerWithNewLocalLink`), and sends `StopProxying` to the acceptor's
   proxy;
5. the broker writes `w2` on `A` — delivered locally to `B''`;
6. the broker closes `A`, `B''`, and the bootstrap pipe — closure
   propagates (`RouteClosed`).

The broker's event stream is BYTE-IDENTICAL between the baseline and the
interop run (15 events: invitation, transfer, `w1`, `r1`, `transfer-back`
with one extracted handle, `w2`, closes, lifecycle); both processes exit 0.

**The bridge-bypass divergence is closed.** The bootstrap pipe is backed by
an ipcz *bridge chain* (`P1 ⟷ attachment ⟷ R_bridge ⟷ R_remote ⟷ broker`),
so both ends independently run bridge bypass. The native routing acceptor
now models the full bridge chain and its state machines, and the
acceptor→broker wire matches the baseline message-for-message:
`BypassPeerWithLink(1→14)`, `FlushRouter(14)`, `StopProxyingToLocalPeer(14, 0)`,
`r1` on sublink 12, and the `transfer-back` on sublink 15 with descriptor
`{new 16, proxy_peer_sublink 12}`. Permitted normalizations only: node
names, inline-vs-fragment parcel data (both decode identically), and
per-direction link sequence numbers. See
`evidence/routing/WIRE-ANALYSIS.md` and
`courts/curated/phase5-routing-bridge-bypass.md`.

The native routing acceptor (`crates/mojo-rs-interop/src/ipcz/{router,routing}.rs`)
implements the non-broker ipcz `Router` state machine against the pinned
sources: terminal/proxy routers, decaying links with sequence-length bounds,
sequenced parcel queues, `Router::Deserialize` (including the
`proxy_already_bypassed` setup), `SerializeNewRouterAndConfigureProxy` +
`BeginProxyingToNewRouter`, `AcceptBypassLink` semantics for the broker's
`BypassPeerWithLink`, `StopProxying` teardown, `RouteClosed` propagation,
`MergeRoute` (local central links born `kStable`, local bridge links born
`kUnstable`), the bridge-aware `Flush` / `StopProxyingToLocalPeer` /
`AcceptRouteClosureFrom`, and `MaybeStartBridgeBypass` /
`StartBridgeBypassFromLocalPeer` (all three bypass cases), plus the shared
`RouterLinkState` compare-exchange loops (`TryLock`/`SetSideStable`/
`Unlock`/`ResetWaitingBit`, each verified against `router_link_state.cc`
and with regression tests, including the in-process local-link state).
Also sealed: the shared-memory-service client handshake on the internal
portal 0 (byte-exact against the golden fixture) and the `RouterDescriptor`
wire layout (96 bytes; `proxy_already_bypassed`/`peer_closed` flag byte at
offset 64).

Real bugs found and fixed during this cycle (each preserved in
`evidence/routing/`):
- the `RouterLinkState` compare-exchange loops never refreshed `expected`
  from the CAS result (Rust's CAS does not update the argument, unlike C++'s
  reference parameter) — would spin forever when the peer had set bits;
- the transfer-back descriptor carried a non-null `new_link_state_fragment`
  (`FragmentDescriptor::default()` was `{0,0,0}` instead of
  `{kInvalidBufferId,0,0}`) — the broker's `Router::Deserialize` rejected the
  peripheral link and tore down the NodeLink;
- `w1` arrives on the decaying sublink (the broker's local peer forwards its
  queued parcel over the decaying link), not on the central sublink — the
  court's receive predicate and the sublink bookkeeping now handle both;
- the bootstrap router's primary sublink migrates on the broker's bypass —
  the transfer-back and the `RouteClosed` wait must follow the current
  primary sublink;
- the bridge bypass lock race: the baseline oracle marks its initial links
  stable only after the broker processes the Connect reply, so the broker's
  early bridge-bypass attempts defer and the acceptor wins the lock at the
  first parcel; the candidate now marks the initial links stable at the same
  point in the exchange, and waits for the broker's own bypass
  (`BypassPeerWithLink(14→15)`) before the transfer-back, reproducing the
  baseline's ordering deterministically;
- `Router::finish_decays` captured the released decaying link *after*
  `maybe_finish_decay` reset the decaying slot (the official captures the
  link before `MaybeFinishDecay`) — fixed with a regression test.

Not yet implemented (documented scope boundary): `BypassPeer`/
`AcceptBypassLink` outbound, multi-subparcel and split parcels, multi-node
graphs, and node loss beyond the single link.

### Phase 5 memory court — parcel-fragment allocation and free-list reuse

The memory court (`scripts/run_memory_court.sh`) runs the official broker
(`invite-broker-memory`) against the official oracle acceptor (baseline) and
against the native Rust `memory-acceptor` (interop), both through the
wire-relay man-in-the-middle:

1. the broker transfers `B` through the bootstrap pipe and writes `w1` on `A`;
2. the acceptor sends `m0..m8` (9 × 200-byte parcels) on `B'` — the primary
   buffer's 256-byte block pool holds exactly 8 allocable blocks, so `m8`'s
   fragment allocation fails and `m8` travels inline;
3. the acceptor sends a `sync` marker; the broker reads `m0..m8` only after
   receiving it, freeing the blocks (LIFO free-list);
4. the acceptor sends the transfer-back; the broker extracts `B''` and does a
   `w2` round trip, then sends `w3`;
5. the acceptor sends `m9`/`m10` on the bootstrap — fragment-backed from the
   **freed primary blocks** (m9 reuses block 8 at offset 98048, m10 reuses
   block 7 at 97792);
6. the broker reads `m9`/`m10`, then closes everything (`RouteClosed`
   propagation).

The equivalence relations, strongest first: the broker's event stream is
BYTE-IDENTICAL between the baseline and the interop run; the acceptor→broker
wire (decoded by `wire-dump`, normalized only for node names and per-link
sequence numbers) is IDENTICAL — including every fragment descriptor and
offset; all four processes exit 0. The court has passed repeatedly
(3+ consecutive runs).

The native now allocates parcel data into shared-memory fragments at put time
(remote primary link) exactly like the oracle — the earlier routing-court
normalization "the oracle fragments, the native inlines" is obsolete for the
acceptor's parcels. One residual remains there: the 64-byte free-list
interleaving can differ by one block (baseline r1 at 1152 vs interop r1 at
1280 in the routing court) because the native does not free a bypassed link's
`RouterLinkState` when its decay completes (the official frees it). The
offset difference is allocation-order dependent and normalized; the memory
court's 64-block allocations matched exactly because the preceding allocation
order coincided.

**Epoch discovery**: the pinned mojo embedder sets
`IPCZ_MEMORY_FIXED_PARCEL_CAPACITY` (`mojo/core/ipcz_api.cc`, TODO
crbug.com/40876289), so `allow_memory_expansion_for_parcel_data_` is false:
parcel-data-driven block-capacity expansion is DISABLED in this epoch (the
baseline confirms — no `RequestMemory`/`ProvideMemory`/`AddBlockBuffer`
traffic appears). The native mirrors this: `write_parcel_fragment_or_inline`
falls back to inline data without lobbying. The `RequestMemory` send path
(`pending_memory_requests_` FIFO), the `ProvideMemory` receive path
(share-then-register ordering), `AddBlockBuffer` in both directions, and the
shared-header buffer-id allocation are implemented and unit-tested, but no
sealed court triggers them in this epoch: the only reachable trigger is
`RouterLinkState` pool exhaustion (the unconditional lobby in
`TryAllocateRouterLinkState`, 1484 blocks), which requires the proxy-bypass
machinery (`BypassPeer` outbound / `AcceptBypassLink` inbound) — the next
Phase 5 gate, which the exhaustion court will seal.

Evidence: `evidence/memory/<stamp>/` (events, wire captures, byte-identical
broker streams), `evidence/manifests/memory-<stamp>.json`, and the casefile
`courts/curated/phase5-memory-court.md`.

### Phase 5 block-capacity exhaustion court — AddBlockBuffer receive side

The exhaustion court (`scripts/run_exhaust_court.sh`) runs the official
broker (`invite-broker-exhaust`) against the official oracle acceptor
(baseline) and the native Rust `exhaust-acceptor` (interop), both through the
wire-relay man-in-the-middle: the broker transfers 1486 portals through the
bootstrap pipe (all pairs held, so the `RouterLinkState` blocks stay
allocated); the primary buffer's 64-byte pool (1483 allocable blocks)
exhausts mid-stream; the failing transfer falls back to the plain proxy path;
the broker lobbies `RequestBlockCapacity(64)` (the unconditional
`TryAllocateRouterLinkState` lobby), allocates a 64 KiB buffer locally, and
shares it via `AddBlockBuffer`; the native adopts it and resolves the
remaining transfers' link states from the new buffer (cross-buffer fragment
resolution). The broker's IO thread flushes asynchronously, so the transfers
arrive OUT OF route-sequence order and migrate across sublinks; both
acceptors reorder via their sequenced queues.

Equivalence: the broker's event stream (2979 events) is BYTE-IDENTICAL
between the runs; both acceptors deliver the complete route sequence
(rseq 0..1485) and exit 0; the expansion occurred in both runs. The court has
passed repeatedly (3+ consecutive runs).

The `RouterLinkState` refcount model (the `RefCountedFragment` lifecycle:
allocation ref 1, link-creation ref, no-increment adoption, last-ref free;
unmanaged fixed initial states) is now implemented and sealed by these
courts. It removed a real corruption bug: the ref-count word aliases the
`FragmentHeader.size` word when a freed state is reused as a parcel fragment,
and a stale release corrupted the fragment's size (the broker read "r" for
"r1" in the routing court).

Documented residual (reduced): the shared free-list REUSE ORDER still
differs by scheduler-dependent interleavings of the peer's IO-thread
releases (the exhaustion point differs — baseline ~transfer 1330 with one
`AddBlockBuffer`, interop ~transfer 750 with two — and the routing court's
r1 reuses a different freed block). The broker's event stream — the primary
equivalence — is unaffected; block-reuse order is internal and normalized.

This court also exposed and fixed a real forensic-tooling bug: the wire
relay and the native channel read with large fixed buffers, so a single
`recvmsg` coalesced several messages and `SCM_RIGHTS` attached the
descriptors to the read's first byte — associating a message's fd with the
wrong message. Both now read exactly the bytes needed to complete the
message at the front of the buffer (matching the official
`ChannelPosix::OnFdReadable`'s `next_read_size`); covered by the
`fd_association_survives_dense_stream` channel test.

Evidence: `evidence/exhaust/<stamp>/` (events, wire captures, byte-identical
broker streams), `evidence/manifests/exhaust-<stamp>.json`, and the casefile
`courts/curated/phase5-exhaust-court.md`.

### Phase 5 bypass court — RequestMemory/ProvideMemory/AddBlockBuffer SEND side

The bypass court (`scripts/run_bypass_court.sh`) seals the SEND side of the
block-capacity expansion round trip. After the routing-court prelude (the
broker transfers `b1` and writes `w1`), the ACCEPTOR creates 1520 fresh local
pairs and transfers one end of each through the bootstrap pipe — the
`SerializeNewRouterWithLocalPeer` serialization, newly implemented on the
native send side: a new central link with a fresh pool `RouterLinkState` plus
an adjacent decaying peripheral sublink (`proxy_already_bypassed=true`), the
local peer's outward edge released, and the proxy's inward edge armed with a
deferred decay (adopted in the pab-aware `BeginProxyingToNewRouter`; the
fresh-pair proxy decays and drops in the same flush). Each transfer holds one
`RouterLinkState` from the shared 64-byte pool. When the pool exhausts, the
unconditional `TryAllocateRouterLinkState` lobby fires
`request_block_capacity(64)` -> `RequestMemory{65536}` to the broker (this
node is the allocation delegate); the broker's `OnRequestMemory` allocates a
64 KiB buffer and replies `ProvideMemory`; the native `on_provide_memory`
adopts it, allocates buffer id 1 from the shared header, shares it via
`AddBlockBuffer` (transmitted BEFORE the local registration — the official
share-then-register ordering), and registers it; the remaining transfers
resolve their link states from the new buffer (cross-buffer fragment
resolution).

Equivalence: the broker's event stream (all 1529 events) is BYTE-IDENTICAL
between the baseline (official oracle acceptor) and the interop (native
`bypass-acceptor`); both wires carry all 1520 transfer parcels plus the
`sync` marker (the broker verifies every payload and exits 0 only if all
matched); the send-side round trip is present on the acceptor→broker wire in
BOTH runs (`RequestMemory` + `AddBlockBuffer`, one each in the sealed runs);
both acceptors exit 0. The court has passed 5+ consecutive runs.

Documented residual: the exhaustion POINT in the transfer loop differs
between the runs (the oracle's concurrent IO thread frees each transfer's
payload fragment as the broker reads it — net ~1 block per transfer — while
the native's single-threaded loop does not wait for the peer's frees — net
~2 per transfer), so the `RequestMemory`'s wire position and the fragment
offsets differ. The broker's event stream — the primary equivalence — is
unaffected; block-reuse order is internal and normalized (the same
free-list-reuse interleaving documented in the routing and exhaustion
courts).

A real bug was found and fixed by this court: the native's
`encode_add_block_buffer`/`encode_provide_memory` attached an EMPTY driver
object, but the mojo driver's `SharedBuffer::Deserialize` requires the
40-byte serialization (`ObjectHeader{size=8, type=kSharedBuffer}` +
`BufferHeader{size, buffer_size, mode=kUnsafe, padding, guid_low, guid_high}`
— `mojo/core/ipcz_driver/shared_buffer.cc`); an empty object fails
deserialization and the official broker drops the NodeLink (all routes close;
the failed run is preserved under `evidence/bypass/20260805T185330Z/`). The
encoding now carries the full object with a deterministic non-zero GUID; the
golden test `encode_add_block_buffer_matches_official_capture` pins it
byte-identical to the official broker's `AddBlockBuffer` (modulo the
normalized sequence number and region GUID).

Evidence: `evidence/bypass/<stamp>/` (events, wire captures, byte-identical
broker streams), `evidence/manifests/bypass-<stamp>.json`, and the casefile
`courts/curated/phase5-bypass-court.md`.

### Phase 5 multi-node referral court — broker + referrer A + referred B, outbound AcceptBypassLink

The 3-node court (`scripts/run_3node_court.sh`) seals the multi-node referral
machinery: broker + referrer A + referred B, with the referral transport
captured by a second man-in-the-middle relay. The baseline runs all three nodes
as the official implementation; the interop replaces B with the native Rust
`3node-acceptor` (`RoutingAcceptor::run_3node`).

The native B implements `NodeConnectorForReferredNonBroker` +
`Invitation::Accept` (INHERIT_BROKER) end to end:

* the `ConnectToReferredBroker` greeting (transmitted raw, without consuming a
  NodeLink sequence number — matching the official connector, whose first
  NodeLink message also carries seq 0);
* `ConnectToReferredNonBroker` acceptance: adoption of the broker link (active,
  side B) and the referrer link (inactive, then activated) from the three
  driver objects, and `EstablishWaitingRouters` on the referrer link (initial
  portals 0/1, the shared-memory client handshake on portal 0, the bootstrap
  attachment bridge on portal 1, side-B stable marks);
* the multi-node portal transfer: the re-transfer of Y' through the b2a pipe
  (referrer sublink 1), whose descriptor carries `proxy_peer_node_name` = the
  broker, triggering the **outbound** `BypassPeer` →
  `BypassPeerWithNewRemoteLink` → `AcceptBypassLink` (id 31) to the broker
  (the previously unreachable outbound path — in 2-node graphs the broker's
  `MaybeStartSelfBypass` always takes `StartSelfBypassToLocalPeer`);
* the X↔Y' round trip: `hello` (forwarded by A's proxy over the A↔B decaying
  sublink) and `world` (a fragment parcel `{0,1152,64}` over the new broker↔B
  central sublink 12), `ProxyWillStop`, `RouteClosed` peer closure, and the
  local closes.

The native `3node-referrer` implements the REFERRER side (interop-a: native
A → official B): the Connect handshake without the client send (the official
referrer transmits `ReferNonBroker` before the client parcel), the referral
send with the 16-byte `Transport` object, `NonBrokerReferralAccepted`
acceptance (A↔B link adoption, `EstablishWaitingRouters`, the service portal 0
and the a2b attachment bridge on portal 1), the transfer-y receipt and the
re-transfer with `proxy_peer_node_name` = the broker (the bypass lock records
B's full `NodeName` in the shared state, so the broker's
`CanNodeRequestBypass` accepts B's `AcceptBypassLink`), the un-consumed
`hello` forwarded by the proxy, and the acceptor-side bootstrap bridge bypass
with the waiting-bit wakeup that lets the broker's second stage and the
pipe_a closure complete.

Equivalence: in BOTH pairings the broker's AND the counterpart node's event
streams are BYTE-IDENTICAL between the baseline and the interop; all four
captured wire directions (broker↔A both ways, broker↔B both ways via the
referral transport) are structurally identical (decoded message sequences,
ids, sequence numbers, sublinks, fragment descriptors) modulo the node-name
GUIDs, which are normalized; all processes exit 0.

Bugs found and fixed while sealing this court:

* the court harness initially connected the relays to the SAME sockets the
  processes held (a relay must sit between two dedicated socketpairs), which
  flooded the referral transport with repeated greetings (~24M copies) and
  broke the broker↔A link;
* the outbound bypass path never registered its new sublink in `owners` (the
  inbound `BypassPeerWithLink` path did), so the broker's parcels on the new
  central sublink would have been classified "parcel for unbound sublink";
* `parcel_data` and the deferred-fragment queue were hard-coded to the broker
  link memory: each NodeLink has its own primary buffer with its own buffer id
  0, so parcels on the referrer link must resolve fragments against the
  referrer memory (the deferral queue is now keyed by `(link, buffer)`);
  `put`'s payload-fragment allocation and `serialize_router`'s sublink
  allocation now use the TARGET link's memory, and `transmit_parcel` sends on
  the link's own channel;
* `ProxyWillStop` (id 33) was previously rejected as unsupported; the multi-node
  court exercises it (the broker's response to B's `AcceptBypassLink`), so the
  `router_proxy_will_stop` state machine was implemented;
* `encode_transport_object` carried a spurious `TransportHeader.size` field
  (20 bytes vs the official 16: `ObjectHeader{size, type}` +
  `TransportHeader{destination_type, 4 flag bytes}`), which made the
  `ReferNonBroker` transport object unreadable and the referral fail silently;
* the referrer's `ReferNonBroker` must precede the shared-memory client
  handshake (the official wire order), and the bypass lock must record the
  FULL `NodeName` of the serialization target (the broker's
  `CanNodeRequestBypass` compares the full name in the shared state);
* the forwarded `hello` was consumed into the terminal portal and lost when
  Y' became a proxy — it is now accepted without the portal delivery, staying
  in the route's queue (the official's app never reads it); this required
  `ParcelQueue::get_current_sequence_length` to count the contiguous PUSHED
  span (the official's `GetCurrentSequenceLength` advances at append), which
  the decay checks and the side-B stable marks depend on;
* the bootstrap bridge bypass needed a forced attachment flush after the
  stable marks to fire the waiting-bit wakeup (`FlushRouter`), letting the
  broker's second-stage bypass complete so the pipe_a closure arrives; the
  closure predicate is evaluated per message because the bypass migrates the
  attachment's sublink;
* the wire-relay now tolerates EPIPE on forward (a node may exit right after
  its final messages, e.g. B's teardown `RouteClosed` after the broker exits),
  keeping the relay's exit status clean.

Evidence: `evidence/3node/<stamp>/` (six event streams, eight wire captures,
manifest), `evidence/manifests/3node-<stamp>.json`, and the casefile
`courts/curated/phase5-multinode-court.md`. The referral wire baseline is also
preserved in the earlier forensic captures (`/tmp/3n-*.bin`, decoded in the
casefile).

### Phase 5 introduction court — broker + referrer A + referred B + introduced C, both mixed-language pairings

The 4-node court (`scripts/run_4node_court.sh`) seals the introduction
machinery (message ids 10–13) — the `EstablishLink` →
`BypassPeerWithNewRemoteLink` path for `Router::BypassPeer` when the bypass
target has NO direct link, which was the next-highest-value parity gate. The
topology: broker + referrer A + referred B + introduced C, with all three
broker links captured by man-in-the-middle relays. A creates (X, Y) locally
and transfers Y through the a2b pipe (the WithLocalPeer path over the direct
link); B re-transfers Y' through the b2c pipe with `proxy_peer_node_name` = A;
C's new router calls `BypassPeer(A)`, finds no link, and sends
`RequestIntroduction` to the broker; the broker sends `AcceptIntroduction` to
both C (side A) and A (side B) with a transport + buffer pair; C adopts the
new C↔A link and completes the bypass with `AcceptBypassLink` over it; the
X↔Y'' "hello"/"world" round trip then crosses the new link. Both mixed
pairings run: interop-c (native Rust C = `4node-acceptor`) and interop-a
(native Rust A = `4node-referrer`). In each pairing the broker's AND the
counterpart nodes' event streams are byte-identical to the all-official
baseline, the native node verifies its exchange and exits 0, and the relayed
broker-link wires are structurally identical (see the normalization note
below).

The native side implements, against the pinned ipcz sources: the wire layer
for `RequestIntroduction`/`AcceptIntroduction`/`RejectIntroduction`/`BypassPeer`
(with the packed `AcceptIntroduction` V0 layout — single-byte `link_side` /
`remote_node_type` at offset 16, `remote_protocol_version` at 20, transport
and memory driver objects — and the V1 `remote_features` OFFSET field, not
the bitfield value), `router_bypass_peer` with the no-link fallback to
`establish_link`, the `pending_introductions` queue, `on_accept_introduction`
(transport + buffer adoption), `on_accept_bypass_link` with the
`CanNodeRequestBypass` check, and the per-link memory/sub-link generalizations
the multi-link graph requires (the bridge self-bypass on the c2b route
migrates the transfer's arrival sublink; the introduced link carries
`AcceptBypassLink`, `StopProxying`/`ProxyWillStop`, and the round trip). The
16/16 golden wire tests pin the packed message encodings byte-identical to
the official captures (`crates/mojo-rs-interop/testdata/ipcz/4node-*.bin`).

Bugs found and fixed while sealing this court:

* the transfer parcel's arrival sublink is NOT hard-codable: the c2b route's
  bridge self-bypass (`maybe_start_bridge_bypass` after the initial portals
  stabilize) migrates the route from sublink 1 to a fresh sublink before the
  transfer arrives, so the transfer predicate matches the PORTAL handle type
  on any sublink (the same fix re-sealed the 3-node native B);
* the referrer's `connect_handshake` never set the broker link's
  `remote_name` in the multi-link refactor's `links` map (the old
  `remote_name_for` returned `self.broker_name`), so the referrer's
  re-transfer descriptor carried `proxy_peer_node_name = {0,0}` and the
  referred node never bypassed — the world round trip then stalled (this
  regressed the sealed 3-node interop-a pairing and is fixed in
  `connect_handshake`);
* the pipe_a bridge-bypass stage-1 lock race is decided by the shared
  `RouterLinkState` CAS, and the oracle acceptor deterministically wins only
  because its side-B stable mark lands after the broker's ConnectReply
  processing; the candidate reproduces that ordering by waiting until the
  broker's side-A stable bit is observable in the shared memory before
  marking (bounded; the broker always processes the ConnectReply);
* the "world" reply may arrive over the a2b link through B's proxy when C
  replies before its own bypass settles — the official `RecvPayload` is a
  Mojo read and link-agnostic, so the candidate accepts the reply on
  whichever link delivers it;
* teardown closes tolerate a peer that already dropped the transport
  (`CloseRoute` on a broken channel is not an error — the official driver's
  `Transmit` failure is asynchronous).

Documented nondeterminism (normalized, not hidden): the all-official baseline
itself races on the pipe_a bridge-bypass exchange — the shared-state lock CAS
between the broker and A decides the initiator of each stage, producing
different message sequences with byte-identical event streams that converge
on the same final sublink. The 4-node court's wire comparison therefore drops
the exchange messages (`BypassPeerWithLink`/`FlushRouter`/
`StopProxyingToLocalPeer`) and the ordinal/sequence prefixes on the broker↔A
directions only; the fixed-point traffic (the `done`/closure on the final
sublink) is still compared exactly. The 3-node court compares only the
broker↔B directions, which are not affected. The normalization is named and
documented in the runner and the receipt.

Evidence: `evidence/4node/<stamp>/` (twelve event streams, eighteen wire
captures, receipt), `evidence/manifests/4node-<stamp>.json`, the golden wire
fixtures, and the casefile `courts/curated/phase5-multinode-court.md`.

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

- Routing / port transfer (Phase 5): the remaining multi-node machinery —
  `BypassPeerWithNewLocalLink`, the broker-side referral roles (the native as
  broker or referrer of a further node: `NodeConnectorForBrokerReferral`, the
  broker's `HandleIntroductionRequest`/`IntroduceRemoteNodes` serving a native
  broker), `RejectIntroduction` and `RequestIndirectIntroduction` (ids 12–13;
  id 10 `RequestIntroduction` and id 11 `AcceptIntroduction` are sealed by the
  4-node court), `ConnectFromBrokerToBroker` (id 7), node loss
  (`RouteDisconnected`) beyond the single-link case, multi-node graphs beyond
  a single referral chain (4+ non-brokers, node loss mid-referral),
  split/multi-subparcel parcels, and the scheduler-dependent free-list reuse
  order (only the interleaving with the peer's IO-thread releases differs —
  normalized). The native routing acceptor now seals the WithLocalPeer
  transfer (both directions), the proxy serialization path, the
  acceptor-initiated bridge bypass (both directions), `StopProxying` teardown,
  closure propagation over a single NodeLink, the parcel-fragment allocator
  (memory court), the `AddBlockBuffer` receive side + cross-buffer fragment
  resolution (exhaustion court), the `RequestMemory`/`ProvideMemory`/
  `AddBlockBuffer` SEND side (bypass court), the `RouterLinkState` refcount
  lifecycle, the multi-node referral in BOTH mixed-language pairings (broker +
  referrer A + referred B; the outbound `AcceptBypassLink` and the
  referrer-side serialization sealed by the 3-node court), and the
  introduction machinery (ids 10–11: `RequestIntroduction` /
  `AcceptIntroduction` and the `EstablishLink` →
  `BypassPeerWithNewRemoteLink` path with a 3-node graph, sealed by the 4-node
  court in BOTH mixed-language pairings).
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
- `scripts/run_routing_court.sh` — Phase 5 routing seal (official broker ⇄
  native routing acceptor; portal transfer in both directions + proxy bypass;
  byte-identical broker events).
- `scripts/run_memory_court.sh` — Phase 5 memory court (official broker ⇄
  native memory acceptor; parcel-fragment allocation, pool exhaustion,
  free-list reuse; byte-identical broker events + wire-identical
  acceptor→broker captures).
- `scripts/run_exhaust_court.sh` — Phase 5 block-capacity exhaustion court
  (official broker ⇄ native exhaust acceptor; 1486 held portal transfers,
  `RouterLinkState` pool exhaustion, `AddBlockBuffer` adoption and
  cross-buffer fragment resolution; byte-identical broker events).
- `scripts/run_bypass_court.sh` — Phase 5 acceptor-initiated exhaustion court
  (official broker ⇄ native bypass acceptor; 1520 WithLocalPeer transfers
  exhaust the acceptor-side pool; `RequestMemory` → `ProvideMemory` →
  `AddBlockBuffer` SEND round trip sealed; byte-identical broker events).
- `scripts/run_3node_court.sh` — Phase 5 multi-node referral court (official
  broker + official referrer A + native referred B; referral handshake,
  broker/referrer link adoption, outbound `AcceptBypassLink`; byte-identical
  broker AND A event streams; four structurally identical wire directions).
- `scripts/run_4node_court.sh` — Phase 5 introduction court (official broker
  + referrer A + referred B + introduced C in BOTH mixed-language pairings;
  `RequestIntroduction`/`AcceptIntroduction`/`AcceptBypassLink` over the
  introduced link; byte-identical broker/counterpart event streams; relayed
  broker-link wires structurally identical modulo the documented pipe_a
  bridge-bypass race).
- `scripts/run_court.sh verify <manifest>` — receipt invalidation check.
- One-command reproduction from clean Docker images:
  `scripts/compose_project.sh build && scripts/run_court.sh system`
  (daemon: `scripts/start_project_docker.sh`).
