# Zedium

A telemetry-free, cloud-free, fully open-source fork of [Zed](https://github.com/zed-industries/zed).

**Not affiliated with Zed Industries.** Zedium is a derivative work of Zed and shares its [AGPL-3.0](zed/LICENSE-AGPL) license.

## What's removed

- All telemetry, metrics, and crash/error reporting
- Zed-Industries-hosted services (collab, sign-in, LLM proxy, extension registry network calls)
- Auto-update (replace the binary to update)
- Direct hardcoded LLM provider integrations (Anthropic, OpenAI, Gemini, Bedrock, Copilot, …)

## What's kept

- The full Zed editor experience
- Local + arbitrary OpenAI-compatible LLM endpoints (works with Ollama, llama.cpp, vLLM, LM Studio, …)
- Edit prediction (Zeta) against a user-configured local endpoint — see [docs/EDIT_PREDICTION.md](docs/EDIT_PREDICTION.md)
- A static snapshot of the Zed extension registry, bundled with each release

## Repo layout

```
.
├── zed/                       # git submodule → zed-industries/zed at the baseline tag
├── patches/                   # numbered .patch series, the canonical strip
├── crates/                    # Zedium's own Rust crates (referenced via path-dep patches)
├── tools/                     # verifier, snapshot pipeline
├── docs/                      # fork documentation (PLAN, DISCOVERY, MAINTAINING, …)
├── .github/workflows/         # CI
└── justfile                   # build orchestration
```

## Building from source

```sh
just init        # one-time: submodule init + apply patches
just build       # cargo build
just run         # cargo run
```

See [`docs/INSTALL.md`](docs/INSTALL.md) for prebuilt binaries (when available) and platform notes.

## Development

```sh
just apply       # reset zed/ to baseline + apply patches/ as commits
just verify      # static checks: forbidden strings + banned crates
just export      # re-export commits in zed/ back to patches/
```

See [`docs/PLAN.md`](docs/PLAN.md) for the full fork plan and [`docs/DISCOVERY.md`](docs/DISCOVERY.md) for the cloud-surface inventory.
