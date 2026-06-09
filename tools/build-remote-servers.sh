#!/usr/bin/env bash
# Build pre-compiled remote_server binaries for bundling with the editor.
#
# Usage:
#   tools/build-remote-servers.sh [TARGET...]
#
# TARGET is one of the logical names below (default: the two Linux musl targets):
#   linux-x86_64   -> x86_64-unknown-linux-musl   (cargo-zigbuild)
#   linux-aarch64  -> aarch64-unknown-linux-musl  (cargo-zigbuild)
#   macos-aarch64  -> aarch64-apple-darwin        (native cargo; macOS host only)
#
# Output: <OUT_DIR>/zed-remote-server-<os>-<arch>.gz
# OUT_DIR defaults to zed/target/remote-servers-bundle (override via OUT_DIR env).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
zed_dir="${here}/zed"
out_dir="${OUT_DIR:-${zed_dir}/target/remote-servers-bundle}"
mkdir -p "${out_dir}"

targets=("$@")
if [ "${#targets[@]}" -eq 0 ]; then
    targets=(linux-x86_64 linux-aarch64)
fi

triple_for() {
    case "$1" in
        linux-x86_64)  echo "x86_64-unknown-linux-musl" ;;
        linux-aarch64) echo "aarch64-unknown-linux-musl" ;;
        macos-aarch64) echo "aarch64-apple-darwin" ;;
        *) echo "unknown target: $1" >&2; return 1 ;;
    esac
}

build_one() {
    local logical="$1"
    local triple
    triple="$(triple_for "$logical")" || exit 1
    local artifact="${out_dir}/zed-remote-server-${logical}.gz"

    echo ">>> building remote_server for ${logical} (${triple})"
    # Add the target to the toolchain cargo will actually use. zed/ pins a
    # toolchain via rust-toolchain.toml, so run rustup inside zed_dir (otherwise
    # the target lands on the default toolchain and the build can't find std).
    ( cd "${zed_dir}" && rustup target add "${triple}" )

    if [[ "${triple}" == *-apple-darwin ]]; then
        # Native macOS build (cross-darwin from Linux is out of scope).
        #
        # remote_server transitively depends on gpui_platform -> gpui_macos,
        # whose build script compiles the Metal shaders with `xcrun metal`.
        # That tool ships only with full Xcode, not the Command Line Tools.
        # The `runtime_shaders` feature swaps build-time `xcrun metal` for
        # runtime shader stitching, so the build needs only the CLT. The
        # remote server is headless and never initializes the Metal renderer,
        # so the runtime path is never actually exercised — this is purely a
        # build-time concern and keeps the recipe Xcode-independent (incl. CI).
        ( cd "${zed_dir}" && cargo build --release --package remote_server \
            --target "${triple}" --features gpui_platform/runtime_shaders )
    else
        # Static musl cross-build via zig.
        if ! command -v cargo-zigbuild >/dev/null 2>&1; then
            echo "cargo-zigbuild not found; install with: cargo install --locked cargo-zigbuild" >&2
            exit 1
        fi
        ( cd "${zed_dir}" && \
          RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
          cargo zigbuild --release --package remote_server --target "${triple}" )
    fi

    local bin="${zed_dir}/target/${triple}/release/remote_server"
    [ -f "${bin}" ] || { echo "expected binary not found: ${bin}" >&2; exit 1; }

    # Strip debug symbols to shrink the bundled artifact (mirrors upstream
    # script/bundle-linux). Cross-arch builds need an arch-agnostic stripper:
    # GNU strip rejects a foreign-arch ELF (e.g. aarch64 from an x86_64 host).
    # Prefer zig's LLVM objcopy (already a build dependency), then llvm-objcopy,
    # then GNU strip. Stripping is a size optimization, so never abort on failure.
    # (macOS is built natively on a Mac and stripped/signed there.)
    if [[ "${triple}" != *-apple-darwin ]]; then
        if command -v zig >/dev/null 2>&1; then
            zig objcopy --strip-debug "${bin}" "${bin}.stripped" \
                && mv "${bin}.stripped" "${bin}" \
                || { rm -f "${bin}.stripped"; echo "note: zig objcopy failed; shipping unstripped ${logical}" >&2; }
        elif command -v llvm-objcopy >/dev/null 2>&1; then
            llvm-objcopy --strip-debug "${bin}" || echo "note: llvm-objcopy failed; shipping unstripped ${logical}" >&2
        elif command -v strip >/dev/null 2>&1; then
            strip --strip-debug "${bin}" || echo "note: strip failed (cross-arch?); shipping unstripped ${logical}" >&2
        else
            echo "note: no objcopy/strip available; shipping unstripped ${logical}" >&2
        fi
    fi

    # Guard: musl builds must not pull in OpenSSL. readelf works cross-arch.
    if [[ "${triple}" == *-musl ]]; then
        if readelf -d "${bin}" 2>/dev/null | grep -qiE 'libssl|libcrypto'; then
            echo "ERROR: ${logical} remote_server links libssl/libcrypto" >&2
            exit 1
        fi
    fi

    gzip -f --best --stdout "${bin}" > "${artifact}"
    echo ">>> wrote ${artifact}"
}

for t in "${targets[@]}"; do
    build_one "$t"
done

echo "remote_server artifacts in ${out_dir}:"
ls -la "${out_dir}"
