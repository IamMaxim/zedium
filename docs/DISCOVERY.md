# Discovery — Zed v1.4.2 cloud surface

This document is the test oracle for the verifier and the punchlist for the strip patch series. It is the output of Phase 0 of [PLAN.md](PLAN.md).

Scope: every code path that touches the network, identifies the user, or hardcodes a Zed-Industries-owned domain/brand string. Read paths are `crates/<crate>/src/<file>.rs`.

## 1. Central pivot points (good news)

Two files do most of the URL routing for Zed-owned services. Patching these gives broad coverage:

| File | Lines | What it does |
|---|---|---|
| `crates/http_client/src/http_client.rs` | 212–260 | `build_zed_api_url`, `build_zed_cloud_url`, `build_zed_llm_url`. Translates `https://zed.dev` → `api.zed.dev` / `cloud.zed.dev` / etc. |
| `crates/client/src/zed_urls.rs` | full file | User-facing page URLs (`account_url`, `start_trial_url`, `terms_of_service`, `ai_privacy_and_security`, `edit_prediction_docs`, `acp_registry_blog`, …) |

Base URL comes from `ClientSettings::server_url`, defaulting to `https://zed.dev`, overridable by `ZED_SERVER_URL` env. RPC is separately overridable by `ZED_RPC_URL`.

## 2. Crate inventory (236 crates)

### Strip wholesale (delete crate + all references)

| Crate | Reason |
|---|---|
| `telemetry` | Event queue macro (66 LOC). Producers everywhere. |
| `telemetry_events` | Event schemas. |
| `client` | RPC, sign-in, telemetry upload, server URL. Pervasive consumer — needs careful replacement; see §3. |
| `collab` | Collab server (backend). |
| `collab_ui` | Collab UI (channels, share, presence). |
| `call` | Voice/video call (depends on LiveKit). |
| `channel` | Collab channels feature. |
| `livekit_api` | LiveKit voice/video API. |
| `livekit_client` | LiveKit voice/video client. |
| `notifications` | Collab notifications. |
| `crashes` | Minidump capture. |
| `cloud_api_client` | Zed-cloud REST client. |
| `cloud_api_types` | Zed-cloud schemas. |
| `cloud_llm_client` | Zed-hosted LLM proxy client (includes `predict_edits_v3` Zeta wire format). |
| `language_models_cloud` | Zed-hosted LLM provider implementation. |
| `auto_update` | Auto-updater. |
| `auto_update_ui` | Auto-updater UI. |
| `auto_update_helper` | Windows updater helper. |
| `feedback` | Feedback panel (open URL to Zed bug form). |
| `copilot` | GitHub Copilot client. |
| `copilot_chat` | GitHub Copilot chat. |
| `copilot_ui` | Copilot UI. |
| `anthropic` | Direct Anthropic provider. |
| `open_ai` | Direct OpenAI provider. |
| `google_ai` | Direct Gemini provider. |
| `bedrock` | AWS Bedrock provider. |
| `mistral` | Mistral provider. |
| `codestral` | Codestral provider. |
| `deepseek` | DeepSeek provider. |
| `x_ai` | xAI provider. |
| `open_router` | OpenRouter provider (also sends `HTTP-Referer: https://zed.dev` — see §4). |
| `lmstudio` | LM Studio provider (kept by upstream, but we'd rather route via `zedium_provider`; revisit). |
| `ai_onboarding` | Zed-AI signup flow. |
| `web_search_providers` (file `cloud.rs` only) | Strip `cloud.rs`; rest of crate may be keepable. |
| `oauth_callback_server` | Used for Zed sign-in OAuth. |
| `nc` | Suspected Zed internal CLI shim — confirm. |
| `eval_cli` / `eval_utils` | Zed-internal evals; not needed for editor. |
| `edit_prediction_cli` / `edit_prediction_metrics` | Used for upstream's Zeta training data collection. |
| `language_onboarding` | Likely Zed-AI tied; confirm in patch phase. |
| `agent_skills` / `skill_creator` / `rules_library` | Zed-cloud-tied agent skills; confirm. |
| `opencode` | Suspected Zed-cloud agent integration; confirm. |

### Patch (modify but keep)

| Crate | What changes |
|---|---|
| `http_client` | Remove `build_zed_api_url`, `build_zed_cloud_url`, `build_zed_llm_url`, hostname rewrites (lines 212–260). |
| `release_channel` | Strip `ZED_DOCS_URL` constant. |
| `theme_importer` | Strip `ZED_THEME_SCHEMA_URL`; bundle the JSON schema if needed. |
| `edit_prediction` | Keep crate. Strip `zeta.rs`, `zed_edit_prediction_delegate.rs`, `license_detection.rs` (if it phones home — verify); keep `open_ai_compatible.rs`, `ollama.rs`, `fim.rs`, prompt building, UI integration. See §5. |
| `edit_prediction_ui` | Keep, strip any "sign in to Zed" upsell paths. |
| `edit_prediction_context` / `edit_prediction_types` | Probably keep as-is. |
| `language_models` | Strip provider/*.rs for stripped providers (anthropic, open_ai, google_ai, bedrock, mistral, codestral, deepseek, x_ai, open_router, openai_subscribed); add `provider/zedium.rs`. Settings shape updated. |
| `language_model` / `language_model_core` | Keep; trait + types only. |
| `agent` / `agent_ui` / `agent_servers` / `acp_thread` / `acp_tools` / `agent_settings` | Keep. Strip any direct cloud calls. |
| `extension_host` | Replace `build_zed_api_url("/extensions/…")` calls with reads from local snapshot. See §6. |
| `extension` / `extension_api` / `extensions_ui` | Keep core; strip the "Browse extensions store online" UI affordances and `https://zed.dev/docs/...` external doc links. |
| `extension_cli` | Keep if used; verify it doesn't push to a Zed-owned registry. |
| `extension_host` | Replace `fetch_extensions_from_api` calls (lines 548, 584, 829, 874) with local snapshot reads. |
| `workspace`, `editor`, `multi_buffer`, `project`, etc. | Strip embedded `zed.dev` doc links; remove sign-in upsells in error notifications. |
| `zed` (main binary) | Strip `reliability.rs` minidump upload; strip `crashes::init` startup call; strip telemetry start; strip `client` instantiation; replace branding strings. |
| `zed_credentials_provider` / `zed_env_vars` / `zed_actions` | Keep, but audit for brand strings (these underpin the binary). |
| `migrator` | Audit; if it points users at zed.dev docs, strip. |
| `onboarding` | Strip the "Sign in to Zed" / AI onboarding paths. |

### Keep as-is

The rest (~150 crates): `gpui*`, `ui*`, `text`, `language*` (except `language_models*`), `editor`, `terminal*`, `project_panel`, `file_finder`, `vim`, `git*`, `lsp`, `dap*`, `debugger*`, `theme*` (except `theme_importer`), `prettier`, `assets`, `fs`, `db`, `sqlez*`, `rope`, `sum_tree`, `task`, `tasks_ui`, `markdown*`, `picker`, `settings*`, `search`, `snippet*`, `outline*`, `journal`, `repl`, `image_viewer`, `svg_preview`, `csv_preview`, `dev_container`, `which_key`, `command_palette*`, `recent_projects`, `remote*`, `proto`, `rpc`, `node_runtime`, `reqwest_client`, `http_client_tls`, `aws_http_client` (audit — name only references AWS HTTP; not Bedrock), `net`, `audio`, `media`, `denoise`, `clock`, `paths`, `util*`, `collections`, `fuzzy*`, `breadcrumbs`, `activity_indicator`, `title_bar`, `platform_title_bar`, `menu`, `picker`, `feature_flags*` (audit — is the feature-flag fetch local-only?), etc.

Confidence note: this classification is from one read pass. Each "patch" entry must be reverified during its commit; "strip" entries that turn out to have non-cloud dependents will surface as build failures during the strip and be downgraded to "patch".

## 3. URL/domain inventory

### Zed-owned hosts (all forbidden post-strip)

```
zed.dev                       crates/http_client/src/http_client.rs:217,233,246,259
                              crates/client/src/zed_urls.rs              (all helpers)
                              crates/release_channel/src/lib.rs:ZED_DOCS_URL
                              crates/theme_importer/src/main.rs:ZED_THEME_SCHEMA_URL
                              crates/feedback/src/feedback.rs:ZED_REPO_URL
                              crates/extensions_ui/src/extensions_ui.rs:1595-1686 (many doc links)
                              crates/open_router/src/open_router.rs:450,543 (HTTP-Referer header)
                              crates/zed/resources/windows/zed.iss:6-8
api.zed.dev                   crates/http_client/src/http_client.rs:217
api-staging.zed.dev           crates/http_client/src/http_client.rs:218
cloud.zed.dev                 crates/http_client/src/http_client.rs:233,246,259
llm-staging.zed.dev           crates/http_client/src/http_client.rs:260
staging.zed.dev               crates/http_client/src/http_client.rs:218,234,247,260
                              crates/collab/src/lib.rs:153
collab.zed.dev                crates/collab/README.md
staging-collab.zed.dev        crates/collab/README.md
billing-support@zed.dev       crates/ai_onboarding/src/young_account_banner.rs:9
```

### Hardcoded LLM provider hosts (all stripped along with their crates)

```
api.anthropic.com             crates/anthropic/src/anthropic.rs:17
api.openai.com                crates/open_ai/src/open_ai.rs:18
generativelanguage.googleapis.com   crates/google_ai/src/google_ai.rs:10
api.mistral.ai                crates/mistral/src/mistral.rs:9
api.x.ai                      crates/x_ai/src/x_ai.rs:5
api.deepseek.com              crates/deepseek/src/deepseek.rs:12
codestral.mistral.ai          crates/codestral/src/codestral.rs:23
api.githubcopilot.com         crates/copilot_chat/src/copilot_chat.rs:23
api.github.com                crates/copilot_chat/src/copilot_chat.rs:44 (GraphQL)
openrouter.ai                 crates/open_router/src/open_router.rs:14
console.anthropic.com         crates/language_models/src/provider/anthropic.rs:622 (link)
```

### Zed-Industries GitHub references

```
github.com/zed-industries/zed                crates/feedback/src/feedback.rs:ZED_REPO_URL
                                              crates/auto_update/src/auto_update.rs:324,326
github.com/zed-industries/extensions          (implicit — extension API at api.zed.dev)
```

### Telemetry/crash SaaS

```
sentry[…]                     crates/zed/src/reliability.rs:281-377 (Sentry minidump form)
                              crashes uploaded to MINIDUMP_ENDPOINT env (see §4)
```

## 4. Env var inventory

| Var | Where | What |
|---|---|---|
| `ZED_SERVER_URL` | `crates/client/src/client.rs` | Override base URL (default `https://zed.dev`). Strip — replace with no-default. |
| `ZED_RPC_URL` | `crates/client/src/client.rs` | Override RPC URL. Strip (RPC removed entirely). |
| `ZED_ADMIN_API_TOKEN` | `crates/client/src/client.rs` | Zed-internal admin. Strip. |
| `ZED_CLOUD_INTERNAL_API_KEY` | `crates/collab/k8s/collab.template.yml` | Zed-cloud internal. Strip (collab gone). |
| `ZED_OPEN_AI_COMPATIBLE_EDIT_PREDICTION_API_KEY` | `crates/edit_prediction/src/open_ai_compatible.rs` | Keep — this is the user-facing edit-prediction key. Rename to `ZEDIUM_...`. |
| `ZED_MINIDUMP_ENDPOINT` | `crates/client/src/telemetry.rs:92-95` (compile-time `option_env!` + runtime `env::var`) | Sentry-style minidump uploads. Strip (crashes crate gone). |

Also expect `ZED_*` references in `crates/zed_env_vars/`; audit that crate explicitly.

## 5. Zeta inventory

Best news in Phase 0: upstream v1.4.2 **already** supports custom edit-prediction endpoints. The patch #8a workload in the plan can shrink significantly.

### Wire format (in `crates/cloud_llm_client/src/predict_edits_v3.rs`)

Two request shapes:

- `PredictEditsV3Request` — Zed-hosted variant; server receives a structured `ZetaPromptInput` and builds the prompt server-side. **Strip.**
- `RawCompletionRequest` / `RawCompletionResponse` — standard OpenAI text-completion shape (`model`, `prompt`, `max_tokens`, `temperature`, `stop`, `usage`, `choices[].text`). **Keep.**

Custom headers `X-Zed-Predict-Edits-{Mode,Request-Id,Trigger}` are Zed-internal — only sent to the Zed-hosted variant. Strip.

### Client-side prompt construction

`crates/zeta_prompt/` (4 files, ~6000 LOC): excerpt selection, prompt formatting, response parsing. AGPL code, openly available. **Keep** — this is what makes Zeta's open-weights model usable from a custom endpoint.

Cursor marker: `<|user_cursor|>` (defined `crates/zeta_prompt/src/zeta_prompt.rs:23`). Other special tokens defined in the same crate (e.g. `EDITABLE_REGION_END_MARKER` referenced in `edit_prediction/src/zeta.rs`).

### Existing OpenAI-compatible path

`crates/edit_prediction/src/open_ai_compatible.rs` (134 LOC) already:
- Reads endpoint, model, and (optional) API key from settings (`language_settings.edit_predictions.open_ai_compatible_api`)
- Constructs a `RawCompletionRequest`
- POSTs to the user-configured URL
- Parses the response

Companion: `crates/edit_prediction/src/ollama.rs` (108 LOC) does the same for Ollama's native `/api/generate` endpoint with `raw: true` (bypasses Ollama's chat templating).

The settings enum `EditPredictionProvider` already has `Ollama` and `OpenAiCompatibleApi` variants. Other variants exist for the Zed-hosted (Zeta) path — strip those.

### Implications for patch #8a

Revised scope: **not "write a new crate"** but:
1. Strip `crates/edit_prediction/src/zeta.rs` (775 LOC — the Zed-hosted variant).
2. Strip `zed_edit_prediction_delegate.rs` (264 LOC) if it's only used by the Zed-hosted path.
3. Strip `license_detection.rs` (866 LOC) — verify it doesn't phone home; if it does, strip.
4. Remove the `PredictEditsV3Request` variant from `cloud_llm_client/src/predict_edits_v3.rs` (move `RawCompletionRequest/Response` to a new local module since `cloud_llm_client` is going away).
5. Trim the `EditPredictionProvider` enum to `None | Ollama | OpenAiCompatibleApi`.
6. Rename `ZED_OPEN_AI_COMPATIBLE_EDIT_PREDICTION_API_KEY` → `ZEDIUM_EDIT_PREDICTION_API_KEY`.
7. No new crate needed unless we want to formalize prompt+endpoint code into one place.

This is materially smaller than originally estimated. Effort: ½–1 day, not 1 day.

### Model weights

User-side concern; we don't host weights. The setup guide (`docs/EDIT_PREDICTION.md`) will direct users to `huggingface.co/zed-industries/zeta`. License of weights: confirm at doc-writing time; not a fork concern beyond linking.

## 6. Extension store

`crates/extension_host/src/extension_host.rs`:
- `fn fetch_extensions_from_api` (line 658) — single chokepoint. All extension-store traffic flows through it.
- Endpoint paths: `/extensions/updates`, `/extensions/{id}`, `/extensions/{id}/download`, `/extensions/{id}/{version}/download` (lines 548, 584, 829, 874).
- Hostname: derived from `http_client.build_zed_api_url("/extensions/…")` → resolves to `api.zed.dev`.

Strip strategy: replace `fetch_extensions_from_api` body with reads from `$ZEDIUM_EXTENSIONS_DIR/<file>`. Default to `<bundle>/share/zedium/extensions/`. Snapshot layout:
```
<dir>/
├── index.json                 # mimics the /extensions API response
├── <id>-<version>.tar.gz      # one per extension
└── ...
```

Extensions themselves may declare `archive` URLs in their manifests (`crates/extension/src/extension_manifest.rs:233,243,256,604`) — those are extension-author-controlled and out of scope for the strip. They run at extension-install time (which now means snapshot-build time, not user-runtime).

`crates/extension/src/extension_builder.rs:30` references `WASI_SDK_URL` for building wasm extensions. Audit during snapshot pipeline work; either bundle the SDK or accept this as a build-time dependency that doesn't ship.

## 7. Auto-update

`crates/auto_update/src/auto_update.rs`:
- Two visible URLs reference `github.com/zed-industries/zed/commits/{nightly,main}` (lines 324, 326) — these are display links shown to the user, not the update API itself.
- The actual update check uses `http_client.build_zed_api_url(...)` somewhere not yet pinpointed; the whole crate goes anyway.

Strip strategy: delete the crate. Strip `auto_update::init(...)` call site in `crates/zed/src/main.rs`. Document in `docs/INSTALL.md` that users update by replacing the binary.

## 8. Telemetry

Architecture:
- `crates/telemetry/` (66 LOC) — just a queue + `event!` macro. Producers everywhere in the tree.
- `crates/telemetry_events/` — event schemas.
- `crates/client/src/telemetry.rs` — the *uploader* (sends events + minidumps to the server). This is where the actual exfiltration happens.
- `crates/zed/src/reliability.rs` (503 LOC) — Sentry-format minidump capture and upload (lines 263–403). Uses `MINIDUMP_ENDPOINT` from env.
- `crates/crashes/` — minidump capture infra.

Strip strategy:
1. Delete `telemetry`, `telemetry_events`, `crashes` crates.
2. Delete `reliability.rs` and its `crashes::init(...)` call in `main.rs` (around line 392).
3. Delete `client/src/telemetry.rs` (along with the rest of the `client` crate).
4. Provide a stub `telemetry::event!` macro that compiles to nothing, *or* sweep every call site and remove the macro invocations. Recommend the latter — keeping a stub macro is a leakage vector if upstream adds more events later that go through it unnoticed.

Producer count is large (hundreds of `telemetry::event!` calls); use `rg --files-with-matches "telemetry::event!"` to enumerate during patch #3.

## 9. Settings keys (cloud-related)

From `crates/settings_content/src/settings_content.rs`:

| Key | Default | Action |
|---|---|---|
| `telemetry.diagnostics` | `true` | Strip key; crashes gone. |
| `telemetry.metrics` | `true` | Strip key; events gone. |
| `server_url` | `https://zed.dev` | Strip key; no Zed server. |
| `audit_url` | inferred | Strip key. |

From `crates/settings_content/src/project.rs`:
| `disable_ai` | nullable bool | Keep (still meaningful — disables `edit_predictions` and assistant uniformly). |

From `crates/settings_content/src/language.rs`:
| `edit_predictions.{...}` | various | Keep; reshape to drop Zed-hosted provider variants. |

Plus assistant/LLM-provider settings under `language_models` — wholesale replace with `zedium_provider`'s shape.

## 10. Boot-time network surface

From `crates/zed/src/main.rs` (initialization sequence around lines 285–602):

| Phase | Init call | Network? | After strip |
|---|---|---|---|
| Logging | `zlog::init`, `ztracing::init` | No | Keep |
| Paths | `init_paths` | No | Keep |
| Crashes | `crashes::init(...)` (~line 392) | **Yes (minidump uploads on next start)** | Delete |
| Settings | `settings::init` | No | Keep |
| Release channel | `release_channel::init` | No | Keep, strip URL constant |
| Extensions | `extension::init` | **Yes (auto-fetches updates)** | Patch — local snapshot only |
| Languages | `languages::init`, `language_extension::init` | No | Keep |
| Client | `client::init`, `Client::new` instantiated earlier | **Yes (RPC connect + auth)** | Delete client construction; rewire dependents |
| Telemetry | `client.telemetry().start(...)` (~line 601) | **Yes (events upload)** | Delete |
| Reliability | `reliability::init` | **Yes (background minidump upload)** | Delete |
| Auto-update | (somewhere in `zed::init` or a workspace listener) | **Yes (poll for releases)** | Delete |

After strip, boot should be entirely offline. Verify by `strace -f -e network 2>&1 | rg connect` (or netns smoke if we ever turn on runtime verification).

## 11. Known unknowns / verify-during-patch items

These were flagged during discovery but not exhaustively resolved. Each becomes a TODO at its patch commit:

1. **`feature_flags`** — does it fetch flags from a server, or is it purely local? Verify at patch time.
2. **`web_search_providers`** — `cloud.rs` is the strip target; confirm other files (if any) don't depend on it.
3. **`web_search` (the trait crate, if separate)** — keep the trait, strip cloud impl. Audit.
4. **`language_models/src/provider/openai_subscribed.rs`** — appears to be a Zed-subscription variant; strip along with other providers.
5. **`migrator`** — settings migrator; check whether it embeds links/URLs.
6. **`onboarding`** — onboarding flow; strip cloud sign-in steps but keep editor onboarding.
7. **`session`** — likely keep (session restoration), but the name suggests user sessions; audit.
8. **`grammars` crate vs. extension-installed grammars** — extensions that fetch grammars at install. Snapshot pipeline must pre-fetch or skip.
9. **`livekit_client` dependents** — `call` crate. Confirm no other crate calls into it.
10. **`copilot` enabled by `language_models` extension API** — confirm the extension API doesn't expose a way to re-add Copilot externally.
11. **`debugger_ui` / `dap_adapters`** — installs DAP binaries at runtime. Local cache only? Audit.
12. **`prettier`** — installs node modules at runtime? Audit. (May be acceptable as user-initiated.)
13. **`node_runtime`** — downloads Node.js binary at first use? Audit.
14. **`oauth_callback_server`** — sign-in OAuth; strip with `client`.

## 12. Verifier seed lists

Once Phase 0 lands, the following can populate `tools/forbidden-strings.yaml` immediately:

```yaml
forbidden_substrings:
  - "zed.dev"
  - "zed-industries"
  - "api.anthropic.com"
  - "api.openai.com"
  - "generativelanguage.googleapis.com"
  - "api.mistral.ai"
  - "api.x.ai"
  - "api.deepseek.com"
  - "codestral.mistral.ai"
  - "api.githubcopilot.com"
  - "openrouter.ai"
  - "console.anthropic.com"
  - "MINIDUMP_ENDPOINT"
  - "sentry["       # form field prefix in reliability.rs
  - "PREDICT_EDITS_MODE_HEADER_NAME"
  - "X-Zed-Predict-Edits-"
forbidden_words:
  - "Zed"           # brand
  - "Zeta"          # brand product name
  - "Copilot"       # GitHub Copilot
allowed_in:
  LICENSE-AGPL:      ["Zed"]
  README.md:         ["Zed", "Copilot", "Zeta"]   # mentions upstream by name
  docs/PLAN.md:      ["*"]
  docs/DISCOVERY.md: ["*"]
  docs/upstream-attribution.md: ["*"]
  docs/EDIT_PREDICTION.md: ["Zeta"]
```

```toml
# tools/deny.toml — cargo-deny banned crates (workspace-local)
[bans]
deny = [
  "telemetry", "telemetry_events",
  "client", "collab", "call", "channel", "notifications",
  "livekit_api", "livekit_client",
  "crashes",
  "cloud_api_client", "cloud_api_types", "cloud_llm_client",
  "language_models_cloud",
  "auto_update", "auto_update_ui", "auto_update_helper",
  "feedback",
  "copilot", "copilot_chat", "copilot_ui",
  "anthropic", "open_ai", "google_ai", "bedrock",
  "mistral", "codestral", "deepseek", "x_ai", "open_router",
  "ai_onboarding",
  "oauth_callback_server",
]
```

(Names confirmed against `crates/` directory listing during Phase 0.)

## 13. Plan deltas to fold in

1. **Patch #8a effort downgraded** from 1 day → ½ day. Probably no new crate; just trim existing `edit_prediction` machinery.
2. **New patch** between #3 and #4: `strip: remove crashes crate + reliability.rs + MINIDUMP_ENDPOINT`. Worth its own commit so it's reverifiable independently.
3. **New patch** before #11: `extensions: bundle WASI SDK or strip extension building`. Phase 0 found `WASI_SDK_URL` in `extension_builder.rs`; need to decide before extension snapshot pipeline.
4. **Verifier expansion**: add `\bCopilot\b` to forbidden words; add the exact substring list from §12 to `forbidden-strings.yaml`.
5. **Phase 0 unknowns become a checklist** in `docs/MAINTAINING.md` so each subsequent upstream merge re-verifies the items in §11 hasn't regressed.
6. **Document the centralization win** in `MAINTAINING.md`: the `http_client.rs` URL pivot point and the `client/zed_urls.rs` helper file are the two places where most domain regressions would land. Code review of upstream merges should explicitly diff these.
