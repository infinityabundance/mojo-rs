# Routing court wire analysis (Phase 5)

This document is the residual analysis for the routing interop court
(`scripts/run_routing_court.sh`): official broker ⇄ native Rust
`routing-acceptor`, sealed by byte-identical broker event streams.

The evidence root for each run is `evidence/routing/<stamp>/`; the sealed
manifest is `evidence/manifests/routing-<stamp>.json`.

## Comparison rules

The primary equivalence relation is the **broker's event stream**: the
official `invite-broker-routing` driver's events must be byte-identical when
its peer is the official oracle acceptor (baseline) and when its peer is the
native Rust routing acceptor (interop). The broker cannot distinguish the two
peers.

The raw wire captures are additionally compared semantically. Permitted
normalizations (each narrow and documented):

1. **Node names** — `ConnectFromBrokerToNonBroker` assigns random node names
   each run; `proxy_peer_node_name` low bits therefore differ.
2. **Sublink allocation order** — sublink ids are allocated from a shared
   `next_sublink_id` counter; exact ids depend on allocation order. In the
   sealed runs the ids coincide with the baseline's (`14`, `15`, `16`)
   because both implementations exercise the same allocation sequence.
3. **Per-link sequence numbers** — assigned per direction by each side; only
   self-consistency within each direction is contractual.
4. **Parcel data transport** — the oracle writes parcel data into
   shared-memory fragments (`AcceptParcel.parcel_fragment`), reusing freed
   blocks (e.g. the freed bypass link-state block at 1152 for `r1`); the
   native inlines the data. Both forms decode identically on the receiving
   side (the broker's events are byte-identical), so fragment-backed vs
   inline parcel data is a sender-side implementation choice.

## The bridge-bypass divergence is closed

The broker's bootstrap message pipe is backed by an ipcz *bridge chain*
(`P1 ⟷ attachment-router ⟷ R_remote ⟷ acceptor`), because the invitation
attachment is `MergePortals`-ed onto the remote initial portal. Both the
official acceptor and the official broker independently run bridge bypass
(`MaybeStartBridgeBypass` / `StartBridgeBypassFromLocalPeer`) to collapse
that chain.

The native routing acceptor now models the full bridge chain: the
app-facing attachment router, the interior bridge router, and the initial
portal router (`R_remote`, sublink 1) linked by a local bridge edge; the
bridge-aware `Flush` (bridge parcel forwarding, bridge decay, dead-bridge
closure forwarding, `MaybeStartBridgeBypass`); and the bridge-aware
`StopProxyingToLocalPeer` / `AcceptRouteClosureFrom` / `AcceptBypassLink`.

The acceptor→broker wire now matches the baseline message-for-message:

```
[0] ConnectFromNonBrokerToBroker
[1] AcceptParcel sub 0 (shared-memory transport box)
[2] RouteClosed sub 0, len 1
[3] BypassPeerWithLink sub 1 → new 14, frag {0, 1152, 64}, inbound_len 0
[4] FlushRouter sub 14
[5] StopProxyingToLocalPeer sub 14, out_len 0
[6] AcceptParcel sub 12, rseq 0, "r1"
[7] AcceptParcel sub 15, rseq 0, "transfer-back" + descriptor {new 16,
    proxy_peer_sublink 12}
```

`[3]` is the acceptor's own bridge bypass (`StartBridgeBypassFromLocalPeer`
from the interior bridge router, whose outward peer is the local
attachment); `[4]` is the waiting-bit wakeup (`FlushOtherSideIfWaiting`)
once the acceptor marks side A stable on the bypass link after collapsing
the bridge; `[5]` is the acceptor's reply to the broker's own bypass
(`AcceptBypassLink` on sub 14 → 15). The transfer-back rides on the
broker-assigned sublink 15, and the re-transferred portal descriptor
allocates sublink 16 — exactly the baseline.

### Timing note

The baseline oracle marks its initial links stable only after the broker has
processed the Connect reply; the broker's early bridge-bypass lock attempts
therefore defer (their waiting bit is set), and the acceptor wins the lock
race when the transfer parcel arrives. The candidate reproduces this
observable ordering by marking the initial links stable at the same point in
the exchange (after the Connect round trip, just before processing the first
parcel) rather than at Connect time. The broker's event stream is unaffected.

## State-machine residuals verified against the pinned oracle

The native routing acceptor's router behavior was verified line-by-line
against the pinned `router.cc`, `route_edge.cc`, `router_link_state.cc`,
`remote_router_link.cc`, `local_router_link.cc`, and `node_link.cc`
(Chromium `151.0.7922.105`):

- `Router::MergeRoute` (bridge-chain creation; `LocalRouterLink::CreatePair`
  with `kBridge` links, born `kUnstable`; `OpenPortals` central links born
  `kStable`).
- `Router::MaybeStartBridgeBypass` — all three cases (neither/one/both
  outward peers local), the two outward-link `TryLockForBypass` ordering,
  and the unlock-on-failure path.
- `Router::StartBridgeBypassFromLocalPeer` — the five-edge decay, the
  `BypassPeerWithLink` transmission, and the local peer's adoption of the
  new central link (side A).
- `Router::AcceptBypassLink` (receive side of `BypassPeerWithLink`) — decay
  of the old link, adoption of the new central link (side B), and
  `StopProxyingToLocalPeer` on the old sublink.
- `Router::StopProxyingToLocalPeer` — both the plain local-peer case and the
  bridge-peer case (five final-length assignments across three local
  routers).
- `Router::Flush` — bridge parcel collection
  (`CollectParcelsToFlush(inbound, *bridge_)`), bridge decay
  (`MaybeFinishDecay(inbound, outbound)`), the decay-gated
  `MarkSideStable`, dead-bridge closure forwarding, the bridge-bypass
  precondition (`bridge_link && stable outward && no inward`), and the
  `FlushOtherSideIfWaiting` tail gated on `dropped_last_decaying_link` /
  `kForceProxyBypassAttempt`.
- `LocalRouterLink::AcceptParcel` — bridge links deliver via
  `AcceptOutboundParcel`; central links via `AcceptInboundParcel`.
- `RouterLinkState::{TryLock, SetSideStable, Unlock, ResetWaitingBit}`
  compare-exchange loops (all `expected` refreshes from the observed status),
  and their in-process counterparts for local links.

## Regression artifacts preserved

The failed interop runs are retained under `evidence/routing/` as forensic
receipts: `20260805T054918Z` and `20260805T055902Z` (w1 misrouting — the
parcel arrives on the decaying sublink), `20260805T060330Z` /
`20260805T062712Z` (non-null `new_link_state_fragment` in the peripheral
descriptor), `20260805T094216Z` (the pre-ordering-fix run in which the
transfer-back rode on the native's own bypass sublink 14 because the
broker's `BypassPeerWithLink(14→15)` had not yet been processed), and
`20260805T121115Z` (the first fragment-transmit run: the transfer-back was
malformed because the inline data array was still appended when a fragment
was present, shifting the handle-types/new-routers array offsets — fixed by
gating the inline append on the fragment being absent).

## Forensic tooling

`cargo run -p mojo-rs-interop --bin wire-dump -- <capture.bin>` decodes a
wire capture into a human-readable message inventory (message ids, sequence
numbers, sublinks, fragment descriptors, parcel payloads, and `RouterDescriptor`
fields). It is the decoding side of this analysis and of the casefiles.

## The memory court (fragment allocator seal, `scripts/run_memory_court.sh`)

### Epoch discovery: parcel-data expansion is disabled

The pinned mojo embedder sets `IPCZ_MEMORY_FIXED_PARCEL_CAPACITY`
(`mojo/core/ipcz_api.cc`, TODO crbug.com/40876289), so
`allow_memory_expansion_for_parcel_data_` is false: `AllocateFragment` does
NOT lobby for parcel data. The baseline confirms: sending 9 x 200-byte
parcels through the 8-block primary 256-byte pool produces NO
`RequestMemory`/`ProvideMemory`/`AddBlockBuffer` traffic — the 9th parcel
(m8) falls back to inline data, and later parcels (m9/m10) reuse the freed
primary blocks (LIFO free-list: block 8 at offset 98048, then block 7 at
97792). The only reachable expansion trigger in this epoch is
`RouterLinkState` pool exhaustion (the unconditional lobby in
`TryAllocateRouterLinkState`).

### The native matches the baseline byte-for-byte on the wire

The native `memory-acceptor` now allocates parcel data into shared-memory
fragments at put time (remote primary link), exactly like the oracle. The
acceptor→broker wire of the interop run is IDENTICAL to the baseline after
normalizing node names and per-link sequence numbers — including the fragment
offsets: m0..m7 at 96256..98048 (blocks 1..8), m8 inline, sync at 1152,
transfer-back at 1280, m9 at 98048 and m10 at 97792 (the freed-block reuse).
The shared free-list state, the put-time allocation order, and the LIFO reuse
are all reproduced exactly.

### Normalization update for the routing court

The earlier routing-court normalization "parcel data transport — the oracle
writes parcel data into fragments, the native inlines it" is now obsolete
for the ACCEPTOR's parcels: the native fragments its parcels exactly like the
oracle. One residual remains: the 64-byte free-list interleaving can differ
by one block (e.g. baseline r1 at 1152 vs interop r1 at 1280 in the routing
court) because the native does not free a bypassed link's `RouterLinkState`
when the decay completes (the official frees it; the native retains it). The
offset difference is allocation-order dependent and normalized; the memory
court's 64-block allocations (sync at 1152, transfer-back at 1280) matched
exactly because the preceding allocation order coincided.

## The exhaustion court (`scripts/run_exhaust_court.sh`)

The broker transfers 1486 portals through the bootstrap (all pairs held);
the primary buffer's 64-byte `RouterLinkState` pool (1483 allocable blocks)
exhausts mid-stream. The failing transfer falls back to the plain proxy
path; the broker lobbies `RequestBlockCapacity(64)` (the unconditional
`TryAllocateRouterLinkState` lobby), allocates a 64 KiB buffer locally, and
shares it via `AddBlockBuffer`; the acceptor adopts it and resolves the
remaining transfers' link states from the new buffer (cross-buffer fragment
resolution). The transfers arrive OUT OF route-sequence order (the broker's
IO thread flushes asynchronously) and migrate across sublinks (route
bypasses); the receivers reorder via their sequenced queues.

The broker's event streams are byte-identical between the baseline and the
interop run, and both acceptors deliver the complete route sequence
(rseq 0..1485) and exit 0.

### The `RouterLinkState` refcount model (link-state free on decay)

The native now models the `RefCountedFragment` lifecycle for shared
`RouterLinkState`s: `try_allocate_link_state` initializes the ref count to 1;
`AddRemoteRouterLink`-equivalent link creation takes a second ref (the
official `FragmentRef` copy in `AddRemoteRouterLink`); adoption
(`AdoptFragmentRefIfValid`) does NOT increment (it takes the sender's
`release()`d ref); a link's release (`GenericFragmentRef::reset`) decrements
and the LAST ref frees the block back to the shared pool. The fixed
initial-portal states are unmanaged (never refcounted or freed).

This removed a real corruption bug: the `RefCountedFragment` ref-count word
occupies the first 4 bytes of a `RouterLinkState`, the same word the
`FragmentHeader.size` uses when the block is reused as a parcel fragment — a
stale release after the block was freed+reused silently corrupted the
fragment's size (the broker read `"r"` for `"r1"`). The native now releases
refs at the decay completions (`finish_decays`), router removal, and closure.

Documented residual (reduced): the shared free-list REUSE ORDER still differs
by scheduler-dependent interleavings of the peer's IO-thread releases (e.g.
the routing court's r1 reuses block 1152 in the baseline but 1280 in the
interop; the exhaustion point differs and the interop can trigger a second
`AddBlockBuffer`). The broker's event stream — the primary equivalence — is
unaffected; block-reuse order is an internal allocation order (normalized).

### Forensic fix: read-sizing the relay and the channel

The dense traffic exposed a real bug in the forensic tooling: the wire relay
and the native channel read with large fixed buffers, so a single `recvmsg`
coalesced several messages, and `SCM_RIGHTS` attaches the descriptors to the
READ's first byte — associating a message's fd with the wrong message
(observable as `BadDriverObjects` on a transfer that inherited the
`AddBlockBuffer`'s memfd). Both now read exactly the bytes needed to complete
the message at the front of the buffer, matching the official
`ChannelPosix::OnFdReadable`'s `next_read_size`; covered by the
`fd_association_survives_dense_stream` channel test.
