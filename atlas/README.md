# mojo-rs Atlas

Machine-readable cartography of the pinned Chromium epoch and the mojo-rs
implementation plan. **Generated data is derived from the pinned revision via
`tools/` — regenerate, never hand-edit.**

## Ground truth

| File | What it pins | Source |
|---|---|---|
| `pins.json` | Chromium tag/commit, depot_tools, rust, tooling, base image | verified 2026-08-04 |
| `api/mojo-c-system-api.json` | C System API: 45 functions, structs + compile-time sizes, flags, constants | parsed from pinned headers |
| `reference/` | Pinned C headers (raw, hash-verified) | `tools/atlas_fetch_reference.py` |
| `reference/wire/` | Wire-format headers (message layouts, alignment) | `tools/atlas_fetch_wire_reference.py` |
| `components.json` | Component atlas (CoreIpcz epoch) | pinned tree |
| `wire/wire-format.json` | Wire format inventory | pinned headers |
| `state-machines.json` | State machines to model | analysis |
| `platform.json` | Linux platform dependencies | analysis |
| `tests.json` | Upstream test inventory | pinned tree |
| `feature-matrix.json` | Capability matrix with sealed statuses | this repo |

## Regeneration

```bash
python3 tools/atlas_fetch_reference.py --tag 151.0.7922.105 --commit <sha> \
    --out atlas/reference --inventory atlas/api/mojo-c-system-api.json
python3 tools/atlas_fetch_wire_reference.py --tag 151.0.7922.105 --out atlas/reference/wire
```

The committed JSON inventories are generated artifacts; their provenance is
recorded inside each file.

## Key epoch facts (verified)

* **CoreIpcz**: in this epoch Mojo Core is a driver over ipcz. Routing, ports,
  and endpoint transfer semantics come from ipcz.
* **C System ABI**: 45 exported functions in this epoch; the C API is dispatched
  through `MojoSystemThunks` (thunks.h/cc).
* **Wire message headers**: v0 = 24 B, v1 = 32 B (request_id), v2 = 48 B
  (payload + interface ids), v3 = 56 B (timeticks), all packed.
* **Struct header**: `{num_bytes: u32, version: u32}` (8 B); arrays
  `{num_bytes, num_elements}`; pointers are 8-byte relative offsets (0 = null).
* **Result codes**: `MOJO_RESULT_*` 0..17+ as `typedef uint32_t MojoResult`.

## Status discipline

The feature matrix is the only place where capability status is authoritative.
Statuses move up the ladder only with receipts.
