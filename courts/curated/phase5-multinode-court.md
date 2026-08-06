# Casefile: Phase 5 multi-node referral court — broker + referrer A + referred B, outbound AcceptBypassLink (both mixed-language pairings)

**Case id**: ROUTING.MULTI_NODE.001
**Court**: 3node
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)
**Status**: sealed — BOTH mixed-language pairings run against the all-official
baseline: (a) interop-b: official broker + official referrer A + native
referred B (`3node-acceptor`); (b) interop-a: official broker + native
referrer A (`3node-referrer`) + official referred B. The native node
completes the full referral handshake, the broker/referrer link adoptions
with their initial portals, the re-transfer of a portal through the b2a/a2b
pipe, the **outbound** `AcceptBypassLink` (`Router::BypassPeer` →
`BypassPeerWithNewRemoteLink`) or the **referrer** serialization
(`proxy_peer_node_name` = the broker), and the X↔Y' round trip
("hello"/"world"). In both pairings the broker's AND the counterpart node's
event streams are byte-identical to the baseline, and all four captured wire
directions are structurally identical (node-name GUIDs normalized).

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

## Candidate behavior (interop-b — native B)

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

## Candidate behavior (interop-a — native A)

The native `3node-referrer` implements the referrer role
(`NodeConnectorForReferrer` + `Invitation::Send` with SHARE_BROKER):

- the Connect handshake WITHOUT the shared-memory client handshake (the
  official referrer transmits `ReferNonBroker` before the client parcel:
  baseline wire seq 0 = ReferNonBroker, seq 1 = the client parcel);
- `ReferNonBroker{referral_id=0, num_initial_portals=2, transport}` with the
  referral transport endpoint boxed as a 16-byte `Transport` object
  (`ObjectHeader{size=8, type=kTransport}` + `TransportHeader{destination_type=
  kNonBroker, ...}` — the far end is the referred node);
- `NonBrokerReferralAccepted` acceptance: the A<->B link adoption (transport +
  buffer driver objects), `EstablishWaitingRouters` on the A<->B link (side A;
  the service portal 0 + the a2b attachment bridge on portal 1), and the
  side-A stable marks;
- the transfer-y receipt over pipe_a, and the re-transfer of Y' through the
  a2b pipe with `proxy_peer_node_name` = the broker and
  `proxy_peer_sublink` = A's Y' outward sublink (12): the bypass lock records
  the FULL `NodeName` of the serialization target (the broker's
  `CanNodeRequestBypass` validates it against the shared state), so B's
  `AcceptBypassLink` is accepted by the broker and the proxy chain collapses
  exactly like the baseline;
- the forwarded `hello` is accepted without the terminal portal delivery (the
  official's app never reads it): it stays in the route's inbound queue, the
  descriptor's `next_incoming_sequence_number` stays at the unconsumed count,
  the sub-13 decay (and the side-B stable mark on sub 12) complete in its
  flush, and the proxy's flush forwards it to B;
- the bootstrap route's bridge bypass is initiated by this side (the
  `mark_bootstrap_link_stable` + flush after the transfer), collapsing the
  chain; the attachment's forced flush fires the waiting-bit wakeup
  (`FlushRouter`), so the broker's second-stage bypass completes and the
  pipe_a closure (`RouteClosed`) arrives on the migrated sublink.

The a-to-broker wire reproduces the baseline's full sequence
(`ConnectReply`, `ReferNonBroker`, shared-memory client + `RouteClosed(0,1)`,
`BypassPeerWithLink(1→14)`, `FlushRouter(14)`,
`StopProxyingToLocalPeer(14, out_len)`, ...), and the broker-to-a wire ends
with `StopProxyingToLocalPeer(1,1)`, `BypassPeerWithLink(14→15)`,
`StopProxying(12,1,0)`, `RouteClosed(15,1)` — byte-identical message
structures to the baseline.

## Equivalence relations

1. The broker's event stream is **byte-identical** between the baseline and each
   interop run (it cannot distinguish the native node from the official one).
2. In interop-b, A's event stream is **byte-identical**; in interop-a, B's is.
3. All four captured wire directions are **structurally identical** (decoded
   message sequences, message ids, sequence numbers, sublinks, fragment
   descriptors `{0,1088,64}` / `{0,1152,64}`, handle counts) modulo the
   node-name GUIDs, which are normalized.
4. The native node verifies the transfer payload (`transfer-y2b`), the `hello`
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
  the link they arrived on (`memory_for(link_id)`) and payload fragments are
  allocated from the link they will be transmitted on (`put`'s
  `write_parcel_fragment_or_inline` now takes the target link) — each NodeLink
  has its own primary buffer with its own buffer id 0.
- `REFERRER.ORDERING.REFER_BEFORE_CLIENT`: the referrer transmits
  `ReferNonBroker` before the shared-memory client handshake (baseline wire:
  ReferNonBroker seq 0, client seq 1); the Connect handshake for the referrer
  role skips the client send, which follows the referral.
- `REFERRER.ALLOWED_BYPASS_SOURCE.FULL_NAME`: the bypass lock records the FULL
  16-byte `NodeName` of the serialization target
  (`RouterLinkState::allowed_bypass_request_source` is a NodeName); the broker's
  `CanNodeRequestBypass` validates it, so a truncated (low-half-only) write
  would reject the referred node's `AcceptBypassLink` and leave the proxy
  chain intact.
- `REFERRER.UNCONSUMED_HELLO`: the forwarded `hello` is accepted WITHOUT the
  terminal portal delivery (the official's app never reads it), so the
  descriptor's `next_incoming_sequence_number` stays at the unconsumed count
  and the parcel remains in the route's queue for the proxy to forward.
- `QUEUE.LENGTH.CONTIGUOUS_PUSHED`: `GetCurrentSequenceLength` counts the
  contiguous PUSHED span (popped count + buffered run), not just the popped
  count — the official's `SequencedQueue::MaybeAdvanceCurrent` advances at
  append, and the decay checks depend on it (the side-B stable mark on the
  transferred link's state completes in the same flush as the parcel that
  triggers it).
- `TRANSMIT.PARCEL.PER_LINK_CHANNEL`: `transmit_parcel` sends over the link's
  own channel (`send_link_message_on(link.link_id, ...)`) — a parcel on the
  A<->B link must not ride the broker channel.
- `CLOSURE.WAIT.MIGRATING_SUBLINK`: the pipe_a closure arrives on the
  attachment's CURRENT primary sublink (the bootstrap bridge bypass and the
  broker's second stage migrate it), so the closure predicate is evaluated per
  message rather than captured once.

---

# Casefile addendum: Phase 5 introduction court — broker + referrer A + referred B + introduced C (ids 10–13)

**Case id**: ROUTING.MULTI_NODE.002
**Court**: 4node
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)
**Status**: sealed — the `EstablishLink` → `BypassPeerWithNewRemoteLink` path
for `Router::BypassPeer` when the bypass target has NO direct link, in BOTH
mixed-language pairings: (a) interop-c: official broker + official A +
official B + native C (`4node-acceptor`); (b) interop-a: official broker +
native A (`4node-referrer`) + official B + official C. In each pairing the
broker's AND the counterpart nodes' event streams are byte-identical to the
all-official baseline; the native node verifies its exchange and exits 0.

## Preconditions

- Topology: broker + referrer A + referred B + introduced C. A accepts
  invitation-1 (pipe_a), refers B (SHARE_BROKER over socket-b), adopts the
  A↔B link on `NonBrokerReferralAccepted`, creates (X, Y) locally and
  transfers Y through the a2b pipe (the WithLocalPeer serialization over the
  direct link — `proxy_already_bypassed`, no proxy peer rolled in). B
  accepts invitation-2 (INHERIT_BROKER), adopts the B↔A link, refers C
  (SHARE_BROKER over socket-c); C accepts invitation-3 (INHERIT_BROKER).
- A's Y transfer gives B's router Y' an outward link on the A↔B link. B
  re-transfers Y' through the b2c pipe with `proxy_peer_node_name` = A (the
  remote of Y''s own outward link) and `proxy_peer_sublink` = its sublink.
- C's new router Y'' calls `BypassPeer(A)` and has NO link to A: it sends
  `RequestIntroduction` (id 10) to the broker. The broker replies
  `AcceptIntroduction` (id 11) to both C (link_side = A) and A (link_side =
  B), carrying a transport + memory pair. C adopts the C↔A link, completes
  the bypass with `AcceptBypassLink` (id 31) over it
  (`BypassPeerWithNewRemoteLink`), and the X↔Y'' "hello"/"world" round trip
  crosses the new link.
- Transport layout (per the court harness): broker ↔ A, broker ↔ B (the
  referral transport), broker ↔ C (the referral transport) all relayed;
  the A↔B and B↔C links are direct socketpairs created by the broker
  (not relayed); the introduced C↔A link is a direct socketpair created by
  the broker (not relayed).

## Observed oracle behavior (baseline)

The relayed broker-link wires (see the probe capture, `evidence/4node/probe/`):

- `broker-to-a`: `Connect`, `NonBrokerReferralAccepted`, then the pipe_a
  bridge-bypass exchange (`StopProxyingToLocalPeer(1)`,
  `BypassPeerWithLink(12→13)`), `AcceptIntroduction(name=<C>, link_side=1,
  remote_type=1, transport + memory)`, `RouteClosed(13, 0)`.
- `a-to-broker`: `ConnectReply`, `ReferNonBroker`, shared-memory client,
  `RouteClosed(0,1)`, the pipe_a bypass exchange (`BypassPeerWithLink(1→12)`,
  `FlushRouter(12)`, `StopProxyingToLocalPeer(12,0)`), `AcceptParcel(13)`
  (the "done" marker on the final sublink).
- `broker-to-c`: `ConnectToReferredNonBroker(name=<C>, referrer=<B>)`,
  `AcceptIntroduction(name=<A>, link_side=0, remote_type=1, transport +
  memory)`.
- `c-to-broker`: `ConnectToReferredBroker`, `RequestIntroduction(name=<A>)`.

The C↔B link (direct, not relayed) carries, in order: `RouteClosed(0,0)`
(the shared-memory service portal closure), `StopProxyingToLocalPeer(1,0)`
(B's response to C's c2b bridge self-bypass), the re-transfer
`AcceptParcel(sub=<route>, "transfer-y2" + PORTAL descriptor naming A)`, the
forwarded `hello`, `BypassPeerWithLink`, and the decaying-link closure.

## Candidate behavior (interop-c — native C)

`RoutingAcceptor::run_4node_c` (`4node-acceptor`):

- the shared `referral_accept` (INHERIT_BROKER): `ConnectToReferredBroker`
  greeting, `ConnectToReferredNonBroker` acceptance (broker link + referrer
  link adoption, initial portals, shared-memory client, bootstrap bridge,
  side-B stable marks);
- the c2b bridge self-bypass fires (the route migrates to a fresh sublink
  before the transfer arrives), so the transfer predicate matches the
  PORTAL handle type on ANY sublink, not a hard-coded initial sublink;
- `process_accept_parcel` deserializes Y'' (the descriptor names A →
  `router_bypass_peer` → no link → `establish_link` →
  `RequestIntroduction`); `on_accept_introduction` adopts the C↔A link;
  the bypass completes with `AcceptBypassLink` over the introduced link;
  `hello` arrives over the decaying B↔C link, `world` is replied over the
  new link; peer closure (`RouteClosed` on the current primary sublink) and
  the local closes follow; teardown closes tolerate a peer that already
  exited.

## Candidate behavior (interop-a — native A)

`RoutingAcceptor::run_4node_a` (`4node-referrer`):

- the referrer Connect handshake (with the broker link's `remote_name`
  recorded — the multi-link refactor regression), `ReferNonBroker` before
  the shared-memory client handshake, the pipe_a bridge-bypass trigger
  (waiting for the broker's side-A stable bit in the shared memory before
  marking — the stage-1 lock race resolved deterministically in the
  candidate's favor, exactly like the oracle acceptor), the WithLocalPeer
  transfer of Y through the a2b pipe, the `hello` on X, the
  `AcceptIntroduction` adoption (link 2), the `AcceptBypassLink` completion
  on X (`StopProxying`/`ProxyWillStop`), the `world` reply accepted on
  WHICHEVER link delivers it (through B's proxy or over the introduced
  link), the `done` marker, the pipe_a closure, and the local closes.

## Equivalence relations

1. The broker's AND the counterpart nodes' event streams are byte-identical
   between the baseline and each interop pairing.
2. The relayed broker-link wires are structurally identical modulo node-name
   GUIDs and the documented pipe_a bridge-bypass race (see Normalizers).
3. The native node verifies its exchange (`transfer-y2` payload, `hello`/
   `world`, closure) and exits 0.

## Normalizers

- Node-name GUIDs → `<name>`.
- The pipe_a bridge-bypass exchange on the broker↔A directions: the
  all-official baseline races on the initiator of each bypass stage (the
  shared `RouterLinkState` lock CAS between the broker and A), producing
  different message sequences with byte-identical event streams that
  converge on the same final sublink; the wire comparison drops the exchange
  messages (`BypassPeerWithLink`/`FlushRouter`/`StopProxyingToLocalPeer`)
  and the ordinal/sequence prefixes on those two directions only, and still
  compares the fixed-point `done`/closure traffic exactly. The other four
  relayed directions compare exactly.
- The direct (unrelayed) A↔B, B↔C, and C↔A links are sealed transitively
  through the byte-identical event streams and the relayed directions.

## Regression cases

- `INTRODUCTION.WIRE.PACKED_ACCEPT`: `AcceptIntroduction` V0 packs single
  byte `link_side`/`remote_node_type` at offset 16 (not u32s), with
  `remote_protocol_version` at 20 and the transport/memory driver objects;
  the V1 `remote_features` field is an OFFSET to the features array.
- `TRANSFER.SUBLINK.DYNAMIC`: the transfer parcel's arrival sublink is the
  route's CURRENT sublink after the bridge self-bypass — predicates match
  the PORTAL handle type, not a fixed initial sublink.
- `REFERRER.LINK_REMOTE_NAME`: the referrer's Connect handshake records the
  broker link's `remote_name` in the `links` map (the re-transfer's
  `proxy_peer_node_name` is the remote of the portal's OWN outward link).
- `BYPPASS.LOCK_RACE.DETERMINISTIC_WIN`: the stage-1 bridge-bypass lock race
  is decided by the shared `RouterLinkState` CAS; the candidate waits for
  the broker's side-A stable bit to be observable before marking its own
  side, reproducing the oracle acceptor's deterministic win.
- `WORLD.LINK_AGNOSTIC`: the reply may arrive over the direct link through
  the peer's proxy when the far end replies before its bypass settles; the
  read is a Mojo read and link-agnostic.
- `TEARDOWN.BROKEN_TRANSPORT`: closing a route whose peer already exited is
  not an error (the official driver's `Transmit` failure is asynchronous).
