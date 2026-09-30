#!/usr/bin/env python3
"""Write seeds/<name>/manifest.json from seeds/<name>/site/**.

Usage: seeds/write-manifest.py <name>

Run it after every edit under site/ and commit the result; `build.sh --check`
(seeds/check-seeds.py) fails on a manifest that does not match its tree.
"""
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import seedlib  # noqa: E402


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    seed = pathlib.Path(__file__).resolve().parent / sys.argv[1]
    if not seed.is_dir():
        raise SystemExit(f"{seed}: no such seed")
    manifest = seedlib.build_manifest(seed)
    (seed / "manifest.json").write_text(seedlib.render(manifest))
    print(f"{seed / 'manifest.json'}: {len(manifest['site'])} site file(s)", file=sys.stderr)


if __name__ == "__main__":
    main()
