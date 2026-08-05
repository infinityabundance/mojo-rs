# Casefile: Phase 5 routing — acceptor-initiated bridge bypass

**Case id**: ROUTING.BRIDGE_BYPASS.001
**Court**: routing
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)
**Status**: sealed — the acceptor-initiated bridge bypass is reproduced
message-for-message by the native routing acceptor; the broker's event
streams are byte-identical to the baseline.

## Preconditions

- Official broker (`invite-broker-routing`): creates invitation, attaches the
  `bootstrap` pipe, sends the invitation over a Unix socket, creates a message
  pipe `(A, B)`, sends `B` through the bootstrap pipe (the
  `SerializeNewRouterWithLocalPeer` path), writes `w1` on `A`, and later
  completes a proxy bypass and extracts the re-transferred portal `B''`.
- The broker's bootstrap pipe is backed by an ipcz *bridge chain*:
  `P1 ⟷ attachment-router ⟷ R_remote ⟷ acceptor` (the invitation attachment
  is `MergePortals`-ed onto the remote initial portal). The native acceptor
  models the mirror chain: `attachment ⟷ R_bridge ⟷ R_remote ⟷ [sublink 1]`.

## Observed oracle behavior (baseline)

The oracle acceptor runs its own bridge bypass on the bootstrap route and
emits, in order: `ConnectFromNonBrokerToNonBroker`,
`AcceptParcel(sub 0, shared-memory transport)`, `RouteClosed(sub 0, 1)`,
`BypassPeerWithLink(sub 1 → new 14)`, `FlushRouter(14)`,
`StopProxyingToLocalPeer(14, 0)`, `AcceptParcel(sub 12, rseq 0, "r1")`,
`AcceptParcel(sub 15, rseq 0, "transfer-back" + descriptor
{new 16, proxy_peer_sublink 12})`.

## Candidate behavior (interop)

The native `routing-acceptor` models the full bridge chain
(`MergeRoute`-equivalent setup, bridge-aware `Flush` / `StopProxyingToLocalPeer`
/ `AcceptBypassLink` / `AcceptRouteClosureFrom`, and the
`MaybeStartBridgeBypass` / `StartBridgeBypassFromLocalPeer` state machines).
It emits the identical message set in the identical order:

```
[0] ConnectFromNonBrokerToNonBroker
[1] AcceptParcel(sub 0, shared-memory transport)
[2] RouteClosed(sub 0, 1)
[3] BypassPeerWithLink(sub 1 → new 14, frag {0, 1152, 64}, inbound_len 0)
[4] FlushRouter(14)
[5] StopProxyingToLocalPeer(14, 0)
[6] AcceptParcel(sub 12, rseq 0, "r1")
[7] AcceptParcel(sub 15, rseq 0, "transfer-back" + descriptor
    {new 16, proxy_peer_sublink 12})
```

## Equivalence relation

1. The broker's event stream (all 15 events) is **byte-identical** between
   the baseline and the interop run.
2. The acceptor→broker wire message *set* and *order* are identical, with
   only the permitted normalizations below.

## Normalizations

1. Node names (random per run).
2. Parcel data transport: the oracle writes parcel data into shared-memory
   fragments (reusing freed blocks); the native inlines it. Both decode
   identically; the broker cannot distinguish them.
3. Per-direction link sequence numbers (self-consistent per direction only).

## Timing note (documented)

The baseline oracle marks its initial links side-B stable only after the
broker has processed the Connect reply, so the broker's early bridge-bypass
lock attempts defer and the acceptor wins the lock race at the first parcel.
The candidate reproduces this observable ordering by marking the initial
links stable at the same point in the exchange. See
`evidence/routing/WIRE-ANALYSIS.md`.

## Evidence

- `evidence/routing/<stamp>/baseline/wire/*.bin`
- `evidence/routing/<stamp>/interop/wire/*.bin`
- `evidence/routing/<stamp>/{baseline,interop}/broker.events` (byte-identical)
- `evidence/routing/WIRE-ANALYSIS.md`
- `evidence/manifests/routing-<stamp>.json`
- Decoder: `cargo run -p mojo-rs-interop --bin wire-dump -- <capture.bin>`
