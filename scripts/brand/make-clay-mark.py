#!/usr/bin/env python3
"""Render Impresspress's built-in brand mark from the clay master.

Usage:
    python3 -I scripts/brand/make-clay-mark.py <logo-mark.png> <out-dir>

<logo-mark.png> is `brand/clay/logo-mark.png` in the impresspress/site repo
(the 1254x1254 RGBA master; the committed files came from site@0e333ff).
Requires Pillow (12.2 produced the committed bytes) and oxipng 10.2.1
(`cargo install oxipng --version 10.2.1 --locked`) on PATH, or named by the
OXIPNG environment variable.

Writes into <out-dir>, and each file is copied from there to:

    impresspress-logo.png     32x32  crates/impresspress-core/src/ui/assets/
    impresspress-logo-2x.png  64x64  crates/impresspress-core/src/ui/assets/
                                     packages/impresspress-js/static/logo.png
    favicon.ico               16/32/48, one PNG frame each
                                     crates/impresspress-core/src/ui/assets/
                                     packages/impresspress-js/static/
    icon-192.png, icon-512.png       crates/impresspress-web/js/ (the web app
                                     manifest's install icons)

Steps, in order:

1. Crop to the blob: the bounding box of pixels at alpha >= 64 (25%). The
   master carries faint generator specks out to its canvas edges; cropping
   to them would leave the mark small inside its square.
2. Centre the crop on a transparent square, with a 4% margin on each side.
3. One Lanczos downscale per size from that square, in premultiplied alpha
   (`RGBa`), so the RGB of transparent pixels never bleeds into the edge.
4. oxipng `-o max --strip all --alpha`: lossless.
5. Pack the optimised 16/32/48 PNGs into favicon.ico, each frame stored as
   the PNG itself.
"""

import os
import struct
import subprocess
import sys
from pathlib import Path

from PIL import Image

ALPHA_THRESHOLD = 64
MARGIN = 0.04

OUTPUTS = [
    ("impresspress-logo.png", 32),
    ("impresspress-logo-2x.png", 64),
    ("icon-192.png", 192),
    ("icon-512.png", 512),
]
ICO_SIZES = [16, 32, 48]


def square_master(path: Path) -> Image.Image:
    im = Image.open(path).convert("RGBA")
    mask = im.getchannel("A").point(lambda a: 255 if a >= ALPHA_THRESHOLD else 0)
    blob = im.crop(mask.getbbox())
    w, h = blob.size
    side = round(max(w, h) / (1 - 2 * MARGIN))
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.paste(blob, ((side - w) // 2, (side - h) // 2))
    return canvas


def render(master: Image.Image, size: int) -> Image.Image:
    return (
        master.convert("RGBa")
        .resize((size, size), Image.Resampling.LANCZOS, reducing_gap=None)
        .convert("RGBA")
    )


def pack_ico(frames: list[tuple[int, bytes]]) -> bytes:
    header = struct.pack("<HHH", 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    entries = b""
    for size, data in frames:
        entries += struct.pack("<BBBBHHII", size, size, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return header + entries + b"".join(data for _, data in frames)


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    master_path, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    master = square_master(master_path)

    pngs = [(out / name, size) for name, size in OUTPUTS]
    frames = [(out / f"favicon-{size}.png", size) for size in ICO_SIZES]
    for path, size in pngs + frames:
        render(master, size).save(path, optimize=True)

    oxipng = os.environ.get("OXIPNG", "oxipng")
    subprocess.run(
        [oxipng, "-o", "max", "--strip", "all", "--alpha", "-q"]
        + [str(p) for p, _ in pngs + frames],
        check=True,
    )

    (out / "favicon.ico").write_bytes(
        pack_ico([(size, path.read_bytes()) for path, size in frames])
    )
    for path, _ in frames:
        path.unlink()


if __name__ == "__main__":
    main()
