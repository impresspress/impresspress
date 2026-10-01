#!/usr/bin/env python3
"""Verify every seed under seeds/: its manifest.json is exactly what
write-manifest.py would write from its tree, byte for byte.

A seed is a directory whose name passes seedlib.SEED_NAME; anything else
under seeds/ (__pycache__, a dotted directory) is not a seed and is skipped.
Each seed is checked on its own: a seed that cannot be read or generated
reports its own line and the next seed is still checked.

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


def problems_for(seed: pathlib.Path) -> list:
    manifest_path = seed / "manifest.json"
    if not manifest_path.is_file():
        return [f"{manifest_path}: missing — run seeds/write-manifest.py {seed.name}"]
    regenerate = f"regenerate with seeds/write-manifest.py {seed.name}"
    try:
        committed = json.loads(manifest_path.read_text())
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        return [f"{seed.name}: manifest.json is not valid JSON ({e}) — {regenerate}"]
    if not isinstance(committed, dict):
        return [f"{seed.name}: manifest.json is not a JSON object — {regenerate}"]
    try:
        expected = seedlib.build_manifest(seed)
    except seedlib.SeedError as e:
        return [f"{seed.name}: {e}"]
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
    if not problems and manifest_path.read_text() != seedlib.render(expected):
        problems.append("manifest.json differs from what write-manifest.py writes — regenerate")
    return [f"{seed.name}: {p}" for p in problems]


def main() -> None:
    seeds = seedlib.seed_dirs()
    if not seeds:
        raise SystemExit(f"{seedlib.SEEDS_DIR}: no seed directories")
    problems = []
    for seed in seeds:
        found = problems_for(seed)
        problems.extend(found)
        if not found:
            print(f"seeds/{seed.name}: manifest.json matches its tree", file=sys.stderr)
    if problems:
        raise SystemExit("\n".join(problems) + "\n\nRegenerate with seeds/write-manifest.py <name>.")


if __name__ == "__main__":
    main()
