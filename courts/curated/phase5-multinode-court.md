# Casefile: Phase 5 multi-node referral court — broker + referrer A + referred B, outbound AcceptBypassLink

**Case id**: ROUTING.MULTI_NODE.001
**Court**: 3node
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)
**Status**: sealed — the referred node B (the native `3node-acceptor`) completes
the full referral handshake, the broker↔B link adoption, the referrer (A↔B)
link adoption with its initial portals, the re-transfer of a portal through the
b2a pipe, the **outbound** `AcceptBypassLink` (`Router::BypassPeer` →
`BypassPeerWithNewRemoteLink`), and the X↔Y' round trip ("hello"/"world"). The
broker's and A's event streams are byte-identical to the all-official baseline;
all four captured wire directions are structurally identical (node-name GUIDs
normalized).

## Preconditions

- Topology: broker + referrer A + referred B. The broker sends invitation-1
  (`TO_BROKER`) to A; A accepts and receives the bootstrap pipe `pipe_a`. A
  refers B: invitation-2 with `MOJO_SEND_INVITATION_FLAG_SHARE_BROKER` over a
  transport whose endpoint travels inside `ReferNonBroker` to the broker. B
  accepts with `MOJO_ACCEPT_INVITATION_FLAG_INHERIT_BROKER`.
- Transport layout (per the court harness):
  - broker ↔ A: relayed (`broker-to-a.bin` / `a-to-broker.bin`);
  - broker ↔ B: the *referral transport* — A's socket-b fd travels inside
    `ReferNonBroker` through relay1 to the broker, which connects to B through
    relay2 (`broker-to-b.bin` / `b-to-broker.bin`);
  - A ↔ B: the *referrer link*, created by the broker (`DriverTransport::CreatePair`);
    its endpoints travel inside `ConnectToReferredNonBroker` (B's end) and
    `NonBrokerReferralAccepted` (A's end). Not relayed.
- The broker creates `(X, Y)` and transfers Y through `pipe_a` to A; A
  re-transfers Y' through the a2b pipe to B (the multi-node portal transfer: A's
  Y' router serializes with `proxy_peer_node_name = broker`,
  `proxy_peer_sublink = 12`). B's new router Y'' calls `BypassPeer` and, having
  a direct link to the broker, `BypassPeerWithNewRemoteLink` — the **outbound
  `AcceptBypassLink`** (id 31) to the broker, which collapses the proxy chain.
- The broker writes `hello` on X (forwarded to B via A's proxy over the A↔B
  link while the bypass settles); B replies `world` on Y'' over the new
  broker↔B central link; the broker closes X; B observes peer closure.

## Observed oracle behavior (baseline)

B→broker (referral transport, `b-to-broker.bin`):

```
[0] ConnectToReferredBroker(id=3, protocol=0, num_initial_portals=8)
[1] AcceptBypassLink(id=31, current_peer_node=<A>, current_peer_sublink=12,
     inbound_len=0, new_sublink=12, frag={0,1088,64})
[2] AcceptParcel(sub=12, rseq=0, frag={0,1152,64})          -- "world"
```

broker→B (`broker-to-b.bin`):

```
[0] ConnectToReferredNonBroker(id=4, name=<B>, broker=<broker>,
     referrer=<A>, broker_buf=0, referrer_transport=1, referrer_buf=2)
[1] ProxyWillStop(sub=12, inbound_len=1)
[2] RouteClosed(sub=12, len=1)
```

A→broker (`a-to-broker.bin`): `ConnectReply`, `ReferNonBroker(id=2, transport=0)`,
`AcceptParcel(sub=0, shared-memory client transport)`, `RouteClosed(0,1)`, then
the Y'-proxy teardown: `BypassPeerWithLink(1→14)`, `FlushRouter(14)`,
`StopProxyingToLocalPeer(14,0)`, `AcceptParcel(sub=15)`.

broker→A (`broker-to-a.bin`): `Connect`, `AcceptParcel(sub=1, "transfer-y" +
descriptor {new=12, decaying=13, proxy_already_bypassed})`,
`AcceptParcel(sub=13, "hello")` (the decaying sublink — the broker's Y proxy
forwards "hello" to A over the decaying link once A re-transfers Y'),
`NonBrokerReferralAccepted(id=5, transport=0, buffer=1)`,
`StopProxyingToLocalPeer(1,1)`, `BypassPeerWithLink(14→15)`,
`StopProxying(12,1,0)`, `RouteClosed(15,1)`.

The oracle B's event stream (the reference for the native's op sequence):
`lifecycle`, accept result, extract result, message("transfer-y2b", 1 handle),
message("hello"), result("world"), message(peer closure, FAILED_PRECONDITION),
close result, lifecycle.

## Candidate behavior (interop)

The native `3node-acceptor` implements `NodeConnectorForReferredNonBroker` +
`Invitation::Accept` (INHERIT_BROKER):

- greets the broker with `ConnectToReferredBroker` (transmitted raw — no
  NodeLink sequence number, matching the official connector's `Transmit`);
- accepts `ConnectToReferredNonBroker`, adopting the broker link (active,
  side B; buffer driver object 0) and the referrer link (inactive then
  activated; transport + buffer driver objects 1 and 2);
- `EstablishWaitingRouters(referrer_link, num_initial_portals=2)`: initial
  portals 0 (internal) and 1 (b2a) get outward links on the referrer link
  (sublinks 0/1, side B, initial link states); the shared-memory client
  handshake goes on portal 0; the bootstrap attachment bridge is merged onto
  portal 1; side-B stable marks are written;
- receives the re-transfer on b2a (referrer sublink 1) and deserializes Y''
  (`router_deserialize` with `proxy_peer_node_name = broker` →
  `router_bypass_peer` → `bypass_peer_with_new_remote_link`): allocates the
  bypass `RouterLinkState` from the broker link memory (block 1 at
  `{0,1088,64}`), allocates sublink 12 on the broker link, decays the A↔B
  outward link, transmits `AcceptBypassLink(A, 12, 0, 12, {0,1088,64})`, and
  adopts the new central link (registering `owners[(broker, 12)]`);
- receives `hello` (over the A↔B decaying sublink), replies `world` (a
  fragment parcel `{0,1152,64}` — block 2 — over the broker link sublink 12);
- processes `ProxyWillStop(12, 1)` (`router_proxy_will_stop` sets the decaying
  link's received length), then `RouteClosed(12, 1)` → peer closure;
- closes b2a and Y'' locally.

## Equivalence relations

1. The broker's event stream is **byte-identical** between the baseline and the
   interop run (it cannot distinguish the native B from the official one).
2. A's event stream is **byte-identical** between the two runs.
3. All four captured wire directions are **structurally identical** (decoded
   message sequences, message ids, sequence numbers, sublinks, fragment
   descriptors `{0,1088,64}` / `{0,1152,64}`, handle counts) modulo the
   node-name GUIDs, which are normalized.
4. The native B verifies the transfer payload (`transfer-y2b`), the `hello`
   payload, delivers `world`, and observes peer closure (exit 0).

## Normalizers

- Node-name GUIDs (32-hex-char fields) → `<name>` (each run assigns fresh
  random names).
- The A↔B referrer link is not wire-captured (its endpoints are created by the
  broker's driver, not passed through a relay); its behavior is sealed
  transitively through the broker's and A's byte-identical event streams and
  the other four captured directions.

## Regression cases

- `REFERRAL.HANDSHAKE.GREETING_FIRST`: the greeting must be transmitted before
  reading (the broker's `NodeConnectorForBrokerReferral` replies only after
  receiving it) and must not consume a NodeLink sequence number.
- `REFERRAL.ADOPTION.THREE_OBJECTS`: exactly three driver objects, indexed by
  the message's declared fields (broker buffer, referrer transport, referrer
  buffer).
- `BYPASS.OUTBOUND.OWNERS_REGISTRATION`: the outbound bypass must register
  `owners[(target link, new sublink)]` (the inbound `BypassPeerWithLink` path
  already did); without it, the broker's subsequent parcels on sublink 12 would
  be classified "parcel for unbound sublink".
- `LINK_MEMORY.SCOPE.PER_LINK_FRAGMENTS`: parcel fragments are resolved against
  the link they arrived on (`memory_for(link_id)`) — each NodeLink has its own
  primary buffer with its own buffer id 0.
