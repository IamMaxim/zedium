# Installing Zedium

Zedium is a telemetry-free, cloud-free fork of [Zed](https://github.com/zed-industries/zed).
It ships as a plain archive — there is no auto-updater and no account.

## Platforms

| Platform | Artifact |
|---|---|
| Linux x86_64 | `zedium-<tag>-linux-x86_64.tar.gz` |
| Linux aarch64 | `zedium-<tag>-linux-aarch64.tar.gz` |
| macOS arm64 | `Zedium-<tag>-arm64.zip` |
| macOS x86_64 | `Zedium-<tag>-x86_64.zip` |

Tags use the form `v1.4.2-1` (upstream tag + fork revision).

## Linux

```sh
tar xzf zedium-<tag>-linux-x86_64.tar.gz
cd zedium-<tag>
./bin/zedium
```

To put it on your `PATH`, symlink `bin/zedium` into `~/.local/bin/`.

User data lives under XDG paths keyed to **Zedium**, so it never collides with an upstream Zed
install:

- Config: `$XDG_CONFIG_HOME/zedium` (default `~/.config/zedium`)
- Data: `$XDG_DATA_HOME/zedium` (default `~/.local/share/zedium`)

## macOS

The app is **ad-hoc signed** (no Apple Developer ID). Gatekeeper will quarantine it on first
launch. Remove the quarantine attribute:

```sh
unzip Zedium-<tag>-arm64.zip
xattr -d com.apple.quarantine Zedium.app
open Zedium.app
```

Bundle identifier: `dev.zedium.Zedium`. User data: `~/Library/Application Support/Zedium`,
config `~/.config/zedium`.

## Updating

There is no in-app updater. Download the newer archive and replace the binary/app bundle. Your
config and data directories are preserved across versions.

## First run

- No sign-in, no telemetry, no network calls on startup.
- AI features (assistant + edit prediction) are **off until you configure a provider**. See
  [EDIT_PREDICTION.md](EDIT_PREDICTION.md) for self-hosted edit prediction; configure chat
  models under `language_models` in `settings.json` (Ollama / LM Studio / any OpenAI-compatible
  endpoint).
- The extension registry is a static offline snapshot bundled with the release; there are no
  live extension updates between releases.
