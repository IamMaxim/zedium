#!/usr/bin/env bash
# Re-export zed/'s applied-branch commits to patches/.
# Prefer `just export` from the parent.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

APPLIED_BRANCH="${ZEDIUM_APPLIED_BRANCH:-zedium-applied}"
PATCHES_DIR="${ZEDIUM_PATCHES_DIR:-patches}"

if ! git -C zed rev-parse --verify "$APPLIED_BRANCH" >/dev/null 2>&1; then
    echo "error: zed has no '$APPLIED_BRANCH' branch (run 'just apply' first)" >&2
    exit 1
fi

baseline=$(git -C zed describe --tags --abbrev=0 HEAD 2>/dev/null)

mkdir -p "$PATCHES_DIR"
rm -f -- "$PATCHES_DIR"/*.patch

git -C zed format-patch \
    --no-signature \
    --zero-commit \
    --no-numbered \
    --no-cover-letter \
    --binary \
    -o "../$PATCHES_DIR" \
    "$baseline..$APPLIED_BRANCH"

count=$(ls -1 "$PATCHES_DIR"/*.patch 2>/dev/null | wc -l)
echo "exported $count patch(es) from $baseline..$APPLIED_BRANCH"
