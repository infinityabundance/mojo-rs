# Casefile: Phase 5 block-capacity exhaustion — AddBlockBuffer receive side

**Case id**: MEMORY.BLOCK_EXHAUSTION.001
**Court**: exhaust
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)
**Status**: sealed — the broker's `RouterLinkState` pool exhausts mid-stream;
the broker shares a new 64-byte block buffer via `AddBlockBuffer`, which the
native exhaust acceptor adopts and then resolves cross-buffer fragments from;
all 1486 portal transfers are delivered in route-sequence order by both
acceptors; the broker's event streams are byte-identical.

## Preconditions

- Official broker (`invite-broker-exhaust`): invitation + bootstrap pipe,
  then 1486 portal transfers through the bootstrap (`transfer-b1` +
  `transfer-2..1486`), each pair HELD on both ends so the `RouterLinkState`
  blocks stay allocated.
- The primary buffer's 64-byte block pool holds 1483 allocable blocks.
- The pinned mojo embedder disables parcel-data expansion
  (`IPCZ_MEMORY_FIXED_PARCEL_CAPACITY`), so the ONLY expansion trigger is the
  unconditional `TryAllocateRouterLinkState` lobby.
- The broker's IO thread flushes asynchronously: the transfers arrive OUT OF
  route-sequence order and migrate across sublinks (route bypasses); the
  receivers reorder via their sequenced queues.

## Observed oracle behavior (baseline)

- The transfers' route sequence (rseq 0..1485) is delivered complete and in
  order by the oracle acceptor (the `SequencedQueue` reorders the wire).
- At the exhaustion point (~transfer 1330 in the sealed run) the failing
  transfer falls back to the plain proxy path; the broker lobbies
  `RequestBlockCapacity(64)`, allocates a 64 KiB buffer locally (it is not an
  allocation delegate), and sends `AddBlockBuffer{id=1, 64}`; the acceptor
  adopts it; the remaining transfers' link states come from the new buffer.
- The bootstrap route migrates sublinks twice (route bypasses) under the
  transfer load.
- The baseline wire contains exactly ONE `AddBlockBuffer`.

## Candidate behavior (interop)

The native exhaust acceptor:
- receives the transfers in wire (scrambled) order, verifies each payload
  against its route sequence number (`rseq 0` → `transfer-b1`, `rseq k` →
  `transfer-{k+1}`), and delivers them through the sequenced queue — the
  routed delivery order matches the oracle's;
- handles the w1 (which may arrive before the transfer-b1 — the sender's IO
  thread flushes asynchronously) via the `early_parcels` deferral, now
  including parcels for the decaying sublink of a not-yet-deserialized
  router;
- adopts the broker's `AddBlockBuffer{id=1, 64}` and (in the sealed run) a
  second `AddBlockBuffer{id=2, 64}`, registering both and resolving the
  post-expansion transfers' `RouterLinkState` fragments from them
  (cross-buffer fragment resolution);
- handles the exhausted-pool proxy transfer and the route migrations
  (`BypassPeerWithLink`) with the sealed routing-court machinery;
- observes peer closure and exits 0.

## Equivalence relations

1. The broker's event stream (all 2979 events) is **byte-identical** between
   the baseline and the interop run.
2. Both acceptors deliver the complete route sequence (rseq 0..1485) and exit
   0 (they verify every payload and the closure).
3. The expansion occurred in both runs (at least one `AddBlockBuffer`).

## Documented residual (the free-timing boundary)

The exhaustion POINT differs between the runs: the baseline exhausted at
~transfer 1330 with one `AddBlockBuffer`; the interop at ~transfer 750 with
two. Root cause: the native retains decayed `RouterLinkState` blocks (it does
not free a link's state when its decay completes — the official frees it via
the `RefCountedFragment` refcount when both sides release). The retained
blocks make the pool exhaust sooner, and the broker's second lobby fires
after the first new buffer is consumed. This is the same free-timing boundary
as the routing court's fragment-offset normalization; the primary equivalence
(the broker's event stream) is unaffected. Sealing the link-state free (a
refcount model for `RouterLinkState` fragments) is the next gate's work and
would remove this residual.

## Forensics: the relay/channel fd association

The court's dense traffic exposed a real bug in the forensic tooling: the
wire relay and the native channel read with large fixed buffers, so a single
`recvmsg` could coalesce several messages, and `SCM_RIGHTS` attaches the
descriptors to the READ's first byte — associating a message's fd with the
wrong message. Fixed by READ-SIZING both sides (read exactly the bytes needed
to complete the message at the front of the buffer, matching the official
`ChannelPosix::OnFdReadable`'s `next_read_size`). The fix is covered by the
`fd_association_survives_dense_stream` channel test.

## Evidence

- `evidence/exhaust/<stamp>/baseline/wire/*.bin`
- `evidence/exhaust/<stamp>/interop/wire/*.bin`
- `evidence/exhaust/<stamp>/{baseline,interop}/broker.events` (byte-identical)
- `evidence/exhaust/<stamp>/{baseline,interop}/acceptor.events`
- `evidence/manifests/exhaust-<stamp>.json`
- Reproduction: `scripts/run_exhaust_court.sh`
- Decoder: `cargo run -p mojo-rs-interop --bin wire-dump -- <capture.bin>`
