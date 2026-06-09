# Upstream Attribution & Non-Affiliation

Zedium is an independent, unofficial fork of **Zed**, the code editor developed by
**Zed Industries, Inc.** (<https://github.com/zed-industries/zed>).

## Not affiliated

Zedium is **not** affiliated with, endorsed by, or supported by Zed Industries. "Zed" and "Zeta"
are names used by Zed Industries for their products; they are referenced here only to describe
this fork's origin and the open-weights model it can run. Do not contact Zed Industries for
support with Zedium.

## What Zedium is

A telemetry-free, cloud-free derivative of Zed `v1.5.4`. It removes:

- All telemetry and crash/minidump upload.
- Sign-in, accounts, billing, and the Zed-hosted collaboration backend.
- The auto-updater.
- Zed-hosted LLM proxying and Zed-hosted edit prediction.
- Hosted-vendor LLM provider integrations (replaced by user-configured, self-hosted/BYO providers).

It keeps the editor, the agent/assistant UI, third-party agent integrations, and edit prediction —
all routed only to endpoints the user explicitly configures.

## License

Zedium inherits Zed's licensing. The bulk of the tree is **GPL-3.0-or-later / AGPL-3.0** (see the
license files in the `zed/` submodule: `LICENSE-GPL`, `LICENSE-AGPL`, `LICENSE-APACHE`, and
`NOTICE`). All fork modifications remain under the same terms and are published openly as the
`patches/` series in this repository.

### AGPL § 13 (network use)

The bundled extension registry is a **static, read-only snapshot** shipped inside the release
artifact. It is not an interactive network service, so the AGPL § 13 source-disclosure obligation
is not triggered by it. All source (upstream + the `patches/` series) is public regardless.

## How modifications are published

Every change Zedium makes to the Zed tree is a reviewable text patch under `patches/`. The
upstream tree itself is an unmodified git submodule pinned to a release tag. This makes the exact
delta between Zedium and upstream Zed auditable at all times.
