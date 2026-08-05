# Casefile: Phase 5 routing — broker bridge-bypass divergence

**Case id**: ROUTING.BRIDGE_BYPASS.001  
**Court**: routing  
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)  
**Status**: documented divergence (permitted); sealed by byte-identical broker
event streams.

## Preconditions

- Official broker (`invite-broker-routing`): creates invitation, attaches the
  `bootstrap` pipe, sends the invitation over a Unix socket, creates a message
  pipe `(A, B)`, sends `B` through the bootstrap pipe (the
  `SerializeNewRouterWithLocalPeer` path), writes `w1` on `A`, and later
  completes a proxy bypass and extracts the re-transferred portal `B''`.
- The broker's bootstrap pipe is backed by an ipcz *bridge chain*:
  `P1 ⟷ attachment-router ⟷ R_remote ⟷ acceptor` (the invitation attachment
  is `MergePortals`-ed onto the remote initial portal).

## Observed oracle behavior (baseline)

The oracle acceptor runs its own bridge bypass on the bootstrap route and
emits, in order: `ConnectFromNonBrokerToBroker`,
`AcceptParcel(sub 0, shared-memory transport)`, `RouteClosed(sub 0, 1)`,
`BypassPeerWithLink(sub 1 → new 14)`, `FlushRouter(14)`,
`StopProxyingToLocalPeer(14, 0)`, `AcceptParcel(sub 12, rseq 0, "r1")`,
`AcceptParcel(sub 15, rseq 0, "transfer-back" + descriptor
{new 16, proxy_peer_sublink 12})`.

## Candidate behavior (interop)

The native `routing-acceptor` models the bootstrap pipe as a single terminal
router (the bridge chain is a documented scope boundary for this phase). It
emits: `ConnectFromNonBrokerToBroker`, `AcceptParcel(sub 0, ...)`,
`RouteClosed(sub 0, 1)`, `StopProxyingToLocalPeer(sub 1, 0)` (reply to the
broker's `BypassPeerWithLink(sub 1 → new 14)`), `AcceptParcel(sub 12, rseq 0,
"r1")`, `AcceptParcel(sub 14, rseq 0, "transfer-back" + descriptor
{new 15, proxy_peer_sublink 12})`.

## Equivalence relation

The broker's event stream (all 15 events: invitation, transfer, `w1`, `r1`,
`transfer-back` with one extracted handle, `w2` delivered locally to `B''`,
closes, lifecycle) is **byte-identical** between the baseline and the interop
run. The broker cannot distinguish the native acceptor from the official one.

## Normalizations

1. Node names (random per run).
2. Sublink ids (allocation-order dependent — the native run does not
   allocate the acceptor's bypass sublink).
3. Per-direction link sequence numbers (self-consistent per direction only).

## Residual

The only structural wire difference is the acceptor-initiated bridge bypass
message set (`BypassPeerWithLink`/`FlushRouter`/`StopProxyingToLocalPeer`
from the acceptor) which the native acceptor does not emit because it does
not model the bridge chain. The end state is identical. This is the same
divergence documented in the Phase 3 interop seal.

## Evidence

- `evidence/routing/<stamp>/baseline/wire/*.bin`
- `evidence/routing/<stamp>/interop/wire/*.bin`
- `evidence/routing/<stamp>/{baseline,interop}/broker.events` (byte-identical)
- `evidence/routing/WIRE-ANALYSIS.md`
- `evidence/manifests/routing-<stamp>.json`
