#!/usr/bin/env bash
# Builds romexplorer-gui for the web and installs it at app_root_ex/assets/gui_web/,
# where `AppPaths::gui_web_dir` looks and the server serves it at /app/ (SPA fallback to
# index.html, token-exempt for the files themselves; the API still needs the token).
#
# Both directories this touches are git-ignored build output, never source. There is one
# build for every platform: dx compiles to wasm32-unknown-unknown, a portable target, so
# the same .wasm runs in every browser and webview on every OS -- nothing here is
# per-OS the way app_root_ex/lib/<os>/ is. Desktop and mobile are different commands
# (`dx build --platform desktop|android|ios`) and do not come through here.
#
# Both directories are wiped before use, because dx fingerprints every emitted file
# (`app-dxh<hash>.css`) and does not clean up the previous hashes. Copying onto a
# non-empty tree would leave stale assets behind that nothing references and that the
# server would still cheerfully serve.
#
# Usage: scripts/build_gui_web.sh [--debug]
#   Releases by default. --debug builds unoptimized, which is much faster but makes dx
#   render its "being rebuilt" overlay when there is no dev server running -- useful for
#   checking that a build completes, not for actually using the GUI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

PROFILE=release
case "${1:-}" in
    --debug) PROFILE=debug ;;
    --release|"") ;;
    *) echo "error: unknown option '$1' (expected --debug)" >&2; exit 2 ;;
esac

if ! command -v dx >/dev/null 2>&1; then
    echo "error: 'dx' is not on PATH -- install it with 'cargo install dioxus-cli'" >&2
    exit 1
fi

# dx strips the release wasm with the toolchain's own rust-objcopy, as its very last step. On
# macOS that binary ships with the `rustc` component but the libLLVM.dylib it loads through
# @loader_path/../lib only arrives with `llvm-tools`, so without the component the build
# compiles for a minute and then dies with "Failed to strip binary". Checked up front instead.
objcopy="$(rustc --print sysroot 2>/dev/null)/lib/rustlib/$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')/bin/rust-objcopy"
if [ "$PROFILE" = release ] && [ -x "$objcopy" ] && ! "$objcopy" --version >/dev/null 2>&1; then
    echo "error: $objcopy does not run (its libLLVM is missing) -- 'rustup component add llvm-tools'" >&2
    exit 1
fi

BUILD_DIR="$ROOT_DIR/target/dx/romexplorer-gui/$PROFILE/web/public"
INSTALL_DIR="$ROOT_DIR/app_root_ex/assets/gui_web"

echo "==> cleaning $BUILD_DIR"
rm -rf "$BUILD_DIR"

# Keep the build machine out of the wasm. rustc records the source path of every panic
# location, and for a dependency that is ~/.cargo/registry/... -- measured, the release module
# carried 203 strings naming this machine's account (179 registry, 24 toolchain). The same
# remaps scripts/package.sh applies to the native binaries; RUSTFLAGS from the environment
# reaches the wasm target because .cargo/config.toml only sets flags for the host triple.
#
# native: rustc is a native Windows program and records C:\... there, so a remap written in
# the shell's /c/... form silently never matches -- the same reason package.sh converts. And
# not $HOME/.cargo outright: in an MSYS2 shell $HOME is /home/<you>, while cargo lives under
# the Windows profile; where the cargo on PATH sits is the fact.
native() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }
sep=/
command -v cygpath >/dev/null 2>&1 && sep='\'
cargo_home="${CARGO_HOME:-}"
if [ -z "$cargo_home" ]; then
    cargo_bin="$(command -v cargo 2>/dev/null || true)"
    case "$cargo_bin" in */bin/cargo*) cargo_home="${cargo_bin%/bin/cargo*}" ;; esac
fi
cargo_home="$(native "${cargo_home:-$HOME/.cargo}")"
remap="--remap-path-prefix=$cargo_home${sep}registry=/cargo-registry --remap-path-prefix=$cargo_home${sep}git=/cargo-git --remap-path-prefix=$(native "$ROOT_DIR")=/build"
sysroot="$(rustc --print sysroot 2>/dev/null || true)"
[ -n "$sysroot" ] && remap="$remap --remap-path-prefix=$(native "$sysroot")=/rust-sysroot"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$remap"
export ROMEXPLORER_SOURCE_ROOT="."

echo "==> dx build --platform web ($PROFILE)"
if [ "$PROFILE" = release ]; then
    (cd "$ROOT_DIR/romexplorer-gui" && dx build --platform web --release)
else
    (cd "$ROOT_DIR/romexplorer-gui" && dx build --platform web)
fi

if [ ! -f "$BUILD_DIR/index.html" ]; then
    echo "error: dx reported success but $BUILD_DIR/index.html is missing" >&2
    exit 1
fi

echo "==> installing into $INSTALL_DIR"
rm -rf "$INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
cp -r "$BUILD_DIR"/. "$INSTALL_DIR"/

echo "done: $(du -sh "$INSTALL_DIR" | cut -f1) in app_root_ex/assets/gui_web (served at /app/)"
