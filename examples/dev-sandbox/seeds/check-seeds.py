#!/usr/bin/env python3
"""Verify every seed under seeds/: its manifest.json is exactly what
write-manifest.py would write from its tree, byte for byte, and every file its
vendor.json (when it has one) pins is under site/ with the pinned sha256.

"Its tree" includes what the manifest's sandbox block is generated from:
sandbox.json, guide.md, and the llms.txt seedlib builds out of
seeds/llms-preamble.md and that guide. An edit to any of them without
regenerating is a manifest whose declared hash the importer would refuse.

sandbox.json also holds the one thing the manifest does not carry: the title
of the seed's boot page, which build.sh hands to the bundler. A seed without
one, or two seeds with the same one, fail here.

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
    # The sandbox block by name, before the byte comparison below: "differs"
    # alone does not say that the stale half is the llms.txt every seed
    # shares a preamble for, which no file in THIS seed's directory changed.
    declared_sandbox = committed.get("sandbox")
    expected_sandbox = expected.get("sandbox")
    if declared_sandbox != expected_sandbox:
        for key in ("guide", "llms"):
            before = (declared_sandbox or {}).get(key)
            after = (expected_sandbox or {}).get(key)
            if before != after:
                source = (
                    f"{seedlib.GUIDE_PATH}"
                    if key == "guide"
                    else f"{seedlib.LLMS_PREAMBLE.name} + {seedlib.GUIDE_PATH}"
                )
                problems.append(
                    f"sandbox.{key}: manifest.json declares {before}, {source} gives {after}"
                )
    if not problems and manifest_path.read_text() != seedlib.render(expected):
        problems.append("manifest.json differs from what write-manifest.py writes — regenerate")
    return [f"{seed.name}: {p}" for p in problems]


def vendor_problems(seed: pathlib.Path) -> list:
    """Every file vendor.json pins is present under site/ with the pinned bytes."""
    try:
        pin = seedlib.load_pin(seed)
    except seedlib.SeedError as e:
        return [f"{seed.name}: {e}"]
    if pin is None:
        return []
    problems = []
    for entry in pin["files"]:
        target = seed / "site" / entry["path"]
        if not target.is_file():
            problems.append(
                f"{seed.name}: vendor.json pins {entry['path']} but site/{entry['path']} is missing "
                f"— run seeds/vendor.py {seed.name}"
            )
        elif seedlib.sha256_hex(target.read_bytes()) != entry["sha256"]:
            problems.append(
                f"{seed.name}: site/{entry['path']} differs from the bytes vendor.json pins "
                f"({entry['url']}) — an edited vendored file is a bug; re-run seeds/vendor.py {seed.name}"
            )
    return problems


def check(seeds: list) -> list:
    """Every problem with `seeds`, one line each; a line per clean seed goes
    to stderr."""
    problems = []
    for seed in seeds:
        found = problems_for(seed) + vendor_problems(seed)
        problems.extend(found)
        if not found:
            print(
                f"seeds/{seed.name}: manifest.json matches its tree"
                + (", vendored files match vendor.json" if (seed / "vendor.json").is_file() else ""),
                file=sys.stderr,
            )
    problems.extend(seedlib.shared_titles(seeds))
    return problems


def main() -> None:
    seeds = seedlib.seed_dirs()
    if not seeds:
        raise SystemExit(f"{seedlib.SEEDS_DIR}: no seed directories")
    problems = check(seeds)
    if problems:
        raise SystemExit(
            "\n".join(problems)
            + "\n\nEach line names its fix: regenerate a manifest with seeds/write-manifest.py <name>, "
            "restore a vendored file with seeds/vendor.py <name>, give a seed its own title in its "
            "sandbox.json."
        )


if __name__ == "__main__":
    main()
