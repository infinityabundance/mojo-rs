# Casefile: Phase 5 memory court — parcel-fragment allocation and free-list reuse

**Case id**: MEMORY.PARCEL_FRAGMENTS.001
**Court**: memory
**Reference**: Chromium `151.0.7922.105` (`bfa3579138998e2fbb981725570fa588c5b6f8cd`)
**Status**: sealed — the native memory acceptor's parcel-fragment allocation,
pool exhaustion, inline fallback, and free-list reuse are byte-observable on
the acceptor→broker wire and match the baseline **byte-for-byte** (modulo node
names and per-link sequence numbers); the broker's event streams are
byte-identical.

## Preconditions

- Official broker (`invite-broker-memory`): invitation + bootstrap pipe,
  creates `(A, B)`, transfers `B` through the bootstrap, writes `w1` on `A`.
- The primary link buffer's 256-byte block allocator region holds exactly
  **8 allocable blocks** (9 blocks including the unallocatable block 0).
- The pinned mojo embedder sets `IPCZ_MEMORY_FIXED_PARCEL_CAPACITY`
  (`mojo/core/ipcz_api.cc`, crbug.com/40876289), so
  `allow_memory_expansion_for_parcel_data_` is false: `AllocateFragment` does
  NOT lobby for parcel data, and a failed parcel-data allocation falls back to
  inline data.

## Scenario (deterministic, no sleeps)

1. the broker transfers `B` through the bootstrap pipe (`transfer-b1`) and
   writes `w1` on `A`;
2. the acceptor sends `m0..m8` (9 × 200-byte parcels) on `B'` — `m0..m7`
   consume the 8 primary 256-byte blocks; `m8`'s fragment allocation fails and
   `m8` travels **inline**;
3. the acceptor sends a `sync` marker on the bootstrap pipe; the broker reads
   `m0..m8` from `A` only after receiving it (so the 256-blocks stay allocated
   at `m8`'s put), freeing the blocks (LIFO free-list);
4. the acceptor sends the transfer-back (`B'` + handle) through the bootstrap;
   the broker extracts `B''` (local bypass completion) and does a `w2` round
   trip on `A`/`B''`;
5. the broker sends `w3` on the bootstrap; receiving it guarantees the broker
   already read (and freed) `m0..m8`;
6. the acceptor sends `m9` and `m10` on the bootstrap — fragment-backed from
   the **freed primary blocks** (`m9` reuses block 8 at offset 98048, `m10`
   reuses block 7 at offset 97792 — the LIFO free-list order);
7. the broker reads `m9`/`m10`, then closes `A`, `B''`, and the bootstrap pipe
   (`RouteClosed` propagation).

## Observed oracle behavior (baseline) — acceptor→broker wire

```
[6..13]  AcceptParcel sub 12 rseq 0..7  frag {0, 96256+256k, 256}   m0..m7
[14]     AcceptParcel sub 12 rseq 8     inline                      m8
[15]     AcceptParcel sub 15 rseq 0     frag {0, 1152, 64}          sync
[16]     AcceptParcel sub 15 rseq 1     frag {0, 1280, 64}          transfer-back
         + descriptor {new 16, proxy_peer_sublink 12}
[17]     AcceptParcel sub 15 rseq 2     frag {0, 98048, 256}        m9  (freed block 8)
[18]     AcceptParcel sub 15 rseq 3     frag {0, 97792, 256}        m10 (freed block 7)
```

## Candidate behavior (interop)

The native `memory-acceptor` emits the **identical message inventory**: same
message types, sublinks, sequence numbers, fragment descriptors (including the
exact offsets — the shared free-list state, the put-time allocation order, and
the LIFO free-reuse all match), payloads, and the same inline fallback for
`m8`. The wire captures are byte-identical after normalizing node names and
per-link sequence numbers.

## Equivalence relation

1. The broker's event stream (all 27 events) is **byte-identical** between the
   baseline and the interop run.
2. The acceptor→broker wire, decoded by `wire-dump` and normalized, is
   **identical** (message types, sublinks, fragment descriptors, payloads).
3. All four processes exit 0.

## Normalizations

1. Node names (random per run; appear in `Connect` and the transfer-back's
   `proxy_peer_node_name`).
2. Per-link sequence numbers (self-consistent per direction only).
3. The broker→acceptor wire order of two independent messages
   (`RouteClosed(0)` vs `StopProxyingToLocalPeer(1)` after the portal-0
   handshake) is scheduler-dependent; both orders occur in the baseline and
   neither is contractual (different sublinks, no ordering contract). The
   primary equivalence — the broker's event stream — is unaffected.

## What this seals

- The generalized `BlockAllocator` machinery (64/256/512/1k/2k/4k pools, CAS
  free-list) against the official byte-for-byte.
- The put-time parcel-data allocation (`Router::AllocateOutboundParcel`):
  remote primary link → shared-memory fragment; local/absent → inline.
- Pool exhaustion → inline fallback (the epoch's `FIXED_PARCEL_CAPACITY`
  behavior).
- The shared free-list LIFO reuse across processes (the broker's reads free
  blocks that the acceptor's later allocations reuse at the exact offsets).

## What this court does NOT exercise (documented boundary)

The `RequestMemory` / `ProvideMemory` / `AddBlockBuffer` capacity-expansion
round trip is implemented (send path with `pending_memory_requests_` FIFO,
receive path with share-then-register ordering, `AddBlockBuffer` both
directions, buffer-id allocation from the shared header) and unit-tested, but
no sealed court triggers it in this epoch: the mojo embedder disables
parcel-data expansion (`IPCZ_MEMORY_FIXED_PARCEL_CAPACITY`), and the only
remaining trigger — `RouterLinkState` pool exhaustion (the unconditional lobby
in `TryAllocateRouterLinkState`) — requires the proxy-bypass machinery
(`BypassPeer` outbound / `AcceptBypassLink` inbound), which is the next Phase 5
gate. The exhaustion court will seal the round trip differentially.

## Evidence

- `evidence/memory/<stamp>/baseline/wire/*.bin`
- `evidence/memory/<stamp>/interop/wire/*.bin`
- `evidence/memory/<stamp>/{baseline,interop}/broker.events` (byte-identical)
- `evidence/memory/<stamp>/{baseline,interop}/acceptor.events`
- `evidence/manifests/memory-<stamp>.json`
- Reproduction: `scripts/run_memory_court.sh`
- Decoder: `cargo run -p mojo-rs-interop --bin wire-dump -- <capture.bin>`
