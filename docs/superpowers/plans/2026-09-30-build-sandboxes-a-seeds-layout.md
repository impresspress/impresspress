# Build Sandboxes A — Seeds Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the dev-sandbox seed pluggable — `examples/dev-sandbox/seeds/<name>/` with a generated manifest, `build.sh --seed NAME`, and a check that covers every seed — with no runtime change and a byte-identical blank bundle.

**Architecture:** Today's `examples/dev-sandbox/seed/` becomes `seeds/blank/`. A Python generator writes each seed's `manifest.json` from its `site/**` (the runtime's own extension table decides content types); a check script proves every committed manifest equals what the generator would write. `build.sh` stages the chosen seed into a gitignored `seed/` directory, because `[[assets.overlay]]` in `impresspress.toml` takes no parameters, and gains `--out DIR` so CI can keep two seeds' bundles side by side.

**Tech Stack:** bash, Python 3 (stdlib only), the existing `impresspress build` sealed web flow, Playwright e2e in `crates/impresspress-web/tests/e2e`.

**Spec:** `docs/superpowers/specs/2026-09-30-build-sandboxes-design.md` §5.1, §5.3, §5.5 and §12 (PR A).

## Global Constraints

- Branch from `main` only after PRs #101–#106 have merged (spec §2.10); they edit `docs/dev-sandbox.md` and the compiler README.
- The blank bundle must be unchanged by this PR: `seeds/blank/manifest.json` is today's `seed/manifest.json` byte for byte, and `dist/seed/**` after `build.sh --seed blank` equals it.
- `seed/` is a build output from now on: gitignored, never committed, always produced from `seeds/<name>/`.
- Content types come from the same table as `paths::content_type_for` (`crates/impresspress-core/src/blocks/dev/paths.rs:258`); a file whose extension that table does not know is refused by the generator, never emitted as `application/octet-stream`.
- No `sandbox` block is emitted yet: no seed has a `sandbox.json` in this PR (Plan B adds the field to the runtime first).
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; open a PR, never push to `main`.

## Review Focus

1. `build.sh --seed nosuch` — must stop with a message naming the seeds that exist, not fail deep inside `cp`. (Task 2, step 4.)
2. A file under `seeds/<x>/site/` the extension table does not know (`.DS_Store`, `LICENSE` with no extension) — the generator must refuse with the file named; the check must refuse a manifest that lists one. (Task 1, step 9.)
3. `--out` naming a directory that already exists — it is replaced; `--out` equal to the default `dist/` must be refused rather than `mv`'d onto itself. (Task 2, step 5.)
4. A seed directory with no `manifest.json` — the check must fail naming the directory, not silently skip it. (Task 1, step 8.)
5. A manifest whose entries are correct but out of path order — the check must fail (the runtime does not care, but "regenerate" is the only way the file is ever written). (Task 1, step 8.)

---

### Task 1: Move the seed and add the generator and the check

**Files:**
- Move: `examples/dev-sandbox/seed/**` → `examples/dev-sandbox/seeds/blank/**`
- Create: `examples/dev-sandbox/seeds/seedlib.py`
- Create: `examples/dev-sandbox/seeds/write-manifest.py`
- Create: `examples/dev-sandbox/seeds/check-seeds.py`

**Interfaces:**
- Produces: `seedlib.build_manifest(seed_dir: Path) -> dict` (the exact JSON object the manifest holds), `seedlib.site_entries(site_dir: Path) -> list[dict]`, `seedlib.CONTENT_TYPES`, `seedlib.extension_of(name) -> str`. `write-manifest.py <name>` writes `seeds/<name>/manifest.json`; `check-seeds.py` exits non-zero with named problems.

- [ ] **Step 1: Move the directory**

```bash
cd examples/dev-sandbox
mkdir -p seeds
git mv seed seeds/blank
ls seeds/blank seeds/blank/site
```
Expected: `manifest.json  site` and `index.html  styles.css`.

- [ ] **Step 2: Write the shared library**

Create `examples/dev-sandbox/seeds/seedlib.py`:

```python
"""What a seed directory is, for the generator and the check.

A seed is `seeds/<name>/`: a `site/` tree that becomes generation 0 of a
fresh sandbox, and a `manifest.json` that lists every file of it with the
sha256, size and content type `impresspress-core::blocks::dev::seed` verifies
at boot. The manifest is generated from the tree (`write-manifest.py`) and
checked against it (`check-seeds.py`); it is never hand-edited.
"""
import hashlib
import json
import pathlib

# Mirrors `paths::content_type_for` in
# crates/impresspress-core/src/blocks/dev/paths.rs (the runtime's own table).
# The importer checks every declared type against that function, so an entry
# that disagrees is refused on the first boot — and caught by the e2e job
# that boots the seed. Keep the two in step.
CONTENT_TYPES = {
    "html": "text/html; charset=utf-8",
    "css": "text/css; charset=utf-8",
    "js": "application/javascript; charset=utf-8",
    "mjs": "application/javascript; charset=utf-8",
    "json": "application/json",
    "svg": "image/svg+xml",
    "png": "image/png",
    "jpg": "image/jpeg",
    "jpeg": "image/jpeg",
    "gif": "image/gif",
    "webp": "image/webp",
    "ico": "image/x-icon",
    "txt": "text/plain; charset=utf-8",
    "md": "text/plain; charset=utf-8",
    "rs": "text/plain; charset=utf-8",
    "toml": "text/plain; charset=utf-8",
    "wasm": "application/wasm",
    "woff2": "font/woff2",
}

SCHEMA_VERSION = 1


def extension_of(name: str) -> str:
    """The lowercase extension of a file name, or "" — a leading dot does not
    start an extension (`.gitignore` has none), as `paths::extension_of`."""
    dot = name.rfind(".")
    if dot <= 0:
        return ""
    return name[dot + 1 :].lower()


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def site_entries(site_dir: pathlib.Path) -> list:
    """Every file under `site_dir`, in path order (the order the runtime's
    BTreeMap manifest iterates in), as manifest entries."""
    files = [p for p in site_dir.rglob("*") if p.is_file()]
    files.sort(key=lambda p: p.relative_to(site_dir).as_posix())
    entries = []
    for file in files:
        rel = file.relative_to(site_dir).as_posix()
        ext = extension_of(file.name)
        if ext not in CONTENT_TYPES:
            raise SystemExit(
                f"{file}: no content type for extension {ext!r} — the runtime would serve it as "
                f"application/octet-stream. Rename the file, or extend CONTENT_TYPES in step with "
                f"paths::content_type_for."
            )
        data = file.read_bytes()
        entries.append(
            {
                "path": rel,
                "sha256": sha256_hex(data),
                "size": len(data),
                "content_type": CONTENT_TYPES[ext],
            }
        )
    return entries


def build_manifest(seed_dir: pathlib.Path) -> dict:
    """The manifest `seed_dir` should carry, in the field order
    `seed::SeedManifest` serializes."""
    site_dir = seed_dir / "site"
    if not site_dir.is_dir():
        raise SystemExit(f"{seed_dir}: has no site/ directory")
    return {
        "schema_version": SCHEMA_VERSION,
        "source_generation": None,
        "site": site_entries(site_dir),
        "blocks": [],
        "data": None,
    }


def render(manifest: dict) -> str:
    return json.dumps(manifest, indent=2) + "\n"
```

- [ ] **Step 3: Write the generator**

Create `examples/dev-sandbox/seeds/write-manifest.py` (make it executable: `chmod +x`):

```python
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
```

- [ ] **Step 4: Write the check**

Create `examples/dev-sandbox/seeds/check-seeds.py` (executable):

```python
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
```

- [ ] **Step 5: Regenerate the blank manifest and prove it is byte-identical**

```bash
python3 examples/dev-sandbox/seeds/write-manifest.py blank
git diff --exit-code examples/dev-sandbox/seeds/blank/manifest.json && echo IDENTICAL
```
Expected: `IDENTICAL`. If the diff is only a trailing newline or key order, the generator is now the source of truth — keep the regenerated file and note it in the commit message.

- [ ] **Step 6: Run the check**

```bash
python3 examples/dev-sandbox/seeds/check-seeds.py; echo "exit=$?"
```
Expected: `seeds/blank: manifest.json matches site/**` and `exit=0`.

- [ ] **Step 7: Negative check — a drifted file**

```bash
echo '/* drift */' >> examples/dev-sandbox/seeds/blank/site/styles.css
python3 examples/dev-sandbox/seeds/check-seeds.py; echo "exit=$?"
git checkout -- examples/dev-sandbox/seeds/blank/site/styles.css
```
Expected: a line starting `blank: site/styles.css: manifest.json declares` and `exit=1`.

- [ ] **Step 8: Negative check — a missing manifest and an out-of-order manifest**

```bash
mkdir -p examples/dev-sandbox/seeds/tmpseed/site && echo hi > examples/dev-sandbox/seeds/tmpseed/site/index.html
python3 examples/dev-sandbox/seeds/check-seeds.py; echo "exit=$?"
rm -r examples/dev-sandbox/seeds/tmpseed
python3 - <<'PY'
import json, pathlib
p = pathlib.Path("examples/dev-sandbox/seeds/blank/manifest.json")
m = json.loads(p.read_text()); m["site"].reverse(); p.write_text(json.dumps(m, indent=2) + "\n")
PY
python3 examples/dev-sandbox/seeds/check-seeds.py; echo "exit=$?"
git checkout -- examples/dev-sandbox/seeds/blank/manifest.json
```
Expected: first run prints `tmpseed: .../manifest.json: missing — run seeds/write-manifest.py tmpseed`, `exit=1`; second run prints `blank: site entries are not in path order`, `exit=1`.

- [ ] **Step 9: Negative check — an unknown extension is refused by the generator**

```bash
touch examples/dev-sandbox/seeds/blank/site/.DS_Store
python3 examples/dev-sandbox/seeds/write-manifest.py blank; echo "exit=$?"
rm examples/dev-sandbox/seeds/blank/site/.DS_Store
```
Expected: `.../.DS_Store: no content type for extension '' — ...` and `exit=1`; `manifest.json` unchanged (`git diff --exit-code` passes).

- [ ] **Step 10: Commit**

```bash
git add examples/dev-sandbox/seeds
git commit -m "dev-sandbox: move the seed to seeds/blank and generate its manifest

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `build.sh --seed NAME`, `--out DIR`, staging, and the widened check

**Files:**
- Modify: `examples/dev-sandbox/build.sh`
- Create: `examples/dev-sandbox/.gitignore`
- Modify: `examples/dev-sandbox/impresspress.toml` (the comment above the `seed` overlay)

**Interfaces:**
- Produces: `build.sh [--seed NAME] [--out DIR]` builds `dist/` (or `DIR`) from `seeds/NAME/` (default `blank`); last stdout line is the dist path. `build.sh --check` runs `seeds/check-seeds.py` and the compiler check.

- [ ] **Step 1: Ignore the staged seed**

Create `examples/dev-sandbox/.gitignore`:
```
# The seed being built, staged by build.sh from seeds/<name>/. Source lives
# under seeds/; this directory is a build output.
seed/
```

- [ ] **Step 2: Replace the inline seed check and add argument parsing to `build.sh`**

In `examples/dev-sandbox/build.sh`, replace the whole `check_seed()` function (the `python3 - "$HERE" <<'PY' … PY` block) with:

```bash
# Every seed under seeds/, not only the one being built: a manifest that has
# drifted from its files is what `seed::import` refuses at boot, and the
# check is cheap. The rules live in seeds/check-seeds.py.
check_seed() {
  log "verifying seeds/*/manifest.json against seeds/*/site/**"
  python3 "$HERE/seeds/check-seeds.py"
}

# Copy the chosen seed into seed/, the directory impresspress.toml overlays
# onto dist/seed/. `[[assets.overlay]]` takes no parameters and the CLI has
# no environment substitution for it, so choosing a seed means staging it
# under the one name the overlay knows. Only what the manifest describes is
# staged: seeds/<name>/ may carry source files (a vendor pin, say) that are
# not bundle content.
stage_seed() {
  local src="$HERE/seeds/$SEED"
  if [ ! -d "$src" ]; then
    echo "build.sh: no seed named '$SEED' under $HERE/seeds/ — available: $(ls "$HERE/seeds" | grep -v '\.py$' | tr '\n' ' ')" >&2
    exit 1
  fi
  log "staging seeds/$SEED into seed/"
  rm -rf "$HERE/seed"
  mkdir -p "$HERE/seed"
  cp "$src/manifest.json" "$HERE/seed/manifest.json"
  cp -R "$src/site" "$HERE/seed/site"
}
```

Then replace the argument handling — the block
```bash
if [ "${1:-}" = "--check" ]; then
  check_seed
  check_compiler
  exit 0
fi

check_seed
```
— with:

```bash
SEED="blank"
OUT=""
CHECK_ONLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --check) CHECK_ONLY=1 ;;
    --seed) SEED="${2:-}"; shift ;;
    --out) OUT="${2:-}"; shift ;;
    *) echo "build.sh: unknown argument '$1' (usage: build.sh [--check] [--seed NAME] [--out DIR])" >&2; exit 1 ;;
  esac
  shift
done
[ -n "$SEED" ] || { echo "build.sh: --seed needs a name" >&2; exit 1; }

if [ "$CHECK_ONLY" = 1 ]; then
  check_seed
  check_compiler
  exit 0
fi

check_seed
stage_seed
```

And at the very end, replace
```bash
log "dist ready: $(du -sh "$DIST" | cut -f1)"
echo "$DIST"
```
with
```bash
# `--out DIR` moves the finished bundle out of dist/, so a second seed can be
# built into dist/ afterwards (CI builds the bootstrap seed, moves it aside,
# then builds blank). A DIR that exists is replaced — it is a build output.
if [ -n "$OUT" ]; then
  case "$OUT" in /*) ;; *) OUT="$(pwd)/$OUT" ;; esac
  if [ "$OUT" = "$DIST" ]; then
    echo "build.sh: --out must not be the default dist/ directory" >&2
    exit 1
  fi
  rm -rf "$OUT"
  mv "$DIST" "$OUT"
  DIST="$OUT"
fi
log "dist ready ($SEED seed): $(du -sh "$DIST" | cut -f1)"
echo "$DIST"
```
(`cd "$HERE"` happens before the build, so a relative `--out` is resolved against `$HERE`.)

Update the header comment's `Usage:` block to:
```
#   examples/dev-sandbox/build.sh                  # build dist/ from seeds/blank
#   examples/dev-sandbox/build.sh --seed bootstrap  # build dist/ from seeds/bootstrap
#   examples/dev-sandbox/build.sh --seed bootstrap --out ../dist-bootstrap
#   examples/dev-sandbox/build.sh --check           # verify every seed and the compiler tree
```
and change the sentence "`--check` verifies every `seed/site/**` file against the hash and size `seed/manifest.json` declares" to "`--check` verifies every `seeds/<name>/manifest.json` against its `site/**` (seeds/check-seeds.py)".

- [ ] **Step 3: Update the overlay comment in `impresspress.toml`**

Replace the comment above `[[assets.overlay]] from = "seed"` with:
```toml
# The seed being built, overlaid onto the bundle wholesale. `seed/` is a
# build output: build.sh stages `seeds/<name>/` (default `blank`) into it,
# because this entry takes no parameters. `apply_overlays` copies a directory
# entry recursively (`crates/impresspress/src/cli/helpers/overlays.rs`).
```

- [ ] **Step 4: Check the argument handling without building**

```bash
examples/dev-sandbox/build.sh --check; echo "exit=$?"
examples/dev-sandbox/build.sh --seed nosuch 2>&1 | tail -1; echo "exit=${PIPESTATUS[0]}"
examples/dev-sandbox/build.sh --bogus 2>&1 | tail -1
```
Expected: the first prints `seeds/blank: manifest.json matches site/**` then either `compiler/dist is not built — nothing to check` or the compiler verification, `exit=0`. The second prints `build.sh: no seed named 'nosuch' under … — available: blank` and `exit=1` (it fails at `stage_seed`, before any wasm build). The third prints `build.sh: unknown argument '--bogus' …`.

- [ ] **Step 5: Check that `--out dist` is refused**

```bash
cd examples/dev-sandbox && bash -c 'HERE=$(pwd); DIST="$HERE/dist"; OUT="dist"; case "$OUT" in /*) ;; *) OUT="$(pwd)/$OUT" ;; esac; [ "$OUT" = "$DIST" ] && echo REFUSED'; cd -
```
Expected: `REFUSED` (the same comparison the script makes). The full-build proof is Task 4.

- [ ] **Step 6: Commit**

```bash
git add examples/dev-sandbox/build.sh examples/dev-sandbox/.gitignore examples/dev-sandbox/impresspress.toml
git commit -m "dev-sandbox: build.sh --seed/--out, staged seed, check every seed

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Every reference to the old path

**Files:**
- Modify: `crates/impresspress-web/tests/e2e/dev-foundations.spec.ts:100-110`
- Modify: `crates/impresspress-web/tests/e2e/fixtures/dev-sandbox.ts:25-30` (comment)
- Modify: `examples/dev-sandbox/README.md` (the "seed/" mentions in the intro, "Build" step 1, `--check`, and the `seed/data.json` section)
- Modify: `.github/workflows/ci-shared.yml` (the two `Check the dev-sandbox seed manifest` step comments, around lines 993 and 1229)

- [ ] **Step 1: The e2e spec reads the blank seed's manifest from its new home**

In `dev-foundations.spec.ts` change
```ts
  new URL('../../../../examples/dev-sandbox/seed/manifest.json', import.meta.url),
```
to
```ts
  new URL('../../../../examples/dev-sandbox/seeds/blank/manifest.json', import.meta.url),
```
and in the doc comment above it, `examples/dev-sandbox/seed/manifest.json` → `examples/dev-sandbox/seeds/blank/manifest.json`, and "`seed/site/index.html`" → "`seeds/blank/site/index.html`". In `fixtures/dev-sandbox.ts` the `WELCOME_PHRASE` comment's path becomes `examples/dev-sandbox/seeds/blank/site/index.html`.

- [ ] **Step 2: README**

In `examples/dev-sandbox/README.md`:
- Intro: "`seed/` is the welcome starter site every fresh origin boots with — `seed/manifest.json` plus `seed/site/{index.html,styles.css}` — overlaid onto `dist/seed/` wholesale by `[[assets.overlay]]`." → "`seeds/blank/` is the welcome starter site every fresh origin boots with — a generated `manifest.json` plus `site/{index.html,styles.css}`. `build.sh --seed NAME` stages `seeds/NAME/` into the gitignored `seed/`, which `[[assets.overlay]]` copies onto `dist/seed/` wholesale; `seeds/write-manifest.py NAME` regenerates a manifest after editing a seed's files."
- Build step 1: "Verifies `seed/manifest.json` against `seed/site/**`" → "Verifies every `seeds/*/manifest.json` against its `site/**` (`seeds/check-seeds.py`)".
- The `--check` paragraph: "verifies every `seed/site/**` file's sha256 and size against `seed/manifest.json`" → "verifies every seed's manifest against its files"; "Run this after editing the seed site" → "Run `seeds/write-manifest.py <name>` after editing a seed's files, then this".
- Add under "## Build": "`build.sh --seed bootstrap --out ../dist-bootstrap` builds another seed and moves the bundle aside so `dist/` stays free for the next one."
- Section heading "## `seed/data.json` — read this before adding one to this bundle": keep the heading text (the bundle path is still `seed/data.json` at runtime) but change "If a `data.json` is ever added to *this* directory's seed" to "If a `data.json` is ever added to a seed under `seeds/`".

- [ ] **Step 3: CI comments**

In both `Check the dev-sandbox seed manifest` / `Check the seed manifest and the compiler tree` step comments replace "a seed/manifest.json that has drifted from seed/site/**" with "a seeds/*/manifest.json that has drifted from its site/**". No command changes: the served URL `/seed/manifest.json` is unchanged.

- [ ] **Step 4: Grep for leftovers**

```bash
grep -rn 'dev-sandbox/seed/' --include=*.md --include=*.ts --include=*.yml --include=*.toml --include=*.sh --include=*.rs . | grep -v node_modules | grep -v target
```
Expected: no matches (a `/seed/` URL or `dist/seed/` is fine; `dev-sandbox/seed/` as a source path is not).

- [ ] **Step 5: Commit**

```bash
git add -A crates/impresspress-web/tests/e2e examples/dev-sandbox/README.md .github/workflows/ci-shared.yml
git commit -m "dev-sandbox: point every source reference at seeds/blank

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Prove the bundle is unchanged, end to end

**Files:** none modified (fix-ups only if something fails).

- [ ] **Step 1: A current CLI and the compiler dist**

From the repo root (this is the recipe `README.md` "Build" and the deploy workflow use):
```bash
(cd crates/impresspress-web && wasm-pack build --target web --release --out-dir pkg -- --locked)
cargo install --path crates/impresspress --locked --root ./out
examples/dev-sandbox/compiler/fetch-dist.sh
```
Expected: `out/bin/impresspress` exists; `fetch-dist.sh` ends with the compiler tree verified (needs `gh auth status` to succeed; the asset for `compiler/PIN.json`'s version is published).

- [ ] **Step 2: Build the blank seed and compare**

```bash
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh --seed blank | tail -1
diff examples/dev-sandbox/dist/seed/manifest.json examples/dev-sandbox/seeds/blank/manifest.json && echo SAME
diff -r examples/dev-sandbox/dist/seed/site examples/dev-sandbox/seeds/blank/site && echo SAME_SITE
git status --short examples/dev-sandbox
```
Expected: the dist path, `SAME`, `SAME_SITE`, and `git status` shows nothing under `examples/dev-sandbox/seed/` or `dist/` (both ignored).

- [ ] **Step 3: Build with `--out` and confirm `dist/` is free again**

```bash
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh --seed blank --out /tmp/dist-blank-check | tail -1
[ -f /tmp/dist-blank-check/seed/manifest.json ] && [ ! -d examples/dev-sandbox/dist ] && echo MOVED
rm -rf /tmp/dist-blank-check
```
Expected: `/tmp/dist-blank-check` then `MOVED`.

- [ ] **Step 4: Run the workspace e2e against a fresh blank build**

```bash
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh --seed blank | tail -1
python3 -m http.server 8082 -d examples/dev-sandbox/dist --bind 127.0.0.1 & echo $! > /tmp/dev-http-pid
(cd crates/impresspress-web && npm ci && npx playwright install chromium && TEST_PORT=8082 npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-workspace.spec.ts)
kill "$(cat /tmp/dev-http-pid)"
```
Expected: `3 passed` (the three tests of `dev-workspace.spec.ts`). `dev-foundations.spec.ts` additionally needs `PROOF_GUEST_WASM` (a `wasm32-wasip1` build of `experiments/browser-service-worker-blocks/guest`); CI runs it, and it reads the moved manifest path from Task 3.

- [ ] **Step 5: Open the PR**

```bash
git push -u origin HEAD
gh pr create --title "dev-sandbox: pluggable seeds under examples/dev-sandbox/seeds/" --body "$(cat <<'EOF'
Plan A of the build-sandboxes design (docs/superpowers/specs/2026-09-30-build-sandboxes-design.md §12).

- `seed/` → `seeds/blank/`; `seed/` is now a gitignored build output build.sh stages.
- `seeds/write-manifest.py` generates a seed's manifest (content types from the runtime's own table); `seeds/check-seeds.py` proves every committed manifest matches its tree and is what `build.sh --check` runs.
- `build.sh --seed NAME` and `--out DIR`.
- No runtime change; the blank bundle is byte-identical (verified with `diff -r` against `dist/seed/`).

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
