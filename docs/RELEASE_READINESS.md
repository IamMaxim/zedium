# Release Readiness — Zedium v1

## 0. Rebased to upstream v1.5.4 (2026-06-09)

The baseline was bumped **`v1.4.2` → `v1.5.4`** (3 minor releases). The 44-patch
v1.4.2 series was replayed with `git am -3`; conflicts were resolved preserving
upstream changes and re-applying the strip. Result: **44 patches**, a full
`cargo build -p zed` green, `./tools/verify.sh` green (**40 patterns / 24 banned
crates**), and the **GAP-8 runtime air-gap smoke PASS on v1.5.4** (see §4).

> **Buildability note (corrected 2026-06-09).** An initial `cargo check -p zed`
> reported clean, but a full `cargo build` then surfaced two crates left with
> dangling references by the cross-version strip: `extensions_ui` still
> referenced the v1.5.4-new ACP-registry *upsell* (`zed_urls::acp_registry_blog`,
> `show_acp_registry_upsell`, a dead `render_acp_registry_upsell`), and
> `edit_prediction` had lost the (non-cloud) `BufferDiff`/`BufferEditSource`/
> `WorktreeId` imports while retaining a cloud-Zeta credential/backoff guard
> (`self.client`, `request_backoff_active`). Both were fixed and folded into their
> semantic patches (the ACP-upsell removal into **0035**, the edit-prediction
> repair into **0024**). The `git am -3` replay had also left `Cargo.lock`
> partially merged (still listing removed crates); the reconciled lockfile is
> patch **0044**. Lesson: `cargo check` is necessary but not sufficient — gate the
> rebase on a full `cargo build`.

Notable resolutions during the rebase:

- **ACP agent registry — re-engineered.** Upstream v1.5.4 refactored this subsystem:
  it merged the old `Extension` (keep) and `Registry` (strip) `CustomAgentServerSettings`
  variants into a single `Registry` variant (`#[serde(alias = "extension")]`) backed by
  `AgentRegistryStore`, with an `EXTENSION_TO_REGISTRY_IDS` migration path. The v1.4.2
  patch **0035** (sever the `cdn.agentclientprotocol.com` boot fetch — make the store
  offline-only) **still applied cleanly via 3-way** and severs the egress in v1.5.4
  (file shrank 675→362 lines; no `REGISTRY_URL`/`fetch_registry_index`/`download_icon`
  remain). The v1.4.2 patch **0036** (full *deletion* of the registry subsystem) was
  **dropped** — a literal replay would break v1.5.4's unified extension-agent support.
  Policy is now **offline-only freeze** (matching the extension-registry freeze in 0016),
  not deletion. `AgentRegistryStore`/`LocalRegistry*Agent` remain but are network-dead.
  The three 0036 symbol tripwires were removed from the gate; the air-gap guard is the
  `cdn.agentclientprotocol.com` egress ban (kept). Old patch numbers ≥0037 shifted down
  by one (the GAP-7 patch is now 0036, the egress-strip patches are 0042–0043).
- **New v1.5.4 brand/egress fixed in-place:** a new `const STATUS_URL = "https://status.zed.dev"`
  + `OpenStatusPage` action (a Zed-hosted status-page link) was stripped, and a new
  `const DOCS_URL = "https://zed.dev/docs/"` in `crates/zed/src/zed.rs` was repointed to
  the Zedium docs URL (both introduced by upstream, unseen by the v1.4.2 brand patches).
- **Edit-prediction cloud engine:** the v1.4.2 strip of the cloud predict path
  (patch 0024) was re-applied across upstream's refactor; only the self-hosted Zeta path
  remains. The v1.5.4-new cloud-Zeta credential/backoff guard now early-returns
  `Ok(None)` (no Cloud backend exists in the fork); the full build confirms no dangling
  cloud symbols.

**GAP-8 runtime air-gap smoke — PASS on v1.5.4 (2026-06-09).** Ran
`tools/airgap-smoke.sh` (= `strace -f -e trace=connect,network`, 30s dwell + manual UI
interaction) against the debug `zedium` built from the v1.5.4 applied tree. All 16
`AF_INET`/`AF_INET6` `connect()` calls were to loopback only (`127.0.0.1`/`::1`):
port-0 socket probes plus the user-opt-in local-LLM probes (Ollama `11434`, LM Studio
`1234`); 16–19 `AF_UNIX` were local IPC. **Zero connects to any external host** — no
`*.zed.dev`, no `cdn.agentclientprotocol.com`. The runtime smoke is now reproducible via
the committed `tools/airgap-smoke.sh`.

The rest of this document is the v1.4.2-era snapshot, retained for history.

---

Status snapshot as of 2026-05-31. This document is the single source of truth for
**what has been done, what diverged from [PLAN.md](PLAN.md), and what remains before
tagging `v1.4.2-1`.** It supersedes the per-patch progress notes for release planning.

Baseline: upstream Zed `v1.4.2`. Applied patches: `patches/0001`–`patches/0025`
(parent commits `db0aa83`, `84cb2b5`, and earlier). Verifier currently **green**
(3497 files, 26 forbidden-string patterns, 18 banned crates).

The repo layout, patch-quilt model, and locked decisions are in [PLAN.md](PLAN.md);
the original cloud-surface inventory is in [DISCOVERY.md](DISCOVERY.md). This file
reconciles those against the code as it actually stands today.

---

## 1. Legend

- **ROOTED OUT** — crate/code deleted outright; reintroduction fails to compile (strip-by-deletion policy).
- **REPLACED** — behaviour reimplemented to a self-hosted / BYO-key / offline equivalent.
- **NEUTERED** — left in place but reduced to a legitimate no-op/Disabled answer (not a leak vector).
- **KEPT** — intentionally retained (shared type crate, BYO provider, or non-cloud feature).
- **GAP** — required for v1 per PLAN, not yet done. Tracked in §6 action list.

---

## 2. What is DONE (patches 0001–0025)

### Telemetry / crash reporting — ROOTED OUT
- `telemetry` crate, `client/src/telemetry.rs` uploader, telemetry log-viewer, `reliability.rs`
  minidump/Sentry upload, `main.rs` telemetry+crashes startup. (Patches 0001, 0004, 0019.)
- `os_name`/`os_version` relocated to `system_specs`.
- Tripwires: `MINIDUMP_ENDPOINT`, `sentry\[`, `crashpad`, `upload_minidump`,
  `telemetry::event!`, `set_authenticated_user_info`, `report_assistant_event`,
  `report_discovered_project_type_events`, `upload_build_timings`.

### Collab / calls / channels / notifications — ROOTED OUT
- `collab`, `collab_ui`, `call`, `channel`, `livekit_api`, `livekit_client`,
  cloud half of `notifications`. (Patches 0002, 0007, 0008.)

### Auto-update — ROOTED OUT
- `auto_update`, `auto_update_ui`, `auto_update_helper`. (Patch 0003.) Users replace the binary.

### Feedback / Copilot — ROOTED OUT
- `feedback` (Patch 0005); `copilot`, `copilot_chat`, `copilot_ui` (Patch 0006).

### Cloud LLM chat providers — REPLACED (effectively self-hosted/BYO only)
- `language_models_cloud` ROOTED OUT (Patch 0012). `web_search_providers` (cloud-only) ROOTED OUT (Patch 0013).
- `language_models` now registers **only** Ollama, LM Studio, and OpenAI-compatible
  (user-configured) providers — see `language_models.rs` `register_language_model_providers`
  / `register_openai_compatible_providers`. The direct cloud provider modules were stripped
  (Patch 0009); `provider/anthropic.rs` is a 1-line stub.
- Tripwires: `CloudLanguageModelProvider`, `OpenAiSubscribedProvider`.
- **Caveat — orphan crates remain:** see §5 / GAP-1.

### Cloud API transport — NEUTERED / KEPT (corrected scope)
- `cloud_api_client` gutted to no-op stub; websocket reintroduction guarded
  (`yawc::WebSocket`, `WebSocket::connect`, `.build_zed_cloud_url(`). (Patch 0014.)
- `sign_in_with_optional_connect` neutered (Patch 0015).
- **KEPT crates** `cloud_llm_client` / `cloud_api_client` / `cloud_api_types` — these are
  shared **type** crates consumed by the retained `agent`, `language_models`,
  `language_model_core`, `extension_host`. Not deletable. `RefreshLlmTokenListener` /
  `LlmApiToken` / `refresh_llm_token` KEPT (used by agent + third-party providers).
  > **Policy note:** patches 0014/0015 used the neuter-to-stub approach. Current policy
  > ([[strip-by-deletion-not-stub-policy]]) prefers deletion. These two are the standing
  > exceptions — revisit if a cleaner deletion boundary appears (GAP-7).

### Sign-in / account / onboarding UI — ROOTED OUT
- `ai_onboarding` crate (Zed Pro popup, trial-end upsell) (Patch 0018).
- Zed sign-in / AI-signup / trial removed from `onboarding` (Patch 0020); kept theme/keymap/
  third-party-agent grid.
- Title-bar Zed-account UI (Sign In, plan dropdown, `plan_chip.rs`) removed (Patch 0021).

### Extension registry — REPLACED (offline-only)
- Registry frozen to offline; network fetch guarded (`.build_zed_api_url(`). (Patch 0016.)
- Tolerant keymap/action loader for stripped action refs (Patch 0017, see [[stripped-action-references-policy]]).

### Edit prediction (Zeta) — REPLACED (self-hosted-only) (Patches 0022–0025)
- **Goal achieved:** edit prediction works **only** with a user-provided self-hosted model
  (Ollama / OpenAI-compatible private server) plus BYO-key Mercury/Codestral. Every
  Zed-account / Zed-hosted-endpoint coupling removed.
- `edit_prediction_cli` ROOTED OUT (Patch 0022).
- `EditPredictionProvider::Zed` settings variant removed; stale `"provider":"zed"`
  auto-migrates to `None` via `fallible_options::deserialize` (Patch 0023).
- Cloud predict engine gutted in `edit_prediction` (Patch 0024): removed `client`/`user_store`/
  `llm_token` fields, V3/raw/accept/settled/reject paths, `/edit_prediction_experiments`,
  usage/quota, data-collection (deleted `capture_example.rs`, `license_detection.rs`,
  `example_spec.rs`, settled/reject workers). Delegate now reports Disabled/None for
  data-collection/usage. Kept only the self-hosted custom-server (Zeta-format) branch in `zeta.rs`.
- Orphaned helpers deleted (Patch 0025): `authenticated_llm_request`, `cached_llm_token`,
  `global_llm_token`, `build_zed_llm_url`.
- Tripwires: `authenticated_llm_request`, `global_llm_token`, `.build_zed_llm_url(`,
  `/predict_edits/{v3,raw,accept,settled,reject}`, `/edit_prediction_experiments`.
- **Coverage loss (accepted):** `edit_prediction_tests.rs` deleted wholesale — it was built on
  a `FakeServer` mimicking `/predict_edits/*`. ~16 non-cloud unit tests went with it,
  recoverable from git pre-`84cb2b5`. (GAP-6 / deferred.)

### Branding — PARTIAL
- Surface-visible "Zed"→"Zedium" strings (Patch 0010) and `zed` Cargo package metadata
  author/description (Patch 0011). **Binary name, bundle ID, APP_NAME, config dir NOT done** — see GAP-2.

---

## 3. Divergences from PLAN.md (intentional, ratified)

| PLAN.md said | What we actually did | Why |
|---|---|---|
| Add new `zedium_edit_predictor` crate; remove upstream `zeta`. | Kept `edit_prediction`+`zeta.rs`, **trimmed in place** to self-hosted-only. | Upstream already had Ollama/OpenAI-compatible paths; a new crate was unnecessary. Smaller blast radius. |
| Delete `cloud_llm_client`/`cloud_api_*` crates. | **Kept** — shared type crates for `agent`/`language_models`. Deleted only EP-exclusive helpers. | Discovered they are not edit-prediction-exclusive; deleting them breaks kept crates. |
| Add new `zedium_provider` crate for chat LLM. | **Not created.** `language_models` already registers only user-configured Ollama/LMStudio/OpenAI-compatible. | The registration trim achieved the same end-state without a new crate. **But the standalone provider crates were not deleted — GAP-1.** |
| Verifier = cargo-deny + forbidden-strings with full domain/brand lists (§12). | forbidden-strings is **code-tripwire only**; no domain or `\bZed\b`/`\bZeta\b` patterns active. | Strip work prioritised runtime severing over brand scrub. **GAP-3.** |

---

## 4. Runtime network surface — current verdict

The shipped `zed` binary's outbound surface after 0001–0025:

| Path | Status |
|---|---|
| Telemetry / crash upload | ROOTED OUT — none. |
| Sign-in / RPC / collab / websockets | ROOTED OUT / NEUTERED — none. |
| Auto-update poll | ROOTED OUT — none. |
| Extension registry fetch | REPLACED — offline only. |
| ACP agent registry fetch | REPLACED — offline only (patch 0035). Was the one live boot-time leak the strace smoke caught (`cdn.agentclientprotocol.com`); now loads agents only from an on-disk cache. |
| Chat LLM | Only user-configured Ollama / LM Studio / OpenAI-compatible endpoints. |
| Edit prediction | Only user-configured Ollama / OpenAI-compatible (+ BYO Mercury/Codestral). |

**Assessment:** the runtime air-gap goal is substantially met. No code path in the shipped
binary contacts a Zed-owned or hardcoded-vendor endpoint without explicit user configuration.
The remaining gaps below are **hygiene, branding, verifier-hardening, and distribution** — not
live network leaks — **except GAP-1** (orphan crates carry hardcoded domains in compiled-but-
unreached code, a latent regression/brand risk).

> **Runtime-verified PASS (2026-05-31).** `strace -f -e trace=connect,network` on a Plasma
> Wayland session (open a file, idle ~33s, graceful quit) shows zero non-loopback `connect()`,
> zero `sendto`/`sendmsg` to a non-loopback address, and zero DNS — only loopback IPC
> (crash-handler `:1234`, an Ollama probe `:11434`). The first run of this smoke caught the ACP
> registry leak now fixed in patch 0035; the re-run after the fix is clean. See GAP-8.

---

## 5. Orphan / latent items found this audit

- **Standalone vendor provider crates still present as workspace members, referenced by 0 other
  Cargo.toml:** `anthropic`, `google_ai`, `mistral`, `deepseek`, `x_ai`, `open_router`, `bedrock`.
  Each hardcodes its vendor domain (`api.anthropic.com`, etc.). Dead weight + brand/regression
  risk. → GAP-1.
- **KEPT vendor crates (legitimately used):** `open_ai` (wire types for OpenAI-compatible),
  `codestral` (BYO edit-prediction), `ollama`, `lmstudio`. `codestral.mistral.ai` is a BYO
  default endpoint — acceptable, but should be allowlisted explicitly, not silently passing.
- **`zed.dev` appears in ~60 files** under `crates/` (doc links, menus, `add_llm_provider_modal`,
  `migrator`, onboarding hints, snap/iss packaging). None are cloud transport; all are
  brand/doc-link leakage. → GAP-3.
- `should_show_upsell_modal` / `ZedPredictUpsell` / `OpenZedPredictOnboarding` remain in
  `edit_prediction` (KVP onboarding, not network) — now unreachable, harmless, tidy later.

---

## 5a. Landed in the release pass (patches 0026–0028 + tooling/docs)

- **GAP-1 DONE** — Patch 0026 deleted 6 orphan vendor crates (`anthropic`, `mistral`, `deepseek`,
  `x_ai`, `open_router`, `bedrock`); added to `banned-crates.txt` (24 banned). Kept `open_ai`,
  `google_ai`, `codestral`, `ollama`, `lmstudio` (genuinely used).
- **GAP-2 DONE** — Patch 0027: `APP_NAME` → `Zedium` (config/data dirs now `zedium`), main binary
  renamed `zed` → `zedium`, bundle IDs `dev.zedium.*`, names `Zedium*`, macOS URL scheme `zedium`.
  Build produces `target/*/zedium`.
- **GAP-3 (partial) DONE** — Patch 0028 trimmed `default.json` `language_models` to the registered
  providers (Ollama / OpenAI-compatible / LM Studio). Verifier gained domain bans for the 4
  fully-removed vendors (`api.mistral.ai`, `api.x.ai`, `api.deepseek.com`, `openrouter.ai`) and an
  allowlist for upstream's own `zed/docs`. **30 patterns / 24 banned crates, green.**
- **GAP-4 DONE** — `.github/workflows/release.yml` (4 targets, archive packaging, checksums,
  GitHub Release on `v*-*` tags).
- **GAP-5 DONE** — `docs/{EDIT_PREDICTION,INSTALL,MAINTAINING,upstream-attribution}.md`.
- **Extension snapshot — RESCOPED.** Patch 0016 froze the registry to *local-install-only*
  (`fetch_extensions_from_api` → empty; remote install/upgrade → no-op). The app reads extensions
  the user has installed locally; there is **no bundled-snapshot reader**. A snapshot *pipeline*
  would require new loader code to consume it — deferred as a real feature, not a v1 blocker. v1
  ships with the offline/local-install posture.

## 6. Action list to release-ready v1

Ordered by blocker severity. Each item should land as a numbered patch (or parent-`tools/` edit)
and keep `cargo build --package zed` + `./tools/verify.sh` green.

### Landed in the brand / packaging pass (patches 0029–0034)

- **GAP-3b DONE — brand sweep + gate.** `zedium://` URL scheme (0029); user-facing brand + doc-link
  sweep repointing zed.dev → fork (`github.com/IamMaxim/zedium`, docs → `iammaxim.github.io/zedium`,
  intentionally 404 until GitHub Pages ships) (0030); asset/settings sweep + dead `default_model` and
  `server_url` off zed.dev (0031); residual fixes (0033); env-var rename (0034). The verifier now
  **enforces** 7 brand tripwires (`Zed Pro`, `Zed AI`, `zed.dev/docs`, `zed.dev/{jobs,account,…}`,
  `twitter.com/zeddotdev`, `dev.zed.Zed`, `dev.zed.Oops`) with a curated allowlist for upstream repo
  docs/infra, back-compat provider ids, tests, and Windows-only shell integration.
- **Packaging DONE (0032).** Stripped snap, flatpak, and the Windows Inno Setup installer
  (Zedium ships GitHub-Actions-built binaries only). Rebranded `.desktop` (scheme/keywords +
  `StartupWMClass=dev.zedium.Zedium`) and the cli installed-app discovery names. Replaced **all** app
  icons with the Zedium "element tile" mark (4 channels). Sources + generator in `tools/icons`; the
  generated binaries ride a `--binary` format-patch (round-trip verified via `just apply`).
- **app_id fixed (0030).** `release_channel::app_id()` `dev.zed.Zed*` → `dev.zedium.Zedium*` — the
  runtime Wayland/X11 WM_CLASS now matches the 0027 bundle ids.

### Done

- **GAP-8 — `strace -f -e trace=connect,network` air-gap smoke: PASS (2026-05-31).** Run on a
  Plasma Wayland session against the built `zedium` binary (open a file, idle ~33s, graceful quit).
  Command: `strace -f -e trace=connect,network -o /tmp/zedium.strace zed/target/debug/zedium <file>`.
  Result: zero non-loopback `connect()`, zero `sendto`/`sendmsg` to a non-loopback address, zero DNS
  — only loopback IPC (crash-handler `:1234`, Ollama probe `:11434`). **The first run was NOT clean:**
  it caught `AgentRegistryStore::init_global` fetching the ACP registry from
  `cdn.agentclientprotocol.com` on boot. That leak is severed in **patch 0035** (registry is now
  offline-only) and the re-run is clean. Re-run this smoke once per upstream bump (MAINTAINING.md).

### Landed after the brand/packaging pass (patches 0036–0037)

- **ACP agent registry — STRIPPED (patch 0036).** The online "Add More Agents" registry discovery UI
  and the offline `AgentRegistryStore` were removed, including the registry-only
  `LocalRegistryArchiveAgent`/`LocalRegistryNpxAgent` — the latter carried a **dormant per-agent
  binary download** (`download_server_binary`) reachable only via a planted registry cache. External
  agents (Claude Code / Codex / Gemini) remain as **local CLI tools** added through `agent_servers`
  Custom settings (see [EXTERNAL_AGENTS.md](EXTERNAL_AGENTS.md)). The `Registry` variant was dropped
  from both `CustomAgentServerSettings` enums; `AllAgentServersSettings` gained a **tolerant per-entry
  deserialize** so a stale `{"type":"registry"}` entry is dropped, not fatal. `download_server_binary`
  is intentionally **kept** — the extension-provided agent path still uses it. New verifier tripwires:
  `AgentRegistryStore`, `LocalRegistryArchiveAgent`, `acp_registry_blog` (**42 patterns total**).

- **GAP-7 — DONE (patch 0037).** Sign-in is fully severed: the boot `authenticate()` path, its two call
  sites, and the `SignIn` action handler were deleted, then the `sign_in_with_optional_connect` no-op
  stub itself (it had no remaining callers). `cloud_api_client` is **not fully deletable** — it stays
  as a **type-only** crate for kept consumers (`client/{client,user,llm_token}.rs`, `language_models`):
  `CloudApiClient` (no-op handle), `ClientApiError`, `LlmApiToken`, and the re-exported
  `cloud_api_types` enums (`Plan`, `Organization*`, `websocket_protocol::MessageToClient`). The
  residual is documented symbol-by-symbol at the top of `cloud_api_client.rs`; no method contacts a
  remote service. This closes GAP-7 (previously "assessed, NOT clean / deferred").

### Verify-gate repair + residual egress strip (patches 0043–0044)

- **The verify gate was silently half-broken.** `tools/verify.sh` trimmed each
  pattern with `echo "$x" | xargs`, which interprets backslashes — turning escaped
  regexes (`\.build_zed_cloud_url\(`, `\.build_zed_api_url\(`, `\.build_zed_llm_url\(`,
  `sentry\[`) into uncompilable ones. `rg` rejected them, the error was swallowed by
  `2>/dev/null`, and those four tripwires matched nothing while verify reported PASS.
  Fixed with a backslash-safe parameter-expansion trim, and an uncompilable pattern is
  now a **hard gate failure** instead of a silent pass.
- **A live cloud egress had survived behind the dead tripwire (patch 0043).**
  `Client::authenticate_as_admin` POSTed to `cloud.zed.dev/internal/users/impersonate`
  (gated on `ZED_IMPERSONATE` + `ZED_ADMIN_API_TOKEN`). Deleted, along with the now-orphaned
  `build_zed_api_url` / `build_zed_cloud_url` / `build_zed_cloud_url_with_query` builders, the
  orphaned `client::SignIn` action (handler gone since 0037), and its dangling docs reference.
  New tripwire `/internal/users/impersonate` pins the exact endpoint (**43 patterns total**).
- **Dead surface swept (patch 0044).** Removed the two callerless `cloud_api_client` no-op
  stubs (`update_system_settings`, `submit_edit_prediction_feedback`) and six dangling
  `onboarding::SignIn` / `onboarding::OpenAccount` keybindings the action removal left behind.
- **Series hygiene.** Subjects normalized to `category: description` (the redundant
  `Patch NNNN:` prefix dropped from 0022–0037), the `Co-Authored-By` trailer made uniform,
  the placeholder author on 0001/0002 corrected, and stale/empty commit bodies fixed. These
  are message-only — the applied tree is byte-identical.

### Non-blocking / deferred
- **Extension snapshot pipeline** — needs a loader that reads a bundled snapshot (local-install-only
  today). Real feature, not a v1 blocker (see §5a).
- **GAP-6 — self-hosted EP test harness: won't-do.** Edit prediction reuses upstream Zed's validated
  predict format; the deleted cloud `FakeServer` suite is not being replaced.
- **Brand residuals (allowlisted, not shipped):** Windows-only `explorer_command_injector` Appx
  (`Zed.exe` / app id), `nix/build.nix` desktop app id, macOS `contents/*/embedded.provisionprofile`
  (Zed signing identity — delete candidate), the dead `show_sign_in` settings toggle, the `--zed`
  cli flag, and the command-palette `zed:` action namespace (renaming risks every keybinding).
- Tidy unreachable `ZedPredictUpsell`/`should_show_upsell_modal` KVP onboarding remnants.

---

## 7. One-line status

**Runtime cloud surface: severed and air-gap-verified; brand surface: rebranded and gated.**
Patches 0001–0044 applied, build + verify green (**43 patterns / 24 banned crates**). All
user-facing Zed branding is Zedium (identity, icons, menus, doc links, scheme, packaging); a
brand/registry gate enforces it. The `strace` air-gap smoke (GAP-8) **passes** after patch 0035
severed the last boot-time leak (ACP registry fetch). Patch 0036 stripped the ACP agent registry
(empty discovery UI + dormant per-agent binary download); patch 0037 **closed GAP-7** (sign-in
deleted, `cloud_api_client` reduced to a documented type-only residual). Patches 0043–0044 **repaired
the verify gate** (a backslash-mangling bug had silently disarmed four cloud tripwires) and deleted
the one **live egress** that bug had hidden — an env-gated admin-impersonation POST to `cloud.zed.dev`.
**No code gates remain before tag** — the next steps are the maintainer's outward actions: create the
GitHub repo and push the `v1.4.2-1` tag. GAP-6 won't-do.
