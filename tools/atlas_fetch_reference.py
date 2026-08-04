#!/usr/bin/env python3
"""Fetch pinned Mojo C System API headers from the pinned Chromium revision and
generate the machine-readable API inventory used by the atlas and ABI courts.

Ground truth source: the pinned Chromium tag (see atlas/pins.json), fetched
directly from chromium.googlesource (gitiles). The headers are also copied into
atlas/reference/ so the ABI court can compile unchanged clients against them.

Usage:
    tools/atlas_fetch_reference.py --tag 151.0.7922.105 --commit <sha> --out atlas/reference
"""
from __future__ import annotations

import argparse
import base64
import json
import re
import sys
import urllib.request
from pathlib import Path

GITILES = "https://chromium.googlesource.com/chromium/src/+/refs/tags/{tag}/{path}?format=TEXT"

# Epoch-151 actual layout of mojo/public/c/system/ (verified against the pinned
# tree; several historical headers were consolidated: data_pipe_consumer.h +
# data_pipe_producer.h -> data_pipe.h, shared_buffer.h -> buffer.h, handle.h ->
# types.h, and wait_set.h/abi.h were removed).
HEADERS = [
    "mojo/public/c/system/buffer.h",
    "mojo/public/c/system/core.h",
    "mojo/public/c/system/data_pipe.h",
    "mojo/public/c/system/functions.h",
    "mojo/public/c/system/invitation.h",
    "mojo/public/c/system/macros.h",
    "mojo/public/c/system/message_pipe.h",
    "mojo/public/c/system/platform_handle.h",
    "mojo/public/c/system/quota.h",
    "mojo/public/c/system/system_export.h",
    "mojo/public/c/system/trap.h",
    "mojo/public/c/system/types.h",
    "mojo/public/c/system/thunks.h",
]

# Optional extras.
OPTIONAL_HEADERS = [
    "mojo/public/c/system/README.md",
]


def fetch(tag: str, path: str) -> bytes:
    url = GITILES.format(tag=tag, path=path)
    with urllib.request.urlopen(url, timeout=60) as r:
        data = r.read()
    # gitiles TEXT format: base64 with a leading ")]}'" JSONP guard.
    text = data.decode("utf-8", errors="replace").strip()
    if text.startswith(")]}'"):
        text = text[4:].strip()
    return base64.b64decode(text)


def parse_functions(text: str) -> list[dict]:
    """Extract Mojo* function declarations from a header."""
    funcs = []
    # MojoResult MojoXxx( ... );
    pattern = re.compile(
        r"MojoResult\s+(Mojo[A-Za-z0-9_]+)\s*\(([^;]*?)\)\s*;", re.S
    )
    for m in pattern.finditer(text):
        params = []
        for p in re.split(r",(?![^()]*\))", m.group(2)):
            p = p.strip()
            if not p:
                continue
            p = re.sub(r"\s+", " ", p)
            params.append(p)
        funcs.append(
            {"name": m.group(1), "return": "MojoResult", "params": params}
        )
    return funcs


def parse_structs(text: str) -> list[dict]:
    structs = []
    # struct [MOJO_ALIGNAS(8)] MojoXxx { ... };
    pattern = re.compile(
        r"struct\s+(?:MOJO_ALIGNAS\(\d+\)\s+)?(Mojo[A-Za-z0-9_]+)\s*\{(.*?)\}\s*;", re.S
    )
    for m in pattern.finditer(text):
        body = m.group(2)
        fields = []
        for fm in re.finditer(
            r"(\w[\w\s\*]+?)\s+(\w+)\s*;", body
        ):
            fields.append(
                {
                    "type": re.sub(r"\s+", " ", fm.group(1)).strip(),
                    "name": fm.group(2),
                }
            )
        structs.append({"name": m.group(1), "fields": fields})
    # MOJO_STATIC_ASSERT(sizeof(struct MojoXxx) == N, ...) — compile-time sizes.
    size_pattern = re.compile(
        r"MOJO_STATIC_ASSERT\(sizeof\(struct\s+(Mojo[A-Za-z0-9_]+)\)\s*==\s*(\d+)",
        re.S,
    )
    for m in size_pattern.finditer(text):
        structs.append({"name": m.group(1), "size_bytes": int(m.group(2)), "from_static_assert": True})
    return structs


def parse_flags(text: str) -> list[dict]:
    """typedef uint32_t MojoXxxFlags; — flag/option types."""
    flags = []
    for m in re.finditer(r"typedef\s+uint32_t\s+(Mojo[A-Za-z0-9_]*Flags)\s*;", text):
        flags.append({"name": m.group(1)})
    return flags


def parse_constants(text: str) -> list[dict]:
    consts = []
    for m in re.finditer(r"#define\s+(MOJO_[A-Z0-9_]+)\s+([^\n]+)", text):
        value = m.group(2).strip()
        # Skip include guards / includes, which are not ABI constants.
        if value.startswith("#include") or value.startswith("\""):
            continue
        consts.append({"name": m.group(1), "value": value})
    return consts


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True)
    ap.add_argument("--commit", required=True)
    ap.add_argument("--out", default="atlas/reference")
    ap.add_argument("--inventory", default="atlas/api/mojo-c-system-api.json")
    args = ap.parse_args()

    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    inventory = {
        "schema_version": 1,
        "generated_at": None,
        "chromium_tag": args.tag,
        "chromium_commit": args.commit,
        "headers": [],
        "functions": [],
        "structs": [],
        "flags": [],
        "constants": [],
    }

    failed = []
    for path in HEADERS + OPTIONAL_HEADERS:
        try:
            content = fetch(args.tag, path)
        except Exception as e:  # noqa: BLE001
            if path in OPTIONAL_HEADERS:
                print(f"skip (optional): {path}: {e}", file=sys.stderr)
                continue
            failed.append(path)
            print(f"FAIL: {path}: {e}", file=sys.stderr)
            continue
        rel = Path(path)
        dest = out_dir / rel.name
        dest.write_bytes(content)
        text = content.decode("utf-8", errors="replace")
        inventory["headers"].append(
            {
                "path": path,
                "local": str(dest),
                "sha256": hashlib_sha256(content),
                "bytes": len(content),
            }
        )
        inventory["functions"].extend(parse_functions(text))
        inventory["structs"].extend(parse_structs(text))
        inventory["flags"].extend(parse_flags(text))
        inventory["constants"].extend(parse_constants(text))

    if failed:
        print(f"FATAL: failed to fetch: {failed}", file=sys.stderr)
        return 1

    # Deduplicate preserving order.
    seen_f = set()
    funcs = []
    for f in inventory["functions"]:
        if f["name"] not in seen_f:
            seen_f.add(f["name"])
            funcs.append(f)
    inventory["functions"] = funcs

    inv_path = Path(args.inventory)
    inv_path.parent.mkdir(parents=True, exist_ok=True)
    inv_path.write_text(json.dumps(inventory, indent=2) + "\n")
    print(
        f"inventory: {len(inventory['functions'])} functions, "
        f"{len(inventory['structs'])} structs, {len(inventory['flags'])} flag types, "
        f"{len(inventory['constants'])} constants -> {inv_path}"
    )
    return 0


def hashlib_sha256(data: bytes) -> str:
    import hashlib

    return hashlib.sha256(data).hexdigest()


if __name__ == "__main__":
    sys.exit(main())
