"""What a seed directory is, for the generator and the check.

A seed is `seeds/<name>/`: a `site/` tree that becomes generation 0 of a
fresh sandbox, and a `manifest.json` that lists every file of it with the
sha256, size and content type `impresspress-core::blocks::dev::seed` verifies
at boot. A seed may also carry `sandbox.json` (its template name, the title
of its boot page and the prompt `/b/dev` suggests) and `guide.md` (the
site-authoring guide `dev_read_reference` serves); the manifest's `sandbox`
block is built from the two. The title is the one thing in `sandbox.json`
the manifest does not carry: it is what the static boot page is headed with,
before any runtime exists to read a manifest, so `build.sh` hands it to the
bundler (`boot_title`). The manifest is generated from the tree (`write-manifest.py`) and
checked against it (`check-seeds.py`); it is never hand-edited.

A seed with a sandbox block also has an `llms.txt` — what a reader that has
not entered the sandbox yet is told about it. It is not a file in the seed
directory: `llms_text` generates it from `seeds/llms-preamble.md` (what every
sandbox says: what this is, how to get in) followed by the seed's own
`guide.md`, so the building instructions exist once. The manifest's sandbox
block declares its hash and `build.sh` writes the file when it stages a seed.
"""
import hashlib
import json
import pathlib
import re


class SeedError(Exception):
    """A seed that cannot be generated or checked; the message names the file and the fix."""


SEEDS_DIR = pathlib.Path(__file__).resolve().parent

# The runtime's block-name rule (paths::block_name_is_valid). Plan B reuses it
# for the manifest's `template` name, so a seed directory and the template it
# names are spelled by one rule. Like the runtime, a doubled hyphen and a
# trailing one are refused (the lookaheads); use it with fullmatch.
SEED_NAME = re.compile(r"[a-z](?!.*--)(?!.*-\Z)[a-z0-9-]{1,31}")

# Mirrors paths::MAX_FILE_BYTES: the importer refuses a larger file at boot.
MAX_FILE_BYTES = 512 * 1024


def seed_dir(name: str) -> pathlib.Path:
    """`seeds/<name>` for a valid seed name; a path or a bad name is refused."""
    if not SEED_NAME.fullmatch(name):
        raise SeedError(
            f"{name!r} is not a seed name: lowercase letters, digits and hyphens, "
            f"starting with a letter, 2 to 32 characters, no doubled or trailing hyphen"
        )
    return SEEDS_DIR / name


def seed_dirs() -> list:
    """Every directory under seeds/ whose name is a seed name, in name order —
    with or without a manifest, so a seed that has none is reported, not skipped."""
    return sorted(p for p in SEEDS_DIR.iterdir() if p.is_dir() and SEED_NAME.fullmatch(p.name))

SCHEMA_VERSION = 1


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
        if file.is_symlink():
            raise SeedError(f"{file}: is a symlink; a seed carries real files")
        data = file.read_bytes()
        if len(data) > MAX_FILE_BYTES:
            raise SeedError(
                f"{file}: {len(data)} bytes is over the {MAX_FILE_BYTES}-byte limit "
                f"the runtime enforces (paths::MAX_FILE_BYTES)"
            )
        entries.append(
            {
                "path": rel,
                "sha256": sha256_hex(data),
                "size": len(data),
            }
        )
    return entries


def build_manifest(seed_dir: pathlib.Path) -> dict:
    """The manifest `seed_dir` should carry, in the field order
    `seed::SeedManifest` serializes."""
    site_dir = seed_dir / "site"
    if not site_dir.is_dir():
        raise SeedError(f"{seed_dir}: has no site/ directory")
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "source_generation": None,
        "site": site_entries(site_dir),
        "blocks": [],
        "data": None,
    }
    # Last, and only when the seed carries one: `seed::SeedManifest` declares
    # it last, and an absent block is how a bundle says it has none.
    sandbox = sandbox_block(seed_dir)
    if sandbox is not None:
        manifest["sandbox"] = sandbox
    return manifest


def render(manifest: dict) -> str:
    return json.dumps(manifest, indent=2) + "\n"


# Mirror seed::GUIDE_PATH, seed::MAX_GUIDE_BYTES and
# seed::MAX_PROMPT_BYTES: the importer refuses a sandbox block outside them.
GUIDE_PATH = "guide.md"
MAX_GUIDE_BYTES = 256 * 1024
MAX_PROMPT_BYTES = 4 * 1024

# The keys of sandbox.json, all required.
SANDBOX_KEYS = {"template", "title", "suggested_prompt"}
# A title is one line of a boot page's <title> and heading.
MAX_TITLE_BYTES = 120


# Mirror seed::LLMS_PATH and seed::MAX_LLMS_BYTES.
LLMS_PATH = "llms.txt"
MAX_LLMS_BYTES = 512 * 1024

# What every sandbox's llms.txt opens with, whichever seed it was built from.
LLMS_PREAMBLE = SEEDS_DIR / "llms-preamble.md"
# The one hole in it: the seed's template name.
LLMS_TEMPLATE_HOLE = "{template}"


def llms_text(template: str, guide: str, guide_path: pathlib.Path) -> str:
    """The seed's llms.txt: the shared preamble, naming the template, then the
    seed's guide verbatim. The guide is the single source of the building
    instructions — the same text `dev_read_reference` serves — so a reader
    that never gets as far as that tool is told exactly what one that does
    is. `guide_path` is where `guide` was read from, for the refusals.

    Plain ASCII, both halves (`require_ascii` says why)."""
    try:
        preamble = LLMS_PREAMBLE.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        raise SeedError(f"{LLMS_PREAMBLE}: cannot be read ({e})")
    if preamble.count(LLMS_TEMPLATE_HOLE) != 1:
        raise SeedError(
            f"{LLMS_PREAMBLE}: must name the template exactly once, as {LLMS_TEMPLATE_HOLE}"
        )
    if not preamble.endswith("\n") or not guide.endswith("\n"):
        raise SeedError(f"{LLMS_PREAMBLE} and the seed's {GUIDE_PATH} must each end with a newline")
    require_ascii(LLMS_PREAMBLE, preamble)
    require_ascii(guide_path, guide)
    return preamble.replace(LLMS_TEMPLATE_HOLE, template) + "\n" + guide


def require_ascii(path, text: str) -> None:
    """Refuse a source of llms.txt that is not plain ASCII, naming the line.

    A static host serves `/llms.txt` as `text/plain` with no charset
    (Cloudflare's asset server and `python3 -m http.server` both do), and a
    browser that NAVIGATES there decodes such a document in its locale's
    legacy encoding rather than UTF-8: a U+2014 dash reaches the reader as
    three characters of noise. The runtime's own answer says
    `charset=utf-8`, but a reader with no service worker yet gets the host's.
    ASCII reads the same under every decoding, on every host, so it is the
    one encoding that needs no host to be configured."""
    for number, line in enumerate(text.splitlines(), start=1):
        for char in line:
            if not char.isascii():
                raise SeedError(
                    f"{path}:{number}: U+{ord(char):04X} ({char}) — llms.txt is generated "
                    "from this file and must be plain ASCII: a static host serves it with no "
                    "charset, and a browser opening it decodes it as its locale's legacy "
                    "encoding rather than UTF-8"
                )


def load_sandbox(seed_dir: pathlib.Path):
    """The seed's sandbox.json, checked key by key, or None when it has none."""
    sandbox_path = seed_dir / "sandbox.json"
    if not sandbox_path.is_file():
        return None
    try:
        sandbox = json.loads(sandbox_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        raise SeedError(f"{sandbox_path}: not valid JSON ({e})")
    if not isinstance(sandbox, dict) or set(sandbox) != SANDBOX_KEYS:
        raise SeedError(
            f"{sandbox_path}: needs a JSON object with exactly the keys "
            f"{', '.join(sorted(SANDBOX_KEYS))}"
        )
    title = sandbox["title"]
    if not isinstance(title, str) or not title or title != title.strip():
        raise SeedError(
            f"{sandbox_path}: title must be a non-empty string with no space around it — "
            f"it heads the boot page of a sandbox built from this seed"
        )
    if not title.isprintable():
        raise SeedError(f"{sandbox_path}: title must be one line of printable text")
    if len(title.encode("utf-8")) > MAX_TITLE_BYTES:
        raise SeedError(f"{sandbox_path}: title is over {MAX_TITLE_BYTES} bytes")
    return sandbox


def boot_title(seed_dir: pathlib.Path):
    """The title of the boot page of a sandbox built from this seed, or None
    for a seed with no sandbox.json. `build.sh` stages it for the bundler; it
    is written nowhere else."""
    sandbox = load_sandbox(seed_dir)
    return None if sandbox is None else sandbox["title"]


def shared_titles(seeds: list) -> list:
    """One line per boot title that more than one of `seeds` carries. Two
    sandboxes with one title cannot be told apart from their boot pages, which
    are all a reader has until the runtime is installed. A seed whose
    sandbox.json cannot be read is skipped: it is reported on its own."""
    by_title = {}
    for seed in seeds:
        try:
            title = boot_title(seed)
        except SeedError:
            continue
        if title is not None:
            by_title.setdefault(title, []).append(seed.name)
    return [
        f"{', '.join(names)}: share the title {title!r} — each seed's sandbox.json names its own"
        for title, names in sorted(by_title.items())
        if len(names) > 1
    ]


def sandbox_block(seed_dir: pathlib.Path):
    """The `sandbox` block of a seed that carries sandbox.json and guide.md,
    or None when it carries neither. Checked here to the runtime's own limits
    (seed::fetch_sandbox), so a seed the importer would refuse at boot is
    refused at generation time instead."""
    sandbox_path = seed_dir / "sandbox.json"
    guide_path = seed_dir / GUIDE_PATH
    sandbox = load_sandbox(seed_dir)
    if sandbox is None:
        if guide_path.exists():
            raise SeedError(f"{guide_path}: present without sandbox.json, so nothing would serve it")
        return None
    template = sandbox["template"]
    prompt = sandbox["suggested_prompt"]
    if not isinstance(template, str) or not SEED_NAME.fullmatch(template):
        raise SeedError(
            f"{sandbox_path}: template {template!r} is not a valid name — the runtime's "
            f"block-name rule (paths::block_name_is_valid)"
        )
    if not isinstance(prompt, str):
        raise SeedError(f"{sandbox_path}: suggested_prompt must be a string")
    if len(prompt.encode("utf-8")) > MAX_PROMPT_BYTES:
        raise SeedError(f"{sandbox_path}: suggested_prompt is over {MAX_PROMPT_BYTES} bytes")
    if not guide_path.is_file() or guide_path.is_symlink():
        raise SeedError(f"{seed_dir}: sandbox.json is present but {GUIDE_PATH} is missing")
    data = guide_path.read_bytes()
    if len(data) > MAX_GUIDE_BYTES:
        raise SeedError(f"{guide_path}: {len(data)} bytes is over the {MAX_GUIDE_BYTES}-byte limit")
    try:
        guide = data.decode("utf-8")
    except UnicodeDecodeError as e:
        raise SeedError(f"{guide_path}: not valid UTF-8 ({e}); the importer refuses it at boot")
    llms = llms_text(template, guide, guide_path).encode("utf-8")
    if len(llms) > MAX_LLMS_BYTES:
        raise SeedError(
            f"{seed_dir}: the generated {LLMS_PATH} is {len(llms)} bytes, over the "
            f"{MAX_LLMS_BYTES}-byte limit"
        )
    return {
        "template": template,
        "suggested_prompt": prompt,
        "guide": {
            "path": GUIDE_PATH,
            "sha256": sha256_hex(data),
            "size": len(data),
        },
        "llms": {
            "path": LLMS_PATH,
            "sha256": sha256_hex(llms),
            "size": len(llms),
        },
    }


def staged_llms(seed_dir: pathlib.Path):
    """The bytes of the llms.txt `build.sh` stages beside the manifest, or
    None for a seed with no sandbox block. Generated the way `sandbox_block`
    hashed it, so the staged file is the one the manifest declares."""
    if sandbox_block(seed_dir) is None:
        return None
    sandbox = json.loads((seed_dir / "sandbox.json").read_text(encoding="utf-8"))
    guide_path = seed_dir / GUIDE_PATH
    guide = guide_path.read_text(encoding="utf-8")
    return llms_text(sandbox["template"], guide, guide_path).encode("utf-8")


def load_pin(seed_dir: pathlib.Path):
    """The seed's vendor.json — `{name, version, license, files: [{path, url,
    sha256}]}`, the third-party files `vendor.py` downloads into site/ — or
    None when it carries none. Every `path` must stay inside site/."""
    pin_path = seed_dir / "vendor.json"
    if not pin_path.is_file():
        return None
    try:
        pin = json.loads(pin_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        raise SeedError(f"{pin_path}: not valid JSON ({e}) — fix the pin")
    shape = "a JSON object with name, version, license and files: [{path, url, sha256}]"
    if not isinstance(pin, dict) or not {"name", "version", "license", "files"} <= set(pin):
        raise SeedError(f"{pin_path}: needs {shape}")
    if not isinstance(pin["files"], list):
        raise SeedError(f"{pin_path}: files must be a list — needs {shape}")
    site = (seed_dir / "site").resolve()
    for entry in pin["files"]:
        if not isinstance(entry, dict) or set(entry) != {"path", "url", "sha256"} or not all(
            isinstance(v, str) for v in entry.values()
        ):
            raise SeedError(f"{pin_path}: entry {entry!r} needs exactly the string keys path, url and sha256")
        if not (site / entry["path"]).resolve().is_relative_to(site):
            raise SeedError(f"{pin_path}: path {entry['path']!r} is outside site/ — fix the pin")
    return pin
