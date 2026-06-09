# Zedium icon sources

The Zedium app icon is a periodic-table "element tile" — a bold `Zd` symbol with
atomic number 26 (Z = the 26th letter) on a teal/navy squircle, deliberately
distinct from Zed's lightning-bolt mark. Four channel variants differ by accent
(stable = teal, dev = amber, nightly = violet, preview = green).

- `template.svg` — the parametrised master (color tokens substituted per channel).
- `icon_{stable,dev,nightly,preview}.svg` — the rendered-per-channel masters.
- `make_assets.sh <src-dir> <dest-resources-dir>` — renders every shipped asset
  (PNG 512 + @2x, Windows multi-res `.ico`, macOS `Document.icns`) into a
  `crates/zed/resources` directory. Requires `rsvg-convert`, ImageMagick, `python3`.
- `icns_pack.py` — packs PNGs into a valid `.icns` container (no `iconutil` needed).

## Regenerate the in-tree icons

    just apply
    bash tools/icons/make_assets.sh tools/icons zed/crates/zed/resources

The generated binaries are carried in `patches/0032-*.patch` (a `--binary`
format-patch), so a normal `just apply` already restores them; only re-run the
generator if you edit the SVGs.
