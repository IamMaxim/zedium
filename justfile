# Zedium — top-level justfile.
#
# Parent repo layout:
#   ./zed/         git submodule pinned to upstream Zed at the baseline tag
#   ./patches/     numbered .patch files (the canonical strip series)
#   ./crates/      our own Rust crates (referenced from the editor via path-dep patches)
#   ./tools/       verifier and friends
#   ./docs/        fork docs
#
# Dev workflow:
#   just init           first time only — submodule init + apply patches
#   just apply          reset zed/ to baseline + git am patches/*.patch
#   just reset          discard applied state, back to clean baseline
#   just export         re-export zed/'s commits to patches/
#   just verify         run static checks on current zed/ tree + parent
#   just build          cargo build (assumes patches applied)
#   just run *ARGS      cargo run -- ARGS
#   just merge-upstream TAG    bump submodule baseline + replay patches

set positional-arguments := true

# The branch in zed/ where patches get applied as commits during development.
applied_branch := "zedium-applied"

# Default: list recipes
default:
    @just --list

# === First-time setup ===

# Initialize submodule + apply patches.
init:
    git submodule update --init --recursive
    @just apply

# === Patch lifecycle ===

# Reset zed/ to the baseline tag and apply patches/**/*.patch as commits on `{{applied_branch}}`.
# Patches live in cosmetic category subfolders; apply order = global NNNN- basename prefix.
apply:
    @ZEDIUM_APPLIED_BRANCH={{applied_branch}} ./tools/patches-apply.sh

# Re-export zed/'s {{applied_branch}} commits back to patches/.
export:
    @./tools/patches-export.sh

# Reset zed/ to baseline (drops applied patches; useful for clean upstream view).
reset:
    #!/usr/bin/env bash
    set -euo pipefail
    # Recorded submodule baseline = whatever the parent's .gitmodules SHA is.
    baseline=$(git ls-tree HEAD zed | awk '{print $3}')
    git -C zed checkout -q "$baseline"
    git -C zed branch -D {{applied_branch}} 2>/dev/null || true
    echo "reset to $(git -C zed describe --tags --always)"

# === Verification ===

# Run static checks (forbidden strings + banned crates) on parent + zed/ trees.
verify:
    @./tools/verify.sh

# === Build & run ===

# Build the editor (debug). Assumes patches are applied.
build *ARGS:
    cd zed && cargo build --package zed {{ARGS}}

# Build release.
build-release *ARGS:
    cd zed && cargo build --release --package zed {{ARGS}}

# Run the editor (debug).
run *ARGS:
    cd zed && cargo run --package zed -- {{ARGS}}

# Run the editor (release).
run-release *ARGS:
    cd zed && cargo run --release --package zed -- {{ARGS}}

# === Test & lint ===

test *ARGS:
    cd zed && cargo test --workspace {{ARGS}}

test-crate crate *ARGS:
    cd zed && cargo test --package {{crate}} {{ARGS}}

check:
    cd zed && cargo check --workspace

clippy *ARGS:
    cd zed && cargo clippy --workspace --all-targets {{ARGS}} -- -D warnings

fmt:
    cd zed && cargo fmt --all

fmt-check:
    cd zed && cargo fmt --all -- --check

# === Upstream tracking ===

# Bump the submodule baseline to TAG, replay patches, run verify.
merge-upstream tag:
    #!/usr/bin/env bash
    set -euo pipefail
    cd zed
    git fetch --tags
    git checkout -q "{{tag}}"
    cd ..
    git add zed
    @just apply
    @just verify
    @echo "bumped submodule to {{tag}}. Review patch conflicts (if any) before committing."

upstream-fetch:
    cd zed && git fetch --tags --prune

# === Remote server binaries ===

# Cross-compile remote_server for bundled SSH/Docker/WSL provisioning.
# No args = the two Linux musl targets. Pass logical targets to override, e.g.
#   just remote-servers macos-aarch64
remote-servers *ARGS:
    @./tools/build-remote-servers.sh {{ARGS}}

# === Maintenance ===

clean:
    cd zed && cargo clean

doctor:
    @rustc --version
    @cargo --version
    @just --version
    @echo "parent branch:    $(git branch --show-current 2>/dev/null || echo detached)"
    @echo "zed submodule:    $(git -C zed describe --tags --always)"
    @echo "applied branch:   $(git -C zed branch --show-current 2>/dev/null || echo none)"
    @echo "patches:          $(ls patches/*.patch 2>/dev/null | wc -l) files"
