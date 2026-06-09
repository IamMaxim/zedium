#!/usr/bin/env bash
# Apply patches/*.patch onto a fresh applied-branch off the baseline tag.
# Prefer `just apply` from the parent.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

APPLIED_BRANCH="${ZEDIUM_APPLIED_BRANCH:-zedium-applied}"
PATCHES_DIR="${ZEDIUM_PATCHES_DIR:-patches}"

baseline=$(git -C zed describe --tags --abbrev=0)

git -C zed checkout -q "$baseline"
git -C zed branch -D "$APPLIED_BRANCH" 2>/dev/null || true
git -C zed checkout -q -B "$APPLIED_BRANCH" "$baseline"

if compgen -G "$PATCHES_DIR/*.patch" >/dev/null; then
    git -C zed am --keep-cr "$PWD/$PATCHES_DIR"/*.patch
    count=$(ls -1 "$PATCHES_DIR"/*.patch | wc -l)
    echo "applied $count patch(es) onto $baseline"
else
    echo "no patches to apply (baseline $baseline)"
fi
