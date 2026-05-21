#!/usr/bin/env python3
"""Minimal mmap reader for ee.pack.bin.v1 frames."""

from __future__ import annotations

import argparse
import json
import mmap
import struct
import sys
from pathlib import Path


MAGIC = b"EEPK"
VERSION = 1
HEADER = struct.Struct("<4sHHQQ32s")
ITEM = struct.Struct("<QII")
TRAILER = struct.Struct("<QQI")


def _blake3_digest(data: bytes) -> bytes | None:
    try:
        import blake3  # type: ignore
    except Exception:
        return None
    return blake3.blake3(data).digest()


def read_pack(path: Path, item_index: int | None) -> dict[str, object]:
    with path.open("rb") as file:
        with mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm:
            if len(mm) < HEADER.size + TRAILER.size:
                raise SystemExit("pack_bin_truncated: frame is shorter than header+trailer")
            magic, version, flags, item_count, total_bytes, content_hash = HEADER.unpack_from(mm, 0)
            if magic != MAGIC:
                raise SystemExit("pack_bin_magic_mismatch: expected EEPK")
            if version > VERSION:
                raise SystemExit(
                    f"pack_bin_version_too_new: binary pack version {version} is newer than supported version {VERSION}"
                )
            if total_bytes != len(mm):
                raise SystemExit(
                    f"pack_bin_total_bytes_mismatch: header declares {total_bytes}, file has {len(mm)}"
                )

            table_start = HEADER.size
            table_end = table_start + item_count * ITEM.size
            if table_end + TRAILER.size > len(mm):
                raise SystemExit("pack_bin_truncated: item table extends beyond frame")

            json_offset, json_len, _reserved = TRAILER.unpack_from(mm, len(mm) - TRAILER.size)
            json_end = json_offset + json_len
            if json_offset < table_end or json_end > len(mm) - TRAILER.size:
                raise SystemExit("pack_bin_invalid_offset: canonical JSON footer is outside the frame")
            canonical_json = mm[json_offset:json_end]
            digest = _blake3_digest(canonical_json)
            hash_verified = digest == content_hash if digest is not None else None
            if digest is not None and digest != content_hash:
                raise SystemExit("pack_bin_content_hash_mismatch: canonical JSON hash does not match header")

            items = []
            for index in range(item_count):
                offset, size, _entry_reserved = ITEM.unpack_from(mm, table_start + index * ITEM.size)
                end = offset + size
                if offset < table_end or end > json_offset:
                    raise SystemExit(f"pack_bin_invalid_offset: item {index} points outside item blob area")
                if item_index is None or item_index == index:
                    items.append(
                        {
                            "index": index,
                            "offset": offset,
                            "byte_len": size,
                            "content": mm[offset:end].decode("utf-8", errors="replace"),
                        }
                    )

            return {
                "schema": "ee.pack.bin.v1",
                "version": version,
                "flags": flags,
                "bytes": len(mm),
                "items": item_count,
                "content_hash": "blake3:" + content_hash.hex(),
                "hash_verified": hash_verified,
                "canonical_json_bytes": json_len,
                "slices": items,
            }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("path", type=Path)
    parser.add_argument("--item", type=int)
    args = parser.parse_args()
    print(json.dumps(read_pack(args.path, args.item), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
