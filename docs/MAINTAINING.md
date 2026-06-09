# Maintaining Zedium — per-release runbook

Zedium is a permanent patch-quilt over an upstream Zed submodule. This is the runbook for cutting
a release and for tracking a new upstream tag. See [PLAN.md](PLAN.md) for the rationale and
[RELEASE_READINESS.md](RELEASE_READINESS.md) for current status / open gaps.

## Repo model (recap)

- Parent repo holds `patches/*.patch` (the source of truth), `tools/`, `docs/`, CI, justfile.
- `zed/` is a submodule pinned to a pristine upstream tag. We never permanently commit to it.
- `just apply` resets `zed/` to the baseline and `git am`s the patch series onto a transient
  `zedium-applied` branch. `just export` re-exports `zed/` commits back to `patches/`.
- The `M zed` submodule-pointer change is **intentionally never staged** in the parent; the
  patches are canonical, not the submodule SHA on the applied branch.

## Cutting a release at the current baseline

```sh
just apply              # zed/ -> zedium-applied with all patches
just verify             # forbidden-strings + banned-crates, must be green
just build              # cargo build --release --package zed
# manual ~10 min smoke launch (see checklist below)
git tag v1.4.2-1        # upstream tag + fork revision
git push origin main --tags
```

Pushing a `v*-*` tag triggers `.github/workflows/release.yml` (Linux x86_64/aarch64 + macOS
arm64/x86_64, ad-hoc signed).

## Tracking a new upstream tag

```sh
cd zed && git fetch --tags && git checkout <new-tag> && cd ..
just apply              # git am the series onto the new baseline; conflicts surface here
#   resolve each conflict in zed/, then: git -C zed am --continue
just export             # bake conflict resolutions into patches/
just verify && just build && just test
# smoke launch
git add patches tools docs   # NOT `git add zed`
git commit -m "track upstream <new-tag>"
git tag v<new-tag>-1 && git push origin main --tags
```

`git rerere` is enabled in the submodule clone by `just init`, so repeated conflict resolutions
replay automatically.

## Post-merge re-verify checklist (the cloud-surface regressions to watch)

Upstream merges most often re-introduce cloud surface in these spots. Re-check each:

1. **`crates/http_client/src/http_client.rs`** — hosted-URL builders. The `build_zed_*_url`
   helpers were deleted (patch 0043, last referrer gone); `.build_zed_*_url(` is now a pure
   re-add tripwire — confirm an upstream merge has not reinstated any hosted-URL helper.
2. **`crates/client/`** — telemetry uploader / sign-in. `set_authenticated_user_info`,
   `report_assistant_event` are tripwired.
3. **`crates/language_models/src/language_models.rs`** — provider registration. Must register only
   Ollama / LM Studio / OpenAI-compatible. `CloudLanguageModelProvider` is tripwired.
4. **`crates/edit_prediction/`** — the self-hosted-only predict path. `/predict_edits/*`,
   `authenticated_llm_request`, `global_llm_token` are tripwired.
5. **`assets/settings/default.json`** `language_models` block — must not regrow hosted-vendor
   provider defaults (patch 0028).
6. **`crates/extension_host/`** — `.build_zed_api_url(` is tripwired; registry stays offline.
7. **`crates/project/src/agent_registry_store.rs`** — the ACP agent registry. `cdn.agentclientprotocol.com`
   and `agentclientprotocol/registry` are tripwired; the store must stay offline (cache-only, no
   `refresh`/`fetch_registry_index`). Patch 0035; it was a live boot-time leak.
8. **`crates/zed/src/main.rs`** boot sequence — no `crashes::init`, no telemetry start.
9. **New vendor provider crates** — if upstream adds one, decide keep-as-BYO vs delete; if delete,
   add to `tools/banned-crates.txt` and the vendor domain to `tools/forbidden-strings.txt`.

A full `strace -f -e trace=network` smoke launch (should show zero unexpected connects on an
otherwise-idle editor) is the strongest gate; run it at least once per upstream bump. This is
now scripted as **`tools/airgap-smoke.sh [binary] [dwell-seconds]`** — it straces the launch,
time-boxes it, and reports any non-loopback `connect()`. (Note: it must run in a real desktop
session with a long-lived foreground process; some CI/agent sandboxes kill the timed launch, in
which case analyse the emitted `/tmp/zedium-airgap.*.strace` directly.)

Gate every rebase on a full **`cargo build -p zed`**, not just `cargo check` — the v1.5.4 bump
had a tree that passed `cargo check` but failed `cargo build` (dangling refs the check missed).

## Verifier maintenance

- `tools/banned-crates.txt` — one crate dir per line; fails if `zed/crates/<name>/` exists.
- `tools/forbidden-strings.txt` — one `rg -e` regex per line. Patterns are real regexes
  (escape literal `.` / `(`); verify.sh **hard-fails** on a pattern that does not compile, so a
  malformed tripwire can no longer silently pass (this bug had disarmed four cloud tripwires).
- `tools/forbidden-strings.allowlist` — path prefixes exempt from the string scan (upstream docs,
  licenses, our patch files, our tool configs).

When a strip patch lands, add the matching tripwire so a future re-introduction fails CI.

## App icons

The Zedium icons are generated from SVG sources in `tools/icons` (see its README). They are carried
in `patches/0032-*.patch` as a `--binary` format-patch, so a normal `just apply` restores them; only
re-run `bash tools/icons/make_assets.sh tools/icons zed/crates/zed/resources` if you edit the SVGs.
The exporter (`tools/patches-export.sh`) uses `git format-patch --binary` — keep that flag or binary
assets silently drop from the quilt.

## Known follow-ups (see RELEASE_READINESS.md §6)

- **GAP-8 — air-gap smoke: PASS (2026-05-31; re-confirmed on v1.5.4, 2026-06-09), now a
  per-bump recurring check.** Run `tools/airgap-smoke.sh` (= `strace -f -e trace=connect,network`)
  against the built binary on a desktop session (open a file, idle ~30s + click around, quit);
  confirm no non-loopback `connect()`/`sendto` and no DNS. The initial run caught the ACP registry
  boot fetch (fixed in patch 0035); the v1.5.4 re-run was clean (only loopback Ollama/LM-Studio
  probes). Re-run after every upstream bump and record the result in RELEASE_READINESS §4.
- **GAP-7 — CLOSED.** Sign-in was deleted (patch 0037), and the last live cloud egress
  (admin-impersonation) plus the orphaned stubs/builders in patches 0043–0044. `cloud_api_client`
  is intentionally retained as a documented **type-only** crate for kept consumers, not a deletion
  target — see RELEASE_READINESS.md.
- Brand residuals, all allowlisted and not in the shipped linux/macOS binary: Windows-only
  `explorer_command_injector` Appx, `nix/build.nix` desktop app id, the macOS
  `contents/*/embedded.provisionprofile` signing identity (delete candidate), the dead
  `show_sign_in` settings toggle, the `--zed` cli flag, and the command-palette `zed:` action
  namespace (renaming it would require regenerating every keymap binding).
- Extension snapshot pipeline (needs a bundled-snapshot loader; registry is local-install-only).
