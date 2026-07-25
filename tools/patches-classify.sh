#!/usr/bin/env bash
# One-time helper: emit an initial patches/manifest (slug<TAB>folder) by slug
# heuristics. Review the output by hand before trusting it; folders are cosmetic
# and never affect apply order, so a misroute is a readability nit, not a bug.
#
# Writes to patches/manifest.new; review, then `mv patches/manifest.new patches/manifest`.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

PATCHES_DIR="${ZEDIUM_PATCHES_DIR:-patches}"

classify() {  # slug -> folder
    case "$1" in
        strip-*|*clear-telemetry*|*remove-telemetry*)                 echo "01-strip" ;;
        brand-*|metadata-*|*rebrand*|*brand-*)                        echo "02-brand" ;;
        remote-*|bundle-*|remote_connection-*|*remote_server*)        echo "03-remote" ;;
        acp_thread-*|agent_servers-*|*subagent*|docs-*ACP*|docs-*acp*) echo "05-agent-acp" ;;
        feat-db-*|fix-db-*|database_client-*|style-db-*|refactor-db-*|docs-db-*) echo "06-database" ;;
        agent-make-LLM*|agent_settings-*|*mascot*|*animation*|markdown-*|*choreograph*|ui-add-MascotPlayer*) echo "07-agent-ux" ;;
        *)                                                            echo "04-features" ;;
    esac
}

: > "$PATCHES_DIR/manifest.new"
n=0
while IFS= read -r f; do
    base=$(basename "$f")
    slug=$(printf '%s' "$base" | sed -E 's/^[0-9]+-//; s/\.patch$//')
    printf '%s\t%s\n' "$slug" "$(classify "$slug")" >> "$PATCHES_DIR/manifest.new"
    n=$((n+1))
done < <(find "$PATCHES_DIR" -type f -name '*.patch' | awk -F/ '{print $NF"\t"$0}' | sort | cut -f2-)

echo "wrote $PATCHES_DIR/manifest.new ($n entries) — review, then mv to manifest"
