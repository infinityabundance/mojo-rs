#!/usr/bin/env python3
"""Fetch the bindings wire-format internal header (the canonical layout
definitions for the Mojo wire format) from the pinned revision."""
from __future__ import annotations

import argparse
import base64
import json
import sys
import urllib.request
from pathlib import Path

GITILES = "https://chromium.googlesource.com/chromium/src/+/refs/tags/{tag}/{path}?format=TEXT"

FILES = [
    "mojo/public/cpp/bindings/lib/bindings_internal.h",
    "mojo/public/cpp/bindings/lib/serialization.h",
    "mojo/public/cpp/bindings/lib/array_internal.h",
    "mojo/public/cpp/bindings/lib/map_internal.h",
    "mojo/public/cpp/bindings/lib/string_internal.h",
    "mojo/public/cpp/bindings/lib/union_internal.h",
    "mojo/public/cpp/bindings/lib/validation_errors.h",
    "mojo/public/cpp/bindings/lib/message_framer.h",
]


def fetch(tag: str, path: str) -> bytes:
    url = GITILES.format(tag=tag, path=path)
    with urllib.request.urlopen(url, timeout=60) as r:
        data = r.read()
    text = data.decode("utf-8", errors="replace").strip()
    if text.startswith(")]}'"):
        text = text[4:].strip()
    return base64.b64decode(text)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True)
    ap.add_argument("--out", default="atlas/reference/wire")
    args = ap.parse_args()

    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    manifest_path = out_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else {"chromium_tag": args.tag, "files": []}
    existing = {f["path"] for f in manifest["files"]}

    for path in FILES:
        try:
            content = fetch(args.tag, path)
        except Exception as e:  # noqa: BLE001
            print(f"skip: {path}: {e}", file=sys.stderr)
            continue
        dest = out_dir / Path(path).name
        dest.write_bytes(content)
        if path not in existing:
            manifest["files"].append(
                {
                    "path": path,
                    "local": str(dest),
                    "bytes": len(content),
                    "sha256": __import__("hashlib").sha256(content).hexdigest(),
                }
            )
        print(f"fetched {path} -> {dest}")

    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"manifest: {len(manifest['files'])} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
