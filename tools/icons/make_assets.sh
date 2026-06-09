#!/usr/bin/env bash
# Generate the full Zedium icon asset set from the per-channel SVG masters.
# DEST is the crates/zed/resources directory to populate.
set -euo pipefail
SRC="${1:?usage: make_assets.sh <src-svg-dir> <dest-resources-dir>}"
DEST="${2:?usage: make_assets.sh <src-svg-dir> <dest-resources-dir>}"
mkdir -p "$DEST/windows" "$SRC/tmp"

for ch in stable dev nightly preview; do
  case "$ch" in
    stable)  sfx="" ;;
    *)       sfx="-$ch" ;;
  esac
  svg="$SRC/icon_${ch}.svg"
  rsvg-convert -w 512  -h 512  "$svg" -o "$DEST/app-icon${sfx}.png"
  rsvg-convert -w 1024 -h 1024 "$svg" -o "$DEST/app-icon${sfx}@2x.png"
  # Windows multi-resolution .ico
  magick "$DEST/app-icon${sfx}@2x.png" -define icon:auto-resize=256,128,64,48,32,16 "$DEST/windows/app-icon${sfx}.ico"
done

# macOS Document.icns from the stable master (packed from PNGs).
for s in 16 32 64 128 256 512 1024; do
  rsvg-convert -w $s -h $s "$SRC/icon_stable.svg" -o "$SRC/tmp/i$s.png"
done
python3 "$SRC/icns_pack.py" "$DEST/Document.icns" \
  icp4:"$SRC/tmp/i16.png" icp5:"$SRC/tmp/i32.png" icp6:"$SRC/tmp/i64.png" \
  ic07:"$SRC/tmp/i128.png" ic08:"$SRC/tmp/i256.png" ic09:"$SRC/tmp/i512.png" ic10:"$SRC/tmp/i1024.png"
echo "--- generated ---"
ls -la "$DEST"/app-icon*.png "$DEST"/Document.icns "$DEST"/windows/app-icon*.ico
