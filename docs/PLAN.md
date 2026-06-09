# Zedium — Fork Plan

A telemetry-free, cloud-free, fully open-source fork of [Zed](https://github.com/zed-industries/zed) that tracks upstream stable releases.

## Decisions (locked)

| Topic | Choice |
|---|---|
| Strip scope | Full air-gap. No outbound network calls except those the user explicitly configures. |
| Upstream tracking | Vanilla upstream Zed as a git submodule pinned to the baseline tag. Fork changes live in `patches/*.patch` at parent level — applied to zed/ by `just apply`. Tracking upstream = bump submodule SHA + rebase patches. Permanent quilt model. |
| Distribution | Public release artifacts + bundled static extension snapshot. |
| Branding | Rename to **Zedium**. New binary name, new bundle ID, new config dir. |
| AI / LLM (chat) | Remove all hardcoded chat-provider crates (Anthropic, OpenAI, Gemini, Bedrock, …). Add one new `zedium_provider` crate that speaks OpenAI-compatible HTTP to user-configured endpoints only. Assistant UX is preserved. |
| AI / LLM (edit prediction) | Keep `edit_prediction` + `edit_prediction_ui` upstream crates intact. Replace upstream's `zeta` crate with a new `zedium_edit_predictor` crate that talks to a user-configured text-completion endpoint (open-weights Zeta runs locally via vLLM / llama.cpp / Ollama). Default: disabled. |
| Platforms | Linux x86_64, Linux aarch64, macOS arm64, macOS x86_64. |
| Extension registry | Static snapshot bundled inside the app bundle. No live updates between releases. |
| Patch granularity | Many small surgical commits, each independently buildable + verifier-green. |
| Release cadence | Monthly or on-demand against the latest upstream stable tag. |
| macOS signing | Ad-hoc signed, unsigned distribution. README documents quarantine removal. |
| Verification | Static only: `cargo deny` + ripgrep forbidden-strings check, in CI. |
| Settings migration | None. Zedium is a clean-slate app from the user's perspective. |
| Hosting | GitHub. CI on GitHub Actions. |
| Tag format | `v1.4.2-1`, `v1.4.2-2`, `v1.5.0-1`, … (trailing int = fork revision against that upstream tag). |

## Phase 0 — Discovery

Output: `docs/DISCOVERY.md`. Must precede any patch work.

- Crate inventory: each of upstream's ~240 crates classified `keep` / `strip` / `patch`.
- URL/domain inventory: ripgrep `https://`, `zed.dev`, `.anthropic.`, `.openai.`, `googleapis`, `zed-industries`, telemetry-SaaS DSN patterns.
- Endpoint env-vars: `ZED_.*_URL`, `*_API_KEY`, `CLIENT_ID`.
- Settings keys gating cloud features.
- Boot-time network surface: `main.rs`, `cli/`, `auto_update`, `feedback`, anything called during startup.
- **Zeta-specific inventory (for `zedium_edit_predictor`):**
  - Exact endpoint shape the upstream `zeta` crate uses (standard OpenAI `/v1/completions` vs. a Zed-proprietary `/predict` shape).
  - Prompt template + special tokens — must match the open-weights model card exactly.
  - License of the weights on Hugging Face.
  - Tokenizer expectations and any non-standard chat-template handling.
  - Response parsing: how the predicted edit is decoded back into a buffer diff.

The output becomes the test oracle for the verifier and the checklist for the patch series.

## Repo layout

The fork is a parent repo that pulls upstream Zed in as a git submodule.
Fork-owned files live at parent level. The submodule is treated as vanilla
upstream — we never commit to it permanently; patches are applied to its
working tree by `just apply` (which creates a transient `zedium-applied`
branch in the submodule).

```
github.com/<owner>/zedium       # the fork (this repo)
├── README.md                   # user-facing overview, AGPL, non-affiliation
├── justfile                    # build orchestration (init / apply / verify / build / merge-upstream)
├── .gitmodules                 # zed/ → upstream Zed at baseline tag
├── .github/workflows/
│   ├── verify.yml              # CI: apply patches + run verify on push/PR
│   └── release.yml             # added later by Phase 5
├── docs/
│   ├── PLAN.md                 # this file
│   ├── DISCOVERY.md            # Phase 0 cloud-surface inventory
│   ├── MAINTAINING.md          # per-release runbook (Phase 8)
│   ├── EDIT_PREDICTION.md      # self-hosted edit-prediction guide
│   └── INSTALL.md              # user install + quarantine notes
├── tools/
│   ├── verify.sh               # forbidden-strings + banned-crates checker
│   ├── forbidden-strings.txt          # regex patterns
│   ├── forbidden-strings.allowlist    # path prefixes exempted
│   ├── banned-crates.txt              # zed/crates/<name> dirs that must not exist
│   ├── deny.toml                      # cargo-deny config (external deps)
│   ├── patches-export.sh              # wraps git format-patch zed→patches
│   ├── patches-apply.sh               # wraps git am patches→zed
│   └── snapshot-extensions/           # Phase 6: extension mirror builder
├── patches/                    # the canonical patch series (first-class artifact)
│   ├── 0001-strip-telemetry.patch
│   └── …
├── crates/                     # Zedium's own Rust crates (zedium_provider, …)
│   │                           # pulled into zed's workspace by a patch that adds
│   │                           # ../../crates/<name>/ as a workspace member.
│   └── …
└── zed/                        # git submodule → zed-industries/zed @ baseline tag
```

Key points:

- Parent repo **never** holds a copy of the upstream Zed tree. Upstream lives
  only inside the submodule.
- `patches/*.patch` is the durable source of truth for all editor-tree changes.
  These are normal `git format-patch` output, reviewable as plain text.
- `crates/` (parent-level) holds Zedium-owned editor crates. They live outside
  the submodule for clean separation; a patch in `patches/` adds them as
  workspace members of `zed/Cargo.toml` via a relative path (`../crates/<name>`).
- The submodule's working tree IS modified during a build (patches are applied
  via `git am` onto a `zedium-applied` branch). Submodule HEAD as recorded in
  `.gitmodules` is always the pristine baseline tag — never our applied branch.

## Branches & remotes

Parent repo (`github.com/<owner>/zedium`):
- `origin` → the fork's GitHub repo
- Branches: `main` is the only long-lived branch. Release lines (e.g.
  `release/v1.4`) are cut as needed for backporting.

Submodule (`zed/`):
- Remote `origin` → `github.com/zed-industries/zed`
- HEAD recorded in `.gitmodules` is the pristine baseline tag (e.g. `v1.4.2`).
- `just apply` creates a local `zedium-applied` branch off the baseline and
  `git am`s the patch series onto it. This branch lives only in the submodule's
  local clone — it's never pushed and not tracked by the parent.

## Patch series

Each entry is one numbered file in `patches/`. Each must be independently
buildable + verifier-green after `just apply` reaches it. Tooling (justfile,
verifier, CI workflow) is no longer a patch — those files live at parent level
and are committed directly to the parent repo, not applied to the submodule.

| File | Concern | Notes |
|---|---|---|
| `0001-strip-telemetry.patch` | Remove telemetry crate + call sites | 188 `event!` invocations across 32 crates. `telemetry_events` is kept (consumed by later patches' targets). |
| `0002-strip-client-collab.patch` | Remove client, collab*, call, channel, livekit_*, notifications | Sign-in UI, share buttons, account menu, voice/video. |
| `0003-strip-auto-update.patch` | Remove auto_update, auto_update_ui, auto_update_helper | Users replace the binary to update. |
| `0004-strip-crashes-reliability.patch` | Remove crashes crate + reliability.rs + MINIDUMP_ENDPOINT | Panics still print locally. |
| `0005-strip-feedback.patch` | Remove feedback panel | Replace menu entry with link to fork's GitHub issues. |
| `0006-strip-copilot.patch` | Remove copilot, copilot_chat, copilot_ui | Keeps edit_prediction + edit_prediction_ui. |
| `0007-predict-zedium-edit-predictor.patch` | Replace upstream zeta crate path with OpenAI-compatible-only path | See "Edit-prediction replacement" below. |
| `0008-strip-zed-llm-client.patch` | Remove zed_llm_client + Zed-hosted LLM provider in language_models | |
| `0009-llm-zedium-provider.patch` | Remove Anthropic/OpenAI/Gemini/Bedrock/etc. provider crates; add zedium_provider via path-dep to ../crates/zedium_provider | See "Provider replacement" below. |
| `0010-extensions-local-snapshot.patch` | Replace `fetch_extensions_from_api` with local snapshot reads | Reads `$ZEDIUM_EXTENSIONS_DIR`, default `<bundle>/share/zedium/extensions/`. |
| `0011-brand-rename.patch` | Binary name, bundle ID, config dir → zedium | Config dir `~/.config/zedium/`. |
| `0012-brand-strings.patch` | Replace user-visible "Zed" strings → "Zedium" | Window titles, menus, dialogs, About panel. |
| `0013-cargo-metadata.patch` | Update workspace metadata (name, repo, homepage, license notice) | |

Numbering is apply-order. Each patch's diff is reviewable in isolation.

### Provider replacement (patch #10)

- **Remove** crates: `anthropic`, `open_ai`, `bedrock`, `mistral`, `deepseek`, `vercel`, `copilot` (provider half), `zeta` (and any others discovered in Phase 0).
- **Add** crate `zedium_provider`:
  - One implementation: OpenAI-compatible HTTP.
  - Zero hardcoded endpoints. Endpoint, auth-header template, and model list all come from user settings.
  - Registers N instances with the `language_models` registry — one per user-configured provider — so the model picker shows real provider names and models.
- **Settings shape:**
  ```jsonc
  "language_models": {
    "providers": [
      {
        "name": "local-ollama",
        "base_url": "http://127.0.0.1:11434/v1",
        "auth_header": null,
        "models": ["llama3.1:70b", "qwen2.5-coder:32b"]
      }
    ]
  }
  ```
- **Default state:** no providers configured → assistant shows an empty-state with a link to settings. UI surfaces (assistant panel, inline AI, slash commands) all still exist.
- **Result:** verifier's forbidden-domains list can be unconditional — no production code path ever names `api.anthropic.com` et al.

### Edit-prediction replacement (patch #8a)

- **Remove** upstream `zeta` crate (HTTP client + Zed-hosted endpoint + prompt construction).
- **Add** crate `zedium_edit_predictor`:
  - Single implementation: OpenAI-compatible `/v1/completions` (text completion, *not* chat).
  - Zero hardcoded endpoints. Endpoint, auth header, prompt template, special tokens, debounce — all from settings.
  - Prompt template defaulted to the format the open-weights model expects; user can override if they're running a differently-trained variant.
  - Response parser converts model output into a buffer diff that `edit_prediction` consumes unmodified.
- **Settings:**
  ```jsonc
  "edit_prediction": {
    "enabled": false,
    "endpoint": "http://127.0.0.1:8000/v1/completions",
    "auth_header": null,
    "max_context_tokens": 16384,
    "debounce_ms": 250
  }
  ```
- **Default state:** disabled. No predictions, no UI noise. Matches clean-slate principle.
- **User-facing doc:** `docs/EDIT_PREDICTION.md` — setup instructions for vLLM, llama.cpp, and Ollama backends; latency/context tuning; troubleshooting (status-bar indicator, verifying the endpoint is firing).

## Patch lifecycle

Permanent quilt model. `patches/*.patch` is the source of truth for all
editor-tree changes — always. There is no "cutover" phase; the same workflow
applies during bootstrap, during the first release, and forever after.

### Daily development loop

```sh
just init                # one-time: submodule init + apply patches
# … edit a patch — typically by working in zed/ directly:
just apply               # reset zed to baseline + git am all patches/*.patch
                         #   → zed/ now on `zedium-applied` branch
cd zed && git rebase -i v1.4.2   # reorder/squash/edit/drop any patch in place
cd ..
just export              # re-export commits in zed/ back to patches/
just verify              # static gate (forbidden-strings + banned-crates)
just build && just run   # iterate
```

For adding a new patch:

```sh
just apply               # ensure zed/ is on zedium-applied with all patches
# … make changes in zed/, then:
git -C zed add -A && git -C zed commit -m "strip: …"
just export              # writes the new patch to patches/
just verify && just build
```

### `git rerere`

The submodule's local clone has `rerere.enabled true` set by `just init`. The
first time you resolve a conflict during `just apply` after bumping the
submodule baseline, git records the resolution. Subsequent runs of `just apply`
replay it automatically — significantly reducing per-release toil.

### Patches are first-class artifacts

`patches/*.patch` is checked into the parent repo and reviewed in PRs like any
other code. Patches use stable filenames with sequence prefixes
(`0001-strip-telemetry.patch`, …) so reviewers see the series order at a glance
and `git am patches/*.patch` applies them deterministically.

### Stacked Git (optional)

[Stacked Git (`stg`)](https://stacked-git.github.io/) treats each patch as a
first-class object with `stg push/pop/refresh`. Nicer than `git rebase -i` for
heavy patch-series editing, at the cost of a dependency. Optional — plain
rebase is enough for most cases.

## Verifier (static-only, `just verify`)

Three checks; all run in CI on every push/PR and gate every upstream merge into `main`.

**a. cargo-deny (`tools/deny.toml`)** — bans by crate name (filled from Phase 0):
```toml
[bans]
deny = [
  { name = "zed_llm_client" },
  { name = "telemetry" },
  # ...
]
```

**b. Forbidden-strings (`tools/forbidden-strings.yaml`)** — regex list with reasons + line-pinned allowlist:
```yaml
- pattern: 'zed\.dev'
  reason: "Zed-owned domain"
- pattern: 'api\.anthropic\.com'
  reason: "Hardcoded provider endpoint"
- pattern: 'api\.openai\.com'
  reason: "Hardcoded provider endpoint"
- pattern: 'generativelanguage\.googleapis\.com'
  reason: "Hardcoded provider endpoint"
- pattern: 'collab\.'
  reason: "Collab service"
- pattern: '\bZed\b'
  reason: "Brand leakage"
  allowed_in:
    - "LICENSE-AGPL"
    - "docs/upstream-attribution.md"
    - "README.md"  # mentions upstream by name
- pattern: '\bZeta\b'
  reason: "Zed-brand product name for the edit-prediction model"
  allowed_in:
    - "docs/EDIT_PREDICTION.md"  # references the open-weights model card
    - "docs/upstream-attribution.md"
```
Implementation: small Rust binary in `tools/verify/`. Output is line-precise so reviewers see the regression.

**c. Workspace metadata sanity** — package name = `zedium`, repository URL points at the fork, license fields populated.

Known limitation: URLs constructed at runtime from string fragments would slip past. Accepted; revisit only if it actually happens.

## Upstream-tracking workflow

`just merge-upstream <tag>` runs:

1. `cd zed && git fetch --tags && git checkout <tag>`
2. `cd .. && git add zed` — stages the submodule SHA bump
3. `just apply` — `git am` the patches onto the new baseline. **Conflicts surface here**; resolve with `git am --resolved` after fixing.
4. `just export` — re-export patches/ (conflict resolutions become permanent in the patch files)
5. `just verify`
6. `just build && just test`
7. Manual ~10-min smoke launch
8. Commit: parent gets both the submodule SHA bump and any patches/ updates.
9. Tag `v<upstream>-<rev>` on parent, push, trigger release pipeline.

The many-small-patches choice pays off here: conflicts surface only on the
specific patches whose territory upstream touched. Each conflict is "delete
this re-added line" or "Zedium → Zedium again" — fast to resolve, and `git
rerere` replays past resolutions for free.

## Release pipeline (GitHub Actions)

Triggered by tags matching `v*-*`.

| Job | Runner | Artifact |
|---|---|---|
| linux-x86_64 | `ubuntu-latest` | `zedium-<tag>-linux-x86_64.tar.gz` |
| linux-aarch64 | `ubuntu-24.04-arm` | `zedium-<tag>-linux-aarch64.tar.gz` |
| macos-arm64 | `macos-14` | `Zedium-<tag>-arm64.zip` (ad-hoc signed) |
| macos-x86_64 | `macos-13` | `Zedium-<tag>-x86_64.zip` (ad-hoc signed) |
| extension-snapshot | `ubuntu-latest` | bundled into each platform artifact |
| publish | needs all above | GitHub Release with changelog + checksums |

PR CI runs only linux-x86_64 build + verify + tests for speed.

Changelog: a script diffs the upstream tag range (e.g. `v1.4.2..v1.5.0`) and lists user-relevant upstream entries plus fork-only commits since the last release. Manual review before publish.

## Extension snapshot pipeline

`tools/snapshot-extensions/`:
- Input: `zed-industries/extensions` at a pinned SHA per fork release.
- For each extension manifest: fetch tarball, fetch grammars/binaries if applicable, validate.
- Output: a flat directory + index JSON, tarred + zstd-compressed.
- Bundled into each platform artifact under `share/zedium/extensions/`.
- Patched extension store reads `$ZEDIUM_EXTENSIONS_DIR`, default `<bundle>/share/zedium/extensions/`.
- No live updates between releases.

Open detail for Phase 0: some extensions fetch grammars or LSP binaries at install time. Either pre-bundle or strip per-extension during the snapshot job.

## Cadence & labor

- **Default:** first Monday of each month — merge latest upstream stable tag.
- **Triggered:** upstream security fix, or a desired feature.
- **Skip:** if upstream hasn't moved meaningfully in a month.
- **Per-cycle effort (once stable):** 1–3h conflict resolution + 1h smoke testing + CI build wait. Heavier the first 2–3 cycles.

## License & legal

- AGPL-3.0 (inherited). All fork code remains open.
- README: explicit non-affiliation with Zed Industries; "derivative work of Zed".
- AGPL § 13: static extension mirror is not an interactive network service; no source-disclosure obligation triggers from it. Documented.
- Trademark: "Zedium" everywhere user-facing. Verifier's `\bZed\b` check guards against leakage. Fork-original logo/icon.

## Risk register

| Risk | Mitigation |
|---|---|
| Upstream refactor touches a stripped crate → painful merge | Keep patches surgical; subscribe to upstream release notes; never refactor stripped code beyond what's needed. |
| New upstream cloud call inside a *kept* crate (e.g. assistant quietly adds telemetry) | Verifier's forbidden-strings catches new domains; manually review diffs in kept-but-patched areas. |
| Runtime-constructed URLs slip past static check | Accepted limitation. Move to runtime sandbox check if it ever materially happens. |
| Extension manifest format changes upstream | Pin snapshot tooling; update when upstream changes the format. |
| macOS unsigned UX | README documents `xattr -d com.apple.quarantine`. Acceptable per decision. |
| User confuses Zedium binary for upstream Zed | Different binary name + config dir prevents collision. |
| `-pre` / nightly tags trip monthly job | Only follow tags matching `^v\d+\.\d+\.\d+$`. |
| Provider model lists go stale | Static curated default lists; user overrides in settings. Refresh per fork release. |

## Phasing (effort estimates)

| Phase | Effort | Output | Status |
|---|---|---|---|
| 0. Discovery | 1–2 days | `docs/DISCOVERY.md` | ✓ done |
| 1. Parent repo + submodule + verifier + CI + patch tooling | 1 day | parent repo, `tools/`, `justfile`, `.github/workflows/verify.yml`, submodule pinned at `v1.4.2` | ✓ done |
| 2. Strip patch series | 3–5 days | `patches/0001` through `patches/0008` and `0010` (telemetry, client/collab, auto_update, crashes, feedback, copilot, zed_llm_client, extensions) | in progress |
| 3. `zedium_provider` crate (parent `crates/` + path-dep patch) | 1–2 days | `patches/0009-llm-zedium-provider.patch` + `crates/zedium_provider/` | |
| 3a. Edit-prediction trim | ½ day | `patches/0007-predict-zedium-edit-predictor.patch` | |
| 3b. `docs/EDIT_PREDICTION.md` | ½ day | setup guide for vLLM / llama.cpp / Ollama | |
| 4. Brand rename | 1 day | `patches/0011`, `patches/0012` | |
| 5. Release pipeline | 2 days | `.github/workflows/release.yml`, Linux first then macOS | |
| 6. Extension snapshot pipeline | 1–2 days | `tools/snapshot-extensions/` | |
| 7. First release | ½ day | Tag `v1.4.2-1` on parent, publish artifacts | |
| 8. Maintainer docs | ½ day | `docs/MAINTAINING.md` (per-release runbook) | |

Total: ~2 weeks of focused work to first release; ~½ day per month thereafter
for upstream-bump cycles.

Note: with the parent+submodule layout there is no Cutover phase. The patch
series is the canonical artifact from day one and stays that way forever.
First release = tag the parent repo at the SHA where everything is green.
