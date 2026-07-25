# Bundled remote-server binaries

This fork provisions the `remote_server` daemon onto SSH/Docker/WSL hosts from
**pre-compiled binaries bundled with the editor** — no network download.

## Building the artifacts

    just remote-servers                 # Linux x86_64 + aarch64 (musl, via zig)
    just remote-servers macos-aarch64   # on a macOS host

Artifacts land in `zed/target/remote-servers-bundle/` as
`zedium-remote-server-<os>-<arch>.gz` and are copied into the app by the bundle
scripts (`libexec/remote_servers/` on Linux, `Contents/MacOS/remote_servers/`
on macOS). Debug symbols are stripped (mirrors upstream `script/bundle-linux`)
to keep the compressed artifacts small.

> **Build prerequisites:**
> - **Linux musl** cross-builds require `zig` and `cargo-zigbuild` on `PATH`
>   (`cargo install --locked cargo-zigbuild` + a `zig` toolchain).
> - **macOS** builds natively with plain `cargo` on a Mac and needs `cmake` on
>   `PATH` (wasmtime's C API build script; `pip3 install --user cmake` works
>   without admin). Xcode is **not** required — the recipe builds `remote_server`
>   with `--features gpui_platform/runtime_shaders`, which skips the build-time
>   `xcrun metal` shader compilation (that tool ships only with full Xcode, not
>   the Command Line Tools). The remote server is headless, so the runtime
>   shader path is never exercised. GitHub macOS CI runners have Xcode, so this
>   feature is harmless there too.

## How the editor finds them

At connect time the editor looks in `<exe_dir>/remote_servers/` for
`zedium-remote-server-<os>-<arch>.gz` matching the host, where `<exe_dir>` is the
directory of the running editor executable. Override the directory for
development with `ZED_REMOTE_SERVER_BUNDLE_DIR`.

The match is served from `download_server_binary_locally`; the existing
transport pipeline uploads the `.gz` and `gunzip`s + `chmod`s + `mv`s it into
place on the host as `zedium-remote-server-<channel>-<version>`.

> **Upgrading from a pre-rename build:** the on-host binary was previously named
> `zed-remote-server-<channel>-<version>`. The first connect after upgrading
> uploads a fresh `zedium-`-named binary; the stale `zed-`-named ones are no
> longer matched by `cleanup_old_binaries` and can be deleted by hand from the
> remote server directory. If no matching artifact is found, the connection fails with a
message naming the expected file and the `just remote-servers` recipe.

## Testing locally

    ZED_BUILD_REMOTE_SERVER=never \
    ZED_REMOTE_SERVER_BUNDLE_DIR="$PWD/zed/target/remote-servers-bundle" \
    just run

then connect to a host. `ZED_BUILD_REMOTE_SERVER=never` disables the
build-from-source path (otherwise active in debug builds) so the bundled
artifact is exercised instead.
