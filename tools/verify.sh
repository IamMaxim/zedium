#!/usr/bin/env bash
# Zedium verifier. Static checks on the parent repo + submodule working tree.
#
# Checks:
#   1. Forbidden strings — regex patterns from tools/forbidden-strings.txt
#      must not appear in any scanned file outside the allowlist.
#   2. Banned crates — directory names listed in tools/banned-crates.txt
#      must not exist under zed/crates/.
#
# Scan scope:
#   - All tracked files in the parent repo (excluding zed/ submodule gitlink).
#   - All files tracked by zed/'s git index — after `just apply` runs `git am`,
#     the index reflects the post-strip state (deleted files no longer listed).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Strip leading/trailing whitespace WITHOUT mangling the contents. The previous
# `echo "$x" | xargs` idiom interpreted backslashes and quotes, silently turning
# an escaped regex (e.g. a cloud-URL-builder tripwire) into an unbalanced,
# uncompilable one; rg then refused it, the tripwire matched nothing, and verify
# falsely passed. Parameter-expansion trim preserves the bytes literally.
trim() {
    local s="$1"
    s="${s#"${s%%[![:space:]]*}"}"
    s="${s%"${s##*[![:space:]]}"}"
    printf '%s' "$s"
}

PATTERNS_FILE="tools/forbidden-strings.txt"
ALLOWLIST_FILE="tools/forbidden-strings.allowlist"
BANNED_CRATES_FILE="tools/banned-crates.txt"

# --- Load allowlist (path prefixes; supports exact files and directory prefixes).
allowed=()
if [[ -f "$ALLOWLIST_FILE" ]]; then
    while IFS= read -r line; do
        line="${line%%#*}"
        line="$(trim "$line")"
        [[ -z "$line" ]] && continue
        allowed+=("$line")
    done < "$ALLOWLIST_FILE"
fi

is_allowed() {
    local file="$1"
    local prefix
    for prefix in "${allowed[@]:-}"; do
        [[ -z "$prefix" ]] && continue
        case "$file" in
            "$prefix"|"$prefix"/*) return 0 ;;
        esac
    done
    return 1
}

# --- Collect files: parent (minus submodule gitlink) + zed/ index.
parent_files=()
while IFS= read -r f; do
    [[ "$f" == "zed" ]] && continue
    parent_files+=("$f")
done < <(git ls-files)

zed_files=()
if [[ -d zed/.git || -f zed/.git ]]; then
    while IFS= read -r f; do
        zed_files+=("zed/$f")
    done < <(git -C zed ls-files)
fi

scan_files=()
for f in "${parent_files[@]}" "${zed_files[@]:-}"; do
    [[ -z "$f" ]] && continue
    is_allowed "$f" || scan_files+=("$f")
done

fail=0

if [[ -f "$PATTERNS_FILE" && ${#scan_files[@]} -gt 0 ]]; then
    while IFS= read -r raw; do
        raw="${raw%%#*}"
        pattern="$(trim "$raw")"
        [[ -z "$pattern" ]] && continue
        # A pattern that does not compile is a gate defect, not an absence of
        # matches: fail loudly instead of silently passing. Validate against
        # empty stdin (piped, so rg reads stdin rather than falling back to the
        # cwd) — rg exits >=2 on a malformed regex; exit 1 ("no match") is fine.
        if ! printf '' | rg --color never -e "$pattern" >/dev/null 2>&1; then
            rc=$?
            if [[ $rc -ge 2 ]]; then
                echo "::error::Invalid forbidden-string regex (does not compile): $pattern" >&2
                fail=1
                continue
            fi
        fi
        matches="$(
            printf '%s\0' "${scan_files[@]}" \
                | xargs -0 rg --no-heading --no-messages --color never -n -e "$pattern" 2>/dev/null \
                | head -50 || true
        )"
        if [[ -n "$matches" ]]; then
            echo "::error::Forbidden string matched: $pattern" >&2
            echo "$matches" >&2
            echo "---" >&2
            fail=1
        fi
    done < "$PATTERNS_FILE"
fi

if [[ -f "$BANNED_CRATES_FILE" ]]; then
    while IFS= read -r raw; do
        raw="${raw%%#*}"
        crate="$(trim "$raw")"
        [[ -z "$crate" ]] && continue
        if [[ -d "zed/crates/$crate" ]]; then
            echo "::error::Banned crate directory present: zed/crates/$crate" >&2
            fail=1
        fi
    done < "$BANNED_CRATES_FILE"
fi

count_active() {
    awk '!/^[[:space:]]*(#|$)/' "$1" 2>/dev/null | wc -l
}

if [[ $fail -eq 0 ]]; then
    printf 'verify: all checks passed (%d files scanned, %d patterns, %d banned crates)\n' \
        "${#scan_files[@]}" \
        "$(count_active "$PATTERNS_FILE")" \
        "$(count_active "$BANNED_CRATES_FILE")"
fi
exit $fail
