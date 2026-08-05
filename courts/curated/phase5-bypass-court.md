# Casefile: Phase 5 acceptor-initiated block-capacity exhaustion — RequestMemory/ProvideMemory/AddBlockBuffer SEND side

**Case id**: MEMORY.BLOCK_EXHAUSTION.002
**Court**: bypass
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)
**Status**: sealed — the ACCEPTOR's `SerializeNewRouterWithLocalPeer` transfers
exhaust the shared 64-byte `RouterLinkState` pool on the acceptor side; the
unconditional `TryAllocateRouterLinkState` lobby sends `RequestMemory` to the
broker; the broker's `ProvideMemory` is adopted by the acceptor, which shares
the new buffer back via `AddBlockBuffer` (the SEND side of the expansion round
trip) and resolves the remaining transfers' link states from it. The broker's
event streams are byte-identical; the send-side round trip is visible on the
acceptor→broker wire in both runs.

## Preconditions

- Official broker (`invite-broker-bypass`) and official oracle acceptor
  (`invite-acceptor-bypass`, baseline) / native `bypass-acceptor` (interop).
- The routing-court prelude: the broker transfers `b1` and writes `w1`
  (anchors the side-B stable marks and the bridge-bypass ordering).
- The acceptor then creates 1520 fresh local pairs and transfers one end of
  each through the bootstrap pipe (`SerializeNewRouterWithLocalPeer`): each
  transfer allocates ONE `RouterLinkState` from the shared 64-byte pool (held
  while both ends of the pair stay open).
- The pinned mojo embedder disables parcel-data expansion
  (`IPCZ_MEMORY_FIXED_PARCEL_CAPACITY`), so the ONLY expansion trigger is the
  unconditional `TryAllocateRouterLinkState` lobby.
- The acceptor connected as the allocation delegate (`Invitation::Accept`), so
  its `Node::AllocateSharedMemory` sends `RequestMemory` to the broker; the
  broker's `OnRequestMemory` allocates a 64 KiB buffer locally and replies
  `ProvideMemory`; the acceptor adopts it and shares it back via
  `AddBlockBuffer` (share-then-register ordering).

## Observed oracle behavior (baseline)

- The oracle acceptor runs the same WithLocalPeer transfer loop; the shared
  pool exhausts mid-loop; the failing transfer falls back to the plain proxy
  path; the acceptor's lobby fires `RequestMemory{65536}` to the broker; the
  broker replies `ProvideMemory{65536, memfd}`; the acceptor sends
  `AddBlockBuffer{id=1, 64, memfd}` and registers it; the remaining transfers
  resolve their link states from the new buffer (cross-buffer fragment
  resolution).
- The baseline wire contains exactly ONE `RequestMemory` and ONE
  `AddBlockBuffer`.
- The broker receives all 1520 transfers (`transfer-0..1519`) plus the `sync`
  marker, then closes the bootstrap pipe; the acceptor observes peer closure.

## Candidate behavior (interop)

The native `bypass-acceptor`:
- runs the identical loop; each transfer allocates its link state from the
  shared pool via `serialize_router_with_local_peer` (the new WithLocalPeer
  serialization: a central link with a fresh pool `RouterLinkState` + a
  decaying peripheral sublink, `proxy_already_bypassed=true`);
- at the exhaustion point, the failing `TryAllocateRouterLinkState` fires the
  unconditional lobby (`request_block_capacity(64)` →
  `RequestMemory{65536}`) and the transfer falls back to the plain proxy path
  with the local outward link unlocked (the official
  `SerializeNewRouterAndConfigureProxy` behavior);
- processes the broker's `ProvideMemory` in the post-put drain:
  `on_provide_memory` allocates buffer id 1 from the shared header, shares the
  buffer via `AddBlockBuffer` (transmitted BEFORE the local registration,
  matching the official share-then-register ordering), and registers it;
- resolves the post-expansion transfers' link states from buffer 1;
- sends the `sync` marker, observes the broker's `RouteClosed`, closes its
  bootstrap end, verifies an extra block buffer was registered, and exits 0.

## Equivalence relations

1. The broker's event stream (all 1529 events) is **byte-identical** between
   the baseline and the interop run.
2. Both wires carry all 1520 transfer parcels; the broker verifies every
   payload and the `sync` marker (it exits 0 only if all matched); both
   acceptors exit 0.
3. The send-side expansion round trip is present on the acceptor→broker wire
   in BOTH runs: at least one `RequestMemory` and at least one
   `AddBlockBuffer` (the interop run is the native's SEND — the sealed path).

## Documented residual (the exhaustion-point interleaving)

The exhaustion POINT in the transfer loop differs between the runs: the
oracle's concurrent IO thread frees each transfer's payload fragment as the
broker reads it, so its state allocations largely reuse freed payload blocks
(net ~1 block per transfer); the native's single-threaded loop does not wait
for the peer's frees, so its state allocations often consume fresh blocks
(net ~2 per transfer) and its pool exhausts earlier. The `RequestMemory`'s
wire position and the exact fragment offsets therefore differ. The primary
equivalence — the broker's event stream — is unaffected (the exhaustion point
is internal block accounting). This is the same free-list-reuse interleaving
documented in the routing and exhaustion courts.

## Forensics: the mojo-driver shared-buffer object encoding

The court exposed a real bug in the native's SEND side: `encode_add_block_buffer`
/ `encode_provide_memory` attached an EMPTY driver object (no serialized data)
to the message. The mojo driver's `SharedBuffer::Deserialize`
(`mojo/core/ipcz_driver/shared_buffer.cc`) requires the 40-byte serialization
(`ObjectHeader{size=8, type=kSharedBuffer=1}` + `BufferHeader{size=32,
buffer_size, mode=kUnsafe=2, padding=0, guid_low, guid_high}`); an empty
object fails deserialization, `OnAddBlockBuffer` returns false, and the
broker drops the NodeLink (all routes close — observed as `PEER_CLOSED` on
the bootstrap pipe and a `Connection reset by peer` on the native's channel).
This path had never been differentially exercised before: the exhaustion
court sealed only the RECEIVE side. Fixed by encoding the full
`SharedBuffer` object (deterministic non-zero GUID derived from the buffer
identity, so the wire is reproducible); the golden test
`encode_add_block_buffer_matches_official_capture` pins the encoding
byte-identical to the official broker's `AddBlockBuffer` (modulo the
transmit-time sequence number and the random region GUID, both documented
normalizations). The failed runs (`evidence/bypass/20260805T185330Z/`,
interop captures showing the route teardown) are preserved as forensic
receipts.

## Evidence

- `evidence/bypass/<stamp>/baseline/wire/*.bin`
- `evidence/bypass/<stamp>/interop/wire/*.bin`
- `evidence/bypass/<stamp>/{baseline,interop}/broker.events` (byte-identical)
- `evidence/bypass/<stamp>/{baseline,interop}/acceptor.events`
- `evidence/manifests/bypass-<stamp>.json`
- Reproduction: `scripts/run_bypass_court.sh`
- Decoder: `cargo run -p mojo-rs-interop --bin wire-dump -- <capture.bin>`
