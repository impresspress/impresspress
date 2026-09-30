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
