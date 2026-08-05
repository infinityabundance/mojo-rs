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
descriptor), and `20260805T094216Z` (the pre-ordering-fix run in which the
transfer-back rode on the native's own bypass sublink 14 because the
broker's `BypassPeerWithLink(14→15)` had not yet been processed).

## Forensic tooling

`cargo run -p mojo-rs-interop --bin wire-dump -- <capture.bin>` decodes a
wire capture into a human-readable message inventory (message ids, sequence
numbers, sublinks, fragment descriptors, parcel payloads, and `RouterDescriptor`
fields). It is the decoding side of this analysis and of the casefiles.
