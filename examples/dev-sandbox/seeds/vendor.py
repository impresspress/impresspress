#!/usr/bin/env python3
"""Vendor the files seeds/<name>/vendor.json pins into seeds/<name>/site/.

Usage:
  seeds/vendor.py <name>            download every entry; refuse any byte that
                                    does not match its pinned sha256
  seeds/vendor.py <name> --refresh  download, then rewrite the sha256 fields —
                                    the version-bump path, after editing
                                    `version` and the URLs by hand

Every entry is downloaded and checked before any file is written, so a
mismatch leaves site/ as it was. After a --refresh: run
seeds/write-manifest.py <name>, then commit vendor.json, manifest.json and the
files together. Vendored files are byte-identical to upstream; the hash is
what makes the pin mean something (check-seeds.py verifies it).
"""
import json
import pathlib
import sys
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import seedlib  # noqa: E402


def main() -> None:
    args = sys.argv[1:]
    flags = [a for a in args if a.startswith("--")]
    names = [a for a in args if not a.startswith("--")]
    if len(names) != 1 or any(f != "--refresh" for f in flags):
        raise SystemExit(__doc__)
    refresh = "--refresh" in flags
    try:
        seed = seedlib.seed_dir(names[0])
        pin = seedlib.load_pin(seed)
    except seedlib.SeedError as e:
        raise SystemExit(str(e))
    pin_path = seed / "vendor.json"
    if pin is None:
        raise SystemExit(f"{pin_path}: no such pin")

    downloads = []
    for entry in pin["files"]:
        target = seed / "site" / entry["path"]
        with urllib.request.urlopen(entry["url"], timeout=60) as response:
            data = response.read()
        actual = seedlib.sha256_hex(data)
        if refresh:
            entry["sha256"] = actual
        elif actual != entry["sha256"]:
            raise SystemExit(
                f"{entry['url']}: downloaded bytes hash to {actual}, vendor.json pins "
                f"{entry['sha256']}. Nothing written. Pass --refresh only for a deliberate bump."
            )
        downloads.append((entry, target, data))

    for entry, target, data in downloads:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        print(f"site/{entry['path']}: {len(data)} bytes, sha256 {entry['sha256']}", file=sys.stderr)
    if refresh:
        pin_path.write_text(json.dumps(pin, indent=2) + "\n")
        print(
            f"{pin_path}: hashes rewritten — now run seeds/write-manifest.py {names[0]}",
            file=sys.stderr,
        )


if __name__ == "__main__":
    main()
