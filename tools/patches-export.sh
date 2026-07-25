#!/usr/bin/env bash
# Re-export zed/'s applied-branch commits into the patches/ category tree.
# Prefer `just export` from the parent.
#
# Routing key = patch slug (filename minus the NNNN- prefix and .patch suffix),
# looked up in patches/manifest (TSV: slug<TAB>folder). Unknown slugs land in
# patches/_unsorted/ (a signal to add a manifest entry and re-run). Folders are
# cosmetic; global apply order is the 4-digit basename prefix (see patches-apply.sh).
#
# Honors ZEDIUM_APPLIED_BRANCH / ZEDIUM_PATCHES_DIR (defaults: zedium-applied / patches).

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

APPLIED_BRANCH="${ZEDIUM_APPLIED_BRANCH:-zedium-applied}"
PATCHES_DIR="${ZEDIUM_PATCHES_DIR:-patches}"
MANIFEST="$PATCHES_DIR/manifest"

if ! git -C zed rev-parse --verify "$APPLIED_BRANCH" >/dev/null 2>&1; then
    echo "error: zed has no '$APPLIED_BRANCH' branch (run 'just apply' first)" >&2
    exit 1
fi

baseline=$(git -C zed describe --tags --abbrev=0 HEAD 2>/dev/null)

mkdir -p "$PATCHES_DIR"
# Clear existing patch files (keep the manifest and folder structure).
find "$PATCHES_DIR" -type f -name '*.patch' -delete

# Export flat into a temp staging dir first, then route each into its folder.
stage="$PATCHES_DIR/.export-stage"
rm -rf "$stage"
mkdir -p "$stage"

git -C zed format-patch \
    --no-signature \
    --zero-commit \
    --no-numbered \
    --no-cover-letter \
    --binary \
    -o "$PWD/$stage" \
    "$baseline..$APPLIED_BRANCH"

route_folder() {  # slug -> folder (via manifest), empty if unknown
    [ -f "$MANIFEST" ] || return 0
    awk -F'\t' -v s="$1" '$1==s {print $2; exit}' "$MANIFEST"
}

count=0; unsorted=0
for f in "$stage"/*.patch; do
    [ -e "$f" ] || continue
    base=$(basename "$f")
    slug=$(printf '%s' "$base" | sed -E 's/^[0-9]+-//; s/\.patch$//')
    folder=$(route_folder "$slug")
    if [ -z "$folder" ]; then folder="_unsorted"; unsorted=$((unsorted+1)); fi
    mkdir -p "$PATCHES_DIR/$folder"
    mv "$f" "$PATCHES_DIR/$folder/$base"
    count=$((count+1))
done
rmdir "$stage" 2>/dev/null || rm -rf "$stage"

# Prune now-empty category folders so a re-route doesn't leave stale dirs.
find "$PATCHES_DIR" -mindepth 1 -type d -empty -delete 2>/dev/null || true

echo "exported $count patch(es) from $baseline..$APPLIED_BRANCH"
if [ "$unsorted" -gt 0 ]; then
    echo "  WARNING: $unsorted patch(es) had no manifest entry -> patches/_unsorted/ (add them to $MANIFEST and re-run)"
fi
