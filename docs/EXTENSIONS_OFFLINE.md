# Installing extensions in an air-gapped environment

Zedium ships with the Zed extension **registry disabled** — the editor never
contacts `api.zed.dev` (or any other host) to browse or download extensions.
That is by design: Zedium is meant to run fully offline. This document explains
how to get extensions, themes, language support, icon themes, etc. onto an
air-gapped machine.

There are two halves: a **bundle** produced on a networked machine, and a
**local install** performed inside Zedium on the air-gapped machine.

## 1. The offline extension bundle

Each Zedium release attaches an asset named:

```
zedium-extensions-stable-<zed-version>.tar.gz
```

It is a snapshot of the public Zed extension registry, taken at release time on
a networked CI machine. It contains, for each bundled extension, the **verbatim
upstream prebuilt archive** (`extensions/<id>/<version>/archive.tar.gz` — compiled
`extension.wasm`, grammars, themes, language config, etc.), plus an `index.json`
(the registry-shaped catalog) and a `MANIFEST.txt` with sha256 checksums.

Only extensions compatible with the shipped **stable** channel are included
(wasm API version ≤ 0.7.0, manifest schema ≤ 1), so everything in the bundle is
guaranteed to load. The release bundle is the **complete** stable-compatible set
— every extension the registry's list endpoint returns (~1000, the endpoint's
hard cap, ≈98% of what `zed.dev/extensions` shows). The few beyond the cap need a
newer wasm API this build can't load anyway.

### Rolling your own bundle

If you want a different set (more extensions, a specific subset, or a fresher
snapshot), run the producer on any networked machine that has `bash`, `curl`,
`python3`, and `tar`:

```sh
# curated default set (tools/extensions-allowlist.txt):
tools/snapshot-extensions.sh --zed-version v1.5.4

# everything in the registry that the stable channel can load:
tools/snapshot-extensions.sh --full --zed-version v1.5.4
```

Edit `tools/extensions-allowlist.txt` to control the curated set (one extension
`id` per line). The script writes the bundle to `dist-extensions/`. It only ever
talks to the registry at build time — it is never part of the shipped editor.

## 2. Installing on the air-gapped machine

1. Copy the bundle to the target machine and unpack it:
   ```sh
   tar xzf zedium-extensions-stable-v1.5.4.tar.gz
   ```
   You now have a directory with `extensions/<id>/<version>/archive.tar.gz` files.

2. In Zedium, open the **Extensions** view and click **Install from File**
   (or run the command palette action **`zed: install prebuilt extension`**).

3. Point the picker at either:
   - an extension's `archive.tar.gz` (e.g.
     `extensions/catppuccin/0.2.25/archive.tar.gz`), **or**
   - an already-unpacked extension directory (one containing `extension.toml`).

   Zedium copies it into your installed extensions, with **no network access and
   no compilation**, and activates it immediately (themes show up in the theme
   picker, languages/grammars become available, etc.).

That's it. Repeat per extension you want. Installed extensions persist like any
other and survive restarts.

## Notes & limitations

- **No recompilation.** Zedium installs *prebuilt* extensions only. If you point
  it at an extension that ships Rust **source but no compiled `extension.wasm`**
  (i.e. it has a `Cargo.toml` and no built artifact), the install is refused with
  a message telling you to build it on a networked machine first. The release
  bundle always contains prebuilt archives, so this only affects hand-assembled
  inputs.
- **Compatibility.** The bundle is filtered to what the stable channel can load.
  An extension built only against a newer extension API (wasm API > 0.7.0) is not
  included and would be rejected at load time.
- **Updates.** There is no auto-update. To upgrade an extension, install a newer
  archive over it (it replaces the previous copy).
- **"Install Dev Extension"** still exists for extension *authors*; it compiles
  from source and needs the Rust/wasm toolchain, so it is not the air-gap path.
  Use **Install from File** for prebuilt content.
