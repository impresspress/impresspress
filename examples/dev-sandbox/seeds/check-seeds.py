#!/usr/bin/env python3
"""Verify every seed under seeds/: its manifest.json is exactly what
write-manifest.py would write from its tree.

Usage: seeds/check-seeds.py

Exits non-zero naming every problem. This is what `build.sh --check` runs,
and what a plain `build.sh` runs before building anything: a manifest that
has drifted from its files is exactly what `seed::import` refuses at boot,
and a fresh origin that refuses its seed boots empty.
"""
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import seedlib  # noqa: E402

SEEDS = pathlib.Path(__file__).resolve().parent


def problems_for(seed: pathlib.Path) -> list:
    manifest_path = seed / "manifest.json"
    if not manifest_path.is_file():
        return [f"{manifest_path}: missing — run seeds/write-manifest.py {seed.name}"]
    committed = json.loads(manifest_path.read_text())
    expected = seedlib.build_manifest(seed)
    problems = []
    declared = {e["path"]: e for e in committed.get("site", [])}
    actual = {e["path"]: e for e in expected["site"]}
    for path, entry in declared.items():
        if path not in actual:
            problems.append(f"site/{path}: declared in manifest.json but missing under site/")
        elif entry != actual[path]:
            problems.append(
                f"site/{path}: manifest.json declares {entry}, the file is {actual[path]}"
            )
    for path in sorted(actual.keys() - declared.keys()):
        problems.append(
            f"site/{path}: under site/ but not declared, so it would never be imported"
        )
    if [e["path"] for e in committed.get("site", [])] != sorted(declared):
        problems.append("site entries are not in path order")
    if committed != expected and not problems:
        problems.append("manifest.json differs from what write-manifest.py writes (header fields)")
    return [f"{seed.name}: {p}" for p in problems]


def main() -> None:
    seeds = sorted(p for p in SEEDS.iterdir() if p.is_dir() and not p.name.startswith("__"))
    if not seeds:
        raise SystemExit(f"{SEEDS}: no seed directories")
    problems = []
    for seed in seeds:
        found = problems_for(seed)
        problems.extend(found)
        if not found:
            print(f"seeds/{seed.name}: manifest.json matches site/**", file=sys.stderr)
    if problems:
        raise SystemExit("\n".join(problems) + "\n\nRegenerate with seeds/write-manifest.py <name>.")


if __name__ == "__main__":
    main()
