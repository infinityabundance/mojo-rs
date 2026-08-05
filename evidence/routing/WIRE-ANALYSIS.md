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
   `next_sublink_id` counter; the exact ids depend on allocation order, which
   differs because the two acceptor implementations exercise different
   subsets of the routing machinery.
3. **Per-link sequence numbers** — assigned per direction by each side; only
   self-consistency within each direction is contractual.

## One documented divergence: the acceptor's bridge bypass

The broker's bootstrap message pipe is backed by an ipcz *bridge chain*
(`P1 ⟷ attachment-router ⟷ R_remote ⟷ acceptor`), because the invitation
attachment is `MergePortals`-ed onto the remote initial portal. Both the
official acceptor and the official broker independently run bridge bypass
(`MaybeStartBridgeBypass` / `StartBridgeBypassFromLocalPeer`) to collapse
that chain.

- **Baseline (oracle acceptor)**: the acceptor runs its own bridge bypass on
  receipt of the transfer (`BypassPeerWithLink(1→14)` + `FlushRouter(14)` +
  `StopProxyingToLocalPeer(14, 0)`); the broker then runs its own
  (`BypassPeerWithLink(14→15)`), and the transfer-back travels on sublink 15.
- **Interop (native acceptor)**: the native acceptor models the bootstrap
  pipe as a single terminal router (the full bridge chain is not modeled —
  documented scope boundary for this phase). It therefore does not initiate
  a bypass; it *replies* to the broker's bypass
  (`StopProxyingToLocalPeer(1, 0)`, sublink 1 being the pre-bypass link) and
  the transfer-back travels on the broker-assigned sublink 14.

The end state is identical (bootstrap route on a direct central link; B'
route on sublink 12; the broker's `BypassPeerWithNewLocalLink` completes the
proxy bypass and sends `StopProxying(12, 1, 1)`, which the native proxy
processes and decays away). The broker's observable behavior — every event,
every payload, every handle transfer, every closure — is byte-identical.

This divergence matches the Phase 3 interop seal's documented divergence
(the official acceptor initiates its own bypass; the native acceptor does
not) and is kept as a curated casefile
(`courts/curated/phase5-routing-bridge-bypass.md`).

## Message inventory (interop run, acceptor→broker)

| seq | id  | meaning                                                        |
|-----|-----|----------------------------------------------------------------|
| 0   | 1   | ConnectFromNonBrokerToBroker reply (8 initial portals)         |
| 0   | 20  | AcceptParcel sub 0: shared-memory-service client transport box |
| 1   | 22  | RouteClosed sub 0, len 1 (portal 0 closed after the handshake) |
| 2   | 35  | StopProxyingToLocalPeer sub 1, outbound 0 (reply to bypass)    |
| 3   | 20  | AcceptParcel sub 12, rseq 0: "r1" (B' → broker's A)            |
| 4   | 20  | AcceptParcel sub 14, rseq 0: "transfer-back" + B' descriptor   |

Broker→acceptor: Connect, transfer-b1 (WithLocalPeer descriptor:
`new=12, decaying=13, next_o=0, next_i=0, din=1, proxy_already_bypassed`),
w1 on the decaying sublink 13 (the broker's local peer forwards its queued
parcel over the decaying link), the broker's bridge bypass
`BypassPeerWithLink(1→14, inbound_len=1)`, `StopProxying(12, 1, 1)`, and
`RouteClosed(14, 1)` on the broker's bootstrap-pipe closure.

## State-machine residuals verified against the pinned oracle

The native routing acceptor's router behavior was verified line-by-line
against the pinned `router.cc`, `route_edge.cc`, `router_link_state.cc`,
`remote_router_link.cc`, and `node_link.cc` (Chromium
`151.0.7922.105`):

- `Router::Deserialize` (proxy_already_bypassed decaying-link setup,
  fragment validation, `peer_closed` handling).
- `SerializeNewRouterAndConfigureProxy` + `BeginProxyingToNewRouter`
  (peripheral inward link adoption, side-stable marking).
- `Router::AcceptBypassLink` semantics for the broker's
  `BypassPeerWithLink` (decay, `StopProxyingToLocalPeer`, side-B stable).
- `Router::StopProxying` final-length bookkeeping and proxy teardown.
- `Router::Flush` closure propagation (`TryLockForClosure` via
  `RouterLinkState::TryLock`), `RouteClosed` sequence lengths.
- `RouterLinkState::{TryLock, SetSideStable, Unlock, ResetWaitingBit}`
  compare-exchange loops (all `expected` refreshes from the observed status).

## Regression artifacts preserved

The failed interop runs are retained under `evidence/routing/` as forensic
receipts: `20260805T054918Z` and `20260805T055902Z` (w1 misrouting — the
parcel arrives on the decaying sublink) and `20260805T060330Z` /
`20260805T062712Z`-predecessor (non-null `new_link_state_fragment` in the
peripheral descriptor, rejected by `Router::Deserialize`).
