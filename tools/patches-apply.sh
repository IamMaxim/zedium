#!/usr/bin/env bash
# Apply patches/**/*.patch onto a fresh applied-branch off the baseline tag.
# Prefer `just apply` from the parent.
#
# Patches live in cosmetic category subfolders (01-strip, 02-brand, ...); the
# apply order is the GLOBAL 4-digit basename prefix (NNNN-), independent of which
# folder a patch sits in. Folder routing is therefore purely cosmetic: moving a
# patch between folders never changes apply order.
#
# Honors ZEDIUM_APPLIED_BRANCH / ZEDIUM_PATCHES_DIR (defaults: zedium-applied / patches).

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

APPLIED_BRANCH="${ZEDIUM_APPLIED_BRANCH:-zedium-applied}"
PATCHES_DIR="${ZEDIUM_PATCHES_DIR:-patches}"

baseline=$(git -C zed describe --tags --abbrev=0)

git -C zed checkout -q "$baseline"
git -C zed branch -D "$APPLIED_BRANCH" 2>/dev/null || true
git -C zed checkout -q -B "$APPLIED_BRANCH" "$baseline"

# Discover recursively; order by basename (the NNNN- prefix), folder-independent.
# awk prepends the basename as a sort key; sort; then strip the key back off.
patches=()
while IFS= read -r p; do
    patches+=("$PWD/$p")
done < <(find "$PATCHES_DIR" -type f -name '*.patch' \
            | awk -F/ '{print $NF"\t"$0}' | sort | cut -f2-)

if [ "${#patches[@]}" -eq 0 ]; then
    echo "no patches to apply (baseline $baseline)"
    exit 0
fi

git -C zed am --keep-cr "${patches[@]}"
echo "applied ${#patches[@]} patch(es) onto $baseline"
