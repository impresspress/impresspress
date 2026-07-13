#!/bin/bash
#
# sync-fonts.sh — pull bundled font binaries from impresspress/site-kit
#
# The Itim woff2 files served by the admin chrome (CSS at
# tokens.css → /b/static/itim-latin-{hash}.woff2) are bundled into the
# worker via include_bytes! at compile time. The canonical source of
# truth is impresspress/site-kit's `/fonts/` directory.
#
# Run this script when site-kit ships a font update (rare — Itim is a
# stable Google font), then commit the changed binaries.
#
# Usage:
#   ./scripts/sync-fonts.sh            # default: site-kit main branch
#   REF=v1.2.3 ./scripts/sync-fonts.sh # pin to a tag/branch/sha

set -euo pipefail

REF="${REF:-main}"
REPO="impresspress/site-kit"
DEST="crates/impresspress-core/src/ui/assets/fonts"
FONTS=(
    "itim-latin.woff2"
    "itim-latin-ext.woff2"
)

# Resolve relative paths from repo root regardless of where invoked.
cd "$(git rev-parse --show-toplevel)"

mkdir -p "$DEST"

for font in "${FONTS[@]}"; do
    url="https://raw.githubusercontent.com/$REPO/$REF/fonts/$font"
    echo "→ $url"
    if ! curl -sSfL --proto '=https' --tlsv1.2 -o "$DEST/$font.tmp" "$url"; then
        echo "  ✗ fetch failed; aborting." >&2
        rm -f "$DEST/$font.tmp"
        exit 1
    fi
    # Sanity-check: woff2 starts with the magic bytes "wOF2" (0x77 0x4F 0x46 0x32).
    magic=$(head -c 4 "$DEST/$font.tmp" | od -An -c | tr -d ' \n')
    if [ "$magic" != "wOF2" ]; then
        echo "  ✗ $font is not a woff2 (got magic: $magic); aborting." >&2
        rm -f "$DEST/$font.tmp"
        exit 1
    fi
    mv "$DEST/$font.tmp" "$DEST/$font"
    size=$(stat -c%s "$DEST/$font" 2>/dev/null || stat -f%z "$DEST/$font")
    echo "  ✓ $DEST/$font ($size bytes)"
done

echo
echo "Done. Review and commit:"
echo "  git diff --stat $DEST"
echo "  git add $DEST && git commit -m 'chore(fonts): sync Itim from $REPO@$REF'"
