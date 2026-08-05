# mojo-rs-interop

Interoperability test clients and the native ipcz acceptor.

**Status: Interoperable (Phase 3 gate sealed).** The native Rust
`ipcz-acceptor` completes the official ConnectNode handshake with the pinned
official broker and exchanges a message plus a wrapped descriptor in each
direction through the bootstrap pipe. `scripts/run_interop_court.sh` shows the
official broker's event stream is byte-identical whether its peer is the
official oracle acceptor or the native acceptor, with both peers exiting 0.
Evidence: `evidence/interop/`, `evidence/manifests/interop-<stamp>.json`.

## Components

* `src/ipcz/wire.rs` — channel framing (`IpczHeader`) and message building.
* `src/ipcz/messages.rs` — NodeLink message decode/encode (byte-exact golden
  tests against captured official wire in `testdata/ipcz/`).
* `src/ipcz/channel.rs` — socket transport with `SCM_RIGHTS` tracking.
* `src/ipcz/link_memory.rs` — the shared primary buffer, fragments,
  `RouterLinkState`, the `BlockAllocator` free-list.
* `src/ipcz/acceptor.rs` — the native non-broker node state machine.
* `src/bin/ipcz-acceptor.rs` — oracle-compatible harness
  (`<socket-fd> <events.jsonl>`).
* `src/bin/wire-relay.rs` — man-in-the-middle wire capture.
* `src/bin/candidate-harness.rs` — in-process system-court harness.

Mixed-language pairings required by the bindings court:

```text
C++ client -> C++ oracle server      (sealed)
C++ client -> Rust native server     (sealed: invite-broker ⇄ ipcz-acceptor)
Rust native client -> C++ oracle server
Rust native client -> Rust native server
```

The remaining pairings require the Phase 5 routing layer (a native broker and
native client-side nodes).
