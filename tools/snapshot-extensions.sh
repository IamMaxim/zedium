#!/usr/bin/env bash
# snapshot-extensions.sh — build a local, air-gap-installable bundle of Zed
# extensions by snapshotting the public Zed extension registry at BUILD time.
#
# This runs ONLY on a networked build/CI machine. It is never compiled into or
# invoked by the shipped Zedium binary — the binary stays air-gapped. The bundle
# it produces is attached to the GitHub release; air-gapped users carry it in and
# install extensions from it locally (see docs/EXTENSIONS_OFFLINE.md).
#
# What it does:
#   1. Enumerate the registry (GET /extensions?max_schema_version=N).
#   2. Select extensions (curated allowlist by default, or --full = all).
#   3. Keep only versions compatible with the shipped channel's wasm_api_version
#      ceiling (stable => 0.7.0). A 0.8.0-compiled wasm would fail to load.
#   4. Download each verbatim upstream archive.tar.gz (prebuilt wasm + assets).
#   5. Emit index.json (registry-shaped, for the in-app local marketplace) +
#      MANIFEST.txt (id/version/wasm_api/sha256) and wrap it all in one asset.
#
# Usage:
#   tools/snapshot-extensions.sh [options]
#     --full                 snapshot ALL compatible extensions (ignore allowlist)
#     --allowlist FILE       allowlist path (default tools/extensions-allowlist.txt)
#     --out DIR              output dir (default ./dist-extensions)
#     --api URL              registry base (default https://api.zed.dev)
#     --max-wasm-api X.Y.Z   wasm api ceiling (default 0.7.0 = stable channel)
#     --max-schema-version N manifest schema ceiling (default 1)
#     --zed-version VER      label for the asset name (default: read RELEASE or "dev")
#     --channel NAME         label for the asset name (default stable)
#     --limit N              cap number of extensions (0 = no cap; default 0)
#
# Requires: bash, curl, python3, tar. (No jq dependency.)
set -euo pipefail

API="https://api.zed.dev"
OUT="dist-extensions"
ALLOWLIST="tools/extensions-allowlist.txt"
MAX_WASM_API="0.7.0"
MAX_SCHEMA="1"
CHANNEL="stable"
ZED_VERSION=""
FULL=0
LIMIT=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --full) FULL=1; shift ;;
    --allowlist) ALLOWLIST="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --api) API="$2"; shift 2 ;;
    --max-wasm-api) MAX_WASM_API="$2"; shift 2 ;;
    --max-schema-version) MAX_SCHEMA="$2"; shift 2 ;;
    --zed-version) ZED_VERSION="$2"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --limit) LIMIT="$2"; shift 2 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Derive a version label for the asset name if not given.
if [[ -z "$ZED_VERSION" ]]; then
  if [[ -f zed/crates/zed/RELEASE ]]; then ZED_VERSION="$(tr -d '[:space:]' < zed/crates/zed/RELEASE)"; fi
  if [[ -z "$ZED_VERSION" ]] && command -v git >/dev/null && [[ -d zed/.git || -f zed/.git ]]; then
    ZED_VERSION="$(git -C zed describe --tags 2>/dev/null | sed 's/-.*//' || true)"
  fi
  [[ -z "$ZED_VERSION" ]] && ZED_VERSION="dev"
fi

BUNDLE_NAME="zedium-extensions-${CHANNEL}-${ZED_VERSION}"
WORK="$OUT/$BUNDLE_NAME"
echo ">> snapshot target: $WORK"
echo ">> registry: $API   ceiling: wasm_api<=$MAX_WASM_API schema<=$MAX_SCHEMA   mode: $([[ $FULL == 1 ]] && echo full || echo "allowlist($ALLOWLIST)")"

rm -rf "$WORK"
mkdir -p "$WORK/extensions"

# --- 1. enumerate -----------------------------------------------------------
echo ">> enumerating registry..."
curl -fsSL --retry 3 --max-time 120 "$API/extensions?max_schema_version=${MAX_SCHEMA}" -o "$OUT/registry.json"

# --- 2+3. select + compat-filter (python: no jq dep) ------------------------
# Emits: $OUT/selected.tsv (id<TAB>version<TAB>wasm_api) and $WORK/index.json.
ALLOWLIST_ARG=""
[[ $FULL == 0 ]] && ALLOWLIST_ARG="$ALLOWLIST"
python3 - "$OUT/registry.json" "$WORK/index.json" "$OUT/selected.tsv" "$MAX_WASM_API" "$MAX_SCHEMA" "$LIMIT" "$ALLOWLIST_ARG" <<'PY'
import json, sys, os
reg_path, index_path, sel_path, max_wasm, max_schema, limit, allowlist = sys.argv[1:8]
max_schema = int(max_schema); limit = int(limit)

def ver(s):
    if not s: return None
    try: return tuple(int(x) for x in str(s).split('.'))
    except ValueError: return None

ceil = ver(max_wasm)
data = json.load(open(reg_path)).get('data', [])
by_id = {}
for e in data:
    by_id.setdefault(e['id'], []).append(e)  # registry returns one (latest) per id, but be safe

wanted = None
if allowlist:
    wanted = []
    for line in open(allowlist):
        line = line.split('#', 1)[0].strip()
        if line: wanted.append(line)

selected, skipped, missing = [], [], []
ids = wanted if wanted is not None else sorted(by_id)
for xid in ids:
    recs = by_id.get(xid)
    if not recs:
        missing.append(xid); continue
    # pick the highest version whose wasm_api <= ceiling and schema <= max_schema
    cand = []
    for e in recs:
        sv = e.get('schema_version')
        if sv is not None and sv > max_schema: continue
        wa = e.get('wasm_api_version')
        if wa is not None and ver(wa) is not None and ceil is not None and ver(wa) > ceil:
            continue
        cand.append(e)
    if not cand:
        skipped.append(xid); continue
    cand.sort(key=lambda e: ver(e.get('version')) or (), reverse=True)
    selected.append(cand[0])

if limit and len(selected) > limit:
    selected = selected[:limit]

with open(index_path, 'w') as f:
    json.dump({'data': selected}, f, indent=2)
with open(sel_path, 'w') as f:
    for e in selected:
        f.write(f"{e['id']}\t{e['version']}\t{e.get('wasm_api_version') or ''}\n")

print(f"   selected={len(selected)} skipped_incompatible={len(skipped)} missing_from_registry={len(missing)}")
if skipped: print("   SKIPPED (no <= %s build): %s" % (max_wasm, ', '.join(sorted(skipped))))
if missing: print("   MISSING (not in registry):  %s" % ', '.join(sorted(missing)))
PY

COUNT=$(wc -l < "$OUT/selected.tsv" | tr -d ' ')
echo ">> downloading $COUNT extension archives..."

# --- 4. download + validate + checksum --------------------------------------
: > "$WORK/MANIFEST.txt"
echo "# id	version	wasm_api_version	sha256" >> "$WORK/MANIFEST.txt"
fail=0
while IFS=$'\t' read -r id version wasm_api; do
  [[ -z "$id" ]] && continue
  dest="$WORK/extensions/$id/$version"
  mkdir -p "$dest"
  url="$API/extensions/$id/$version/download"
  if ! curl -fsSL --retry 3 --max-time 180 "$url" -o "$dest/archive.tar.gz"; then
    echo "   !! download failed: $id $version" >&2; fail=$((fail+1)); continue
  fi
  # validate: real gzip tar containing extension.toml (or legacy extension.json).
  # List once into a var and match with pure bash — piping `tar` into `grep -q`
  # under `set -o pipefail` is a trap: grep exits on first match and SIGPIPEs
  # tar (exit 141), which pipefail then reports as a (spurious) failure.
  if ! listing=$(tar tzf "$dest/archive.tar.gz" 2>/dev/null); then
    echo "   !! not a valid gzip tar: $id $version" >&2; fail=$((fail+1)); continue
  fi
  case "$listing" in
    *extension.toml*|*extension.json*) : ;;
    *) echo "   !! archive missing extension manifest: $id $version" >&2; fail=$((fail+1)); continue ;;
  esac
  sha=$( (sha256sum "$dest/archive.tar.gz" 2>/dev/null || shasum -a 256 "$dest/archive.tar.gz") | awk '{print $1}')
  printf '%s\t%s\t%s\t%s\n' "$id" "$version" "${wasm_api:-}" "$sha" >> "$WORK/MANIFEST.txt"
done < "$OUT/selected.tsv"

if [[ $fail -gt 0 ]]; then
  echo ">> WARNING: $fail extension(s) failed to download/validate (see above). Bundle still produced with the rest." >&2
fi

# provenance
cat > "$WORK/README.txt" <<EOF
Zedium offline extension bundle
  channel:        $CHANNEL
  zed version:    $ZED_VERSION
  wasm_api ceil:  <= $MAX_WASM_API   schema ceil: <= $MAX_SCHEMA
  source:         $API
  extensions:     $(($(wc -l < "$WORK/MANIFEST.txt")-1))
Install: see docs/EXTENSIONS_OFFLINE.md. index.json is consumed by Zedium's
local extension marketplace; extensions/<id>/<version>/archive.tar.gz are the
verbatim upstream prebuilt archives.
EOF

# --- 5. wrap into one asset -------------------------------------------------
ASSET="$OUT/${BUNDLE_NAME}.tar.gz"
tar czf "$ASSET" -C "$OUT" "$BUNDLE_NAME"
SIZE=$(du -h "$ASSET" | awk '{print $1}')
echo ">> wrote $ASSET ($SIZE)"
( sha256sum "$ASSET" 2>/dev/null || shasum -a 256 "$ASSET" ) | tee "$ASSET.sha256"
echo ">> done."
