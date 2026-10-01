"""What a seed directory is, for the generator and the check.

A seed is `seeds/<name>/`: a `site/` tree that becomes generation 0 of a
fresh sandbox, and a `manifest.json` that lists every file of it with the
sha256, size and content type `impresspress-core::blocks::dev::seed` verifies
at boot. A seed may also carry `sandbox.json` (its template name and the
prompt `/b/dev` suggests) and `guide.md` (the site-authoring guide
`dev_read_reference` serves); the manifest's `sandbox` block is built from
the two. The manifest is generated from the tree (`write-manifest.py`) and
checked against it (`check-seeds.py`); it is never hand-edited.
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
            raise SeedError(
                f"{file}: no content type for extension {ext!r} — the runtime would serve it as "
                f"application/octet-stream. Rename the file, or extend CONTENT_TYPES in step with "
                f"paths::content_type_for."
            )
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
                "content_type": CONTENT_TYPES[ext],
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


# Mirror seed::GUIDE_PATH, seed::GUIDE_CONTENT_TYPE, seed::MAX_GUIDE_BYTES and
# seed::MAX_PROMPT_BYTES: the importer refuses a sandbox block outside them.
GUIDE_PATH = "guide.md"
GUIDE_CONTENT_TYPE = "text/markdown; charset=utf-8"
MAX_GUIDE_BYTES = 256 * 1024
MAX_PROMPT_BYTES = 4 * 1024


def sandbox_block(seed_dir: pathlib.Path):
    """The `sandbox` block of a seed that carries sandbox.json and guide.md,
    or None when it carries neither. Checked here to the runtime's own limits
    (seed::fetch_sandbox), so a seed the importer would refuse at boot is
    refused at generation time instead."""
    sandbox_path = seed_dir / "sandbox.json"
    guide_path = seed_dir / GUIDE_PATH
    if not sandbox_path.is_file():
        if guide_path.exists():
            raise SeedError(f"{guide_path}: present without sandbox.json, so nothing would serve it")
        return None
    try:
        sandbox = json.loads(sandbox_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        raise SeedError(f"{sandbox_path}: not valid JSON ({e})")
    if not isinstance(sandbox, dict) or set(sandbox) != {"template", "suggested_prompt"}:
        raise SeedError(
            f"{sandbox_path}: needs a JSON object with exactly the keys template and suggested_prompt"
        )
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
        data.decode("utf-8")
    except UnicodeDecodeError as e:
        raise SeedError(f"{guide_path}: not valid UTF-8 ({e}); the importer refuses it at boot")
    return {
        "template": template,
        "suggested_prompt": prompt,
        "guide": {
            "path": GUIDE_PATH,
            "sha256": sha256_hex(data),
            "size": len(data),
            "content_type": GUIDE_CONTENT_TYPE,
        },
    }
