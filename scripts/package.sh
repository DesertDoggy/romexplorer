#!/usr/bin/env bash
# Assemble a shippable romexplorer package from an already-built binary and an already-built
# native-library tree, and archive it.
#
# It builds the Rust side itself (`--no-build` to skip) and takes the native libraries as
# given from `app_root_ex/lib/<os>/<arch>/`. Building here is not scope creep: whether the
# result is relocatable is decided by the linker flags, so if packaging did not own them it
# could only discover the problem afterwards. .cargo/config.toml carries a third, absolute
# rpath entry so `cargo run` works in the source tree; this script overrides the rustflags to
# leave it out, and then the gate below proves it did.
#
#   scripts/package.sh --kind server  --os linux --arch x64
#   scripts/package.sh --kind desktop --os linux --arch x64 --no-archive
#   scripts/package.sh --kind server  --os windows --arch arm64
#
#   scripts/package.sh --kind chdlite --os windows --arch x64
#   scripts/package.sh --kind rommusic --os windows --arch x64
#
# Kinds: `server` (the three server binaries + bundled MariaDB + installer), `desktop` (the
# GUI + bundled MariaDB) and `chdlite` (the chdlite tools, with its own version
# and its own tree -- see below) and `rommusic` (the ROM music tool, versioned by
# rommusic-cli/Cargo.toml, in release/rommusic/<version>/). `cli` used to name a server package without MariaDB, which
# was a server under the wrong name.
#
# A different arch on the same OS (windows/arm64 on an x64 PC) is built with
# `cargo build --target <triple>` into target/<triple>/release; a different OS is packaged
# from --bin-dir as given, never built here.
#
# The layout it produces, and why it is that shape:
#
#   release/<version>/<os>/<arch>/<kind>/   <- where a staged package lands
#   release/<version>/dist/                <- every platform's archives for that version
#
#   release/chdlite/<chdlite version>/<os>/<arch>/   <- the chdlite kind, versioned by chdlite/Cargo.toml
#   release/chdlite/<chdlite version>/dist/
#
#   chdlite-<v>-<os>-<arch>/chdlite, chdcomp, chdread, chdhash   <- one console tool per mode,
#                                                since a file dropped on a binary can't pick one
#   chdlite-<v>-<os>-<arch>/chdlite-gui        <- on Windows a single file: chdman and
#                                                dolphinrvz are inside it (bundled-natives)
#   chdlite-<v>-<os>-<arch>/lib/<os>/<arch>/   <- chdman and dolphinrvz only
#
#   <name>/romexplorer_server              <- the executables, at the top level
#   <name>/lib/<os>/<arch>/                <- AppPaths::root_for_exe finds this beside them,
#   <name>/assets/                            and native_lib::exe_root shares the answer
#   <name>/LICENSE
#
# The package *is* the app root. app_root_ex/ in this repository is only an imitation of
# that directory, named because a checkout's top level holds everything else too; its
# contents are what ships, never the directory itself.
#
# Nothing here needs ROMEXPLORER_APP_ROOT or a symlink; the unit files set the variables
# anyway, as documentation.
#
# What it deliberately does NOT ship: docs/, src/, submodules/, migrations/ (compiled into
# the binary by sqlx::migrate!), Cargo.*, or the repository README. Binaries, libraries,
# assets, licences, and -- for the server -- install.sh, romexplorer.conf.example and the
# romexplorer-server-start the service unit runs.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

KIND="" OS="" ARCH="" VERSION="" OUT_DIR="" BIN_DIR="" LIB_ROOT=""
STRIP=1 ARCHIVE=1 DB_TOOLS=0 DEDUPE=1 BUILD=1

die() { echo "error: $*" >&2; exit 1; }
note() { echo "  $*"; }

usage() {
    # Found by where the comments stop, not by a line range: a range goes stale the first
    # time the header is edited, and then quietly truncates the help.
    sed -n '2,/^[^#]/p' "${BASH_SOURCE[0]}" | sed -n 's/^# \{0,1\}//p'
    cat <<'EOF'

Options:
  --kind server|desktop|chdlite|rommusic   which package to build        (required)
  --os linux|macos|windows                                  (default: this host)
  --arch x64|arm64                                          (default: this host)
  --version V             (default: the workspace version from cargo metadata)
  --bin-dir DIR           where the built binaries are      (default: target/release)
  --lib-root DIR          the lib/ tree to take <os>/<arch> from
                          (default: app_root_ex/lib)
  --out DIR               stage here instead of release/<version>/<os>/<arch>/<kind>
  --no-strip              keep debug sections (a ~3x larger package)
  --no-dedupe             keep SONAME duplicates as separate files, not symlinks
  --with-db-tools         also ship the MariaDB command-line client
  --no-archive            stage only, do not tar/zip
  --no-build              use whatever is already in --bin-dir; the relocatability gate
                          will then reject binaries built with the dev rpath
EOF
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --kind) KIND="${2:-}"; shift 2 ;;
        --os) OS="${2:-}"; shift 2 ;;
        --arch) ARCH="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --bin-dir) BIN_DIR="${2:-}"; shift 2 ;;
        --lib-root) LIB_ROOT="${2:-}"; shift 2 ;;
        --out) OUT_DIR="${2:-}"; shift 2 ;;
        --no-strip) STRIP=0; shift ;;
        --no-dedupe) DEDUPE=0; shift ;;
        --with-db-tools) DB_TOOLS=1; shift ;;
        --no-archive) ARCHIVE=0; shift ;;
        --no-build) BUILD=0; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 2 ;;
    esac
done

# `cli` is the old name of the chdlite kind, which wrote to release/cli/.
[[ "$KIND" == "cli" ]] && KIND="chdlite"
case "$KIND" in server|desktop|chdlite|rommusic) ;; *) echo "error: --kind must be server, desktop, chdlite or rommusic" >&2; usage 2 ;; esac
# The crate whose version names the package: chdlite and rommusic are released on their own schedule.
VERSION_PACKAGE="romexplorer"; [[ "$KIND" == "chdlite" ]] && VERSION_PACKAGE="chdlite"; [[ "$KIND" == "rommusic" ]] && VERSION_PACKAGE="rommusic-cli"

# --- host defaults, in the app layout's spelling (native_lib::os_dir_name) ----------------
if [[ -z "$OS" ]]; then
    case "$(uname -s)" in
        Linux) OS="linux" ;;
        Darwin) OS="macos" ;;
        MINGW*|MSYS*|CYGWIN*) OS="windows" ;;
        *) die "can't tell what OS this is; pass --os" ;;
    esac
fi
if [[ -z "$ARCH" ]]; then
    case "$(uname -m)" in
        x86_64|amd64) ARCH="x64" ;;
        arm64|aarch64) ARCH="arm64" ;;
        *) die "can't tell what architecture this is; pass --arch" ;;
    esac
fi
case "$OS" in linux|macos|windows) ;; *) die "--os must be linux, macos or windows (got '$OS')" ;; esac
case "$ARCH" in x64|arm64) ;; *) die "--arch must be x64 or arm64 (got '$ARCH')" ;; esac

CROSS=0
HOST_OS_APP="$(case "$(uname -s)" in Linux) echo linux;; Darwin) echo macos;; *) echo windows;; esac)"
HOST_ARCH_APP="$(case "$(uname -m)" in arm64|aarch64) echo arm64;; *) echo x64;; esac)"
[[ "$OS" == "$HOST_OS_APP" ]] || CROSS=1

# Same OS, other arch: cargo can build that here with --target, so it is not CROSS.
TARGET_TRIPLE=""
if [[ "$CROSS" -eq 0 && "$ARCH" != "$HOST_ARCH_APP" ]]; then
    case "$OS/$ARCH" in
        windows/arm64) TARGET_TRIPLE="aarch64-pc-windows-msvc" ;;
        windows/x64)   TARGET_TRIPLE="x86_64-pc-windows-msvc" ;;
        linux/arm64)   TARGET_TRIPLE="aarch64-unknown-linux-gnu" ;;
        linux/x64)     TARGET_TRIPLE="x86_64-unknown-linux-gnu" ;;
        macos/arm64)   TARGET_TRIPLE="aarch64-apple-darwin" ;;
        macos/x64)     TARGET_TRIPLE="x86_64-apple-darwin" ;;
    esac
fi

# The form rustc sees paths in. Under MSYS2/Git Bash $HOME is /c/Users/..., but rustc (a
# native Windows program) records C:\Users\..., so a remap or a grep on the MSYS form would
# silently never match.
native_path() {
    if [[ "$HOST_OS_APP" == "windows" ]] && command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi
}

if [[ -z "$VERSION" ]]; then
    VERSION="$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT_DIR/Cargo.toml" 2>/dev/null \
        | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]==sys.argv[1]))' "$VERSION_PACKAGE" 2>/dev/null)" \
        || die "couldn't read the workspace version; pass --version"
fi
[[ -n "$VERSION" ]] || die "couldn't read the workspace version; pass --version"

if [[ -n "$TARGET_TRIPLE" ]]; then
    BIN_DIR="${BIN_DIR:-$ROOT_DIR/target/$TARGET_TRIPLE/release}"
else
    BIN_DIR="${BIN_DIR:-$ROOT_DIR/target/release}"
fi
LIB_ROOT="${LIB_ROOT:-$ROOT_DIR/app_root_ex/lib}"
SRC_LIB="$LIB_ROOT/$OS/$ARCH"
[[ -d "$SRC_LIB" ]] || die "no native libraries at $SRC_LIB -- build them with scripts/build_native_deps.sh $OS $ARCH, or point --lib-root at a downloaded bundle"

EXE=""; [[ "$OS" == "windows" ]] && EXE=".exe"
case "$KIND" in
    server)  NAME="romexplorer-server-$VERSION-$OS-$ARCH" ;;
    desktop) NAME="romexplorer-$VERSION-$OS-$ARCH" ;;
    chdlite) NAME="chdlite-$VERSION-$OS-$ARCH" ;;
    rommusic) NAME="rommusic-$VERSION-$OS-$ARCH" ;;
esac
# Version first, then platform, arch, kind -- so everything belonging to one release sits
# under one directory and can be archived, copied or deleted as a unit. The archives for
# every platform share that version's dist/, which is what a release upload actually needs.
STAGE="${OUT_DIR:-$ROOT_DIR/release/$VERSION/$OS/$ARCH/$KIND}"
DIST="$ROOT_DIR/release/$VERSION/dist"
if [[ "$KIND" == "chdlite" || "$KIND" == "rommusic" ]]; then
    tree="chdlite"; [[ "$KIND" == "rommusic" ]] && tree="rommusic"
    STAGE="${OUT_DIR:-$ROOT_DIR/release/$tree/$VERSION/$OS/$ARCH}"
    DIST="$ROOT_DIR/release/$tree/$VERSION/dist"
fi

echo "=== $KIND $VERSION for $OS/$ARCH ==="
note "binaries from $BIN_DIR"
note "libraries from $SRC_LIB"
note "staging into $STAGE"
[[ "$CROSS" -eq 1 ]] && note "note: staging a $OS package on a different host -- strip/symlink steps that need $OS tools are skipped where absent"

rm -rf "$STAGE"
mkdir -p "$STAGE"
# The staged package is the app root: what app_root_ex/ holds in the source tree goes
# straight into it, beside the executables.
APP="$STAGE"

# --- build ---------------------------------------------------------------------------------
# Only the relative rpaths. CARGO_ENCODED_RUSTFLAGS, like RUSTFLAGS, replaces the `[target.<triple>] rustflags` table in
# .cargo/config.toml wholesale, while `[env] OPENSSL_*` still applies -- which is exactly the
# split we want: keep the build pointing at the vendored OpenSSL, drop the dev rpath that
# names this machine.
if [[ "$BUILD" -eq 1 ]]; then
    if [[ "$CROSS" -eq 1 ]]; then
        note "cross target: skipping the build, using $BIN_DIR as given"
    else
        case "$OS" in macos) origin="@loader_path" ;; *) origin="\$ORIGIN" ;; esac
        rel="lib/$OS/$ARCH/openssl/lib"
        # PE has no rpath and MSVC's link.exe rejects -Wl,... outright: on Windows the GUI's
        # OpenSSL DLLs are copied next to the executable instead (see "native libraries").
        if [[ "$OS" == "windows" ]]; then
            RUSTFLAGS_LIST=()
        else
            RUSTFLAGS_LIST=(-C "link-arg=-Wl,-rpath,$origin/$rel")
        fi
        # A second entry on macOS, for the .app bundle scripts/macos_app.sh builds. Inside a
        # bundle the executable is at Contents/MacOS/ and lib/ at Contents/Resources/, so
        # `@loader_path/lib` above names a directory that is not there and the GUI's
        # libssl/libcrypto would not resolve. lib/ has to live in Resources/ rather than
        # beside the binary because AppPaths::root_for_exe probes exactly there
        # (src/database/config.rs), Info.plist's LSEnvironment being ignored by modern macOS.
        # Additive, so the plain macOS package is unaffected, and @loader_path keeps the otool
        # gate below happy.
        [[ "$OS" == "macos" ]] && RUSTFLAGS_LIST+=(-C "link-arg=-Wl,-rpath,$origin/../Resources/$rel")
        # Strip the build machine out of the binaries. rustc bakes source paths into panic
        # messages, debug assertions and every dependency's file!() -- a release build here
        # carried 792 copies of /root/.cargo/registry, which says who built it and where. The
        # gate below warns about exactly this; remapping means it has nothing to warn about.
        # Measured on this repo: 792 occurrences in romexplorer_server before, 0 after, with
        # the worker and dbtool the same and the package byte-for-byte the same size.
        # All three prefixes, because the registry is the bulk and the worktree is the rest.
        # native_path: rustc records C:\... on Windows, and remaps by prefix of that form.
        # Passed as CARGO_ENCODED_RUSTFLAGS (one flag per \x1f-separated field), not RUSTFLAGS,
        # which is split on spaces: a profile like C:\Users\John Smith would break the build.
        # Not $HOME/.cargo outright: in an MSYS2 shell $HOME is /home/<you>, while cargo lives
        # under the Windows profile, so the remap matched nothing and every registry path
        # stayed in the binaries. Where the cargo on PATH sits is the fact.
        cargo_home="${CARGO_HOME:-}"
        if [[ -z "$cargo_home" ]]; then
            cargo_bin="$(command -v cargo 2>/dev/null || true)"
            [[ "$cargo_bin" == */bin/cargo* ]] && cargo_home="${cargo_bin%/bin/cargo*}"
        fi
        cargo_home="$(native_path "${cargo_home:-$HOME/.cargo}")"
        sep="/"; [[ "$OS" == windows ]] && sep='\'
        RUSTFLAGS_LIST+=("--remap-path-prefix=$cargo_home${sep}registry=/cargo-registry")
        RUSTFLAGS_LIST+=("--remap-path-prefix=$cargo_home${sep}git=/cargo-git")
        RUSTFLAGS_LIST+=("--remap-path-prefix=$(native_path "$ROOT_DIR")=/build")
        # And the toolchain: with the rust-src component installed, rustc points std's panic
        # locations at that local copy (C:\Users\<you>\.rustup\toolchains\...) instead of the
        # neutral /rustc/<hash> -- measured on a Windows build, the only paths left after the
        # three above, 380-odd of them per binary.
        sysroot="$(rustc --print sysroot 2>/dev/null || true)"
        [[ -n "$sysroot" ]] && RUSTFLAGS_LIST+=("--remap-path-prefix=$(native_path "$sysroot")=/rust-sysroot")
        encode_rustflags() { local IFS=$'\x1f'; export CARGO_ENCODED_RUSTFLAGS="${RUSTFLAGS_LIST[*]}"; }
        encode_rustflags
        # And the one path a remap cannot reach: env!("CARGO_MANIFEST_DIR"), an environment
        # variable rather than a source location. native_lib::source_root reads this instead,
        # so a packaged binary carries "." where it used to carry this checkout's absolute
        # path -- which was the last thing the build-machine warning below still found.
        export ROMEXPLORER_SOURCE_ROOT="."
        target_args=()
        if [[ -n "$TARGET_TRIPLE" ]]; then
            target_args=(--target "$TARGET_TRIPLE")
            note "other arch: --target $TARGET_TRIPLE (rustup target add $TARGET_TRIPLE if missing)"
            # openssl-sys for the target, not the host: .cargo/config.toml's [env] names the
            # host arch's build, and a variable already set in the environment wins over it.
            export OPENSSL_INCLUDE_DIR="$(native_path "$SRC_LIB/openssl/include")"
            export OPENSSL_LIB_DIR="$(native_path "$SRC_LIB/openssl/lib")"
            if [[ "$OS/$ARCH" == "linux/arm64" ]]; then
                export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER:-aarch64-linux-gnu-gcc}"
                export CC_aarch64_unknown_linux_gnu="${CC_aarch64_unknown_linux_gnu:-aarch64-linux-gnu-gcc}"
                export CXX_aarch64_unknown_linux_gnu="${CXX_aarch64_unknown_linux_gnu:-aarch64-linux-gnu-g++}"
                export AR_aarch64_unknown_linux_gnu="${AR_aarch64_unknown_linux_gnu:-aarch64-linux-gnu-ar}"
                export PKG_CONFIG_ALLOW_CROSS=1
                # ALLOW_CROSS alone lets pkg-config answer with the host's x64 .pc files, and
                # the desktop build (webkit2gtk, gtk, libxdo) then fails at link time. The
                # target-scoped variable leaves host build scripts on the host's own.
                #
                # The arm64 -dev packages cannot be installed beside the x64 ones (apt removes
                # those to make room), so they come from scripts/linux_arm64_sysroot.sh's
                # unpacked copy when there is one. rpath-link is for the libraries webkit
                # itself needs: the linker has to resolve them and looks nowhere in a sysroot
                # it was not told about.
                arm64_sysroot="${LINUX_ARM64_SYSROOT:-$ROOT_DIR/tests/output/sysroot/linux-arm64}"
                if [[ -f "$arm64_sysroot/.complete" ]]; then
                    note "arm64 sysroot: $arm64_sysroot"
                    sl="$arm64_sysroot/usr/lib/aarch64-linux-gnu"
                    export PKG_CONFIG_SYSROOT_DIR_aarch64_unknown_linux_gnu="$arm64_sysroot"
                    export PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu="$sl/pkgconfig:$arm64_sysroot/usr/share/pkgconfig"
                    RUSTFLAGS_LIST+=(-L "native=$sl" -C "link-arg=-Wl,-rpath-link,$sl" -C "link-arg=-Wl,-rpath-link,$arm64_sysroot/lib/aarch64-linux-gnu")
                    encode_rustflags
                else
                    export PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu="${PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu:-/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig}"
                fi
            fi
        fi
        # ring assembles its aarch64 code with clang on windows/arm64 -- MSVC's armasm64 cannot
        # read it -- and cc-rs looks for a bare `clang` on PATH, which a Visual Studio install
        # does not put there: the build stops at ring's build script with `failed to find tool
        # "clang"`. Visual Studio ships one (the "C++ Clang tools" component), in a directory
        # per host arch; appended, so it is found for this and shadows nothing. uname -s rather
        # than -m for the host: an MSYS2 shell on an arm64 machine is an emulated x64 program
        # and says x86_64, and the ARM64 directory holds the binaries that run natively there.
        if [[ "$OS/$ARCH" == "windows/arm64" ]] && ! command -v clang >/dev/null 2>&1; then
            case "$(uname -s)" in *-ARM64*) llvm_host="ARM64" ;; *) llvm_host="x64" ;; esac
            vs_root="${VS_INSTALL_DIR:-C:\\Visual Studio\\18\\Community}"
            vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
            if [[ ! -d "$(cygpath -u "$vs_root")/VC/Tools/Llvm" && -f "$vswhere" ]]; then
                vs_root="$("$vswhere" -latest -products '*' -property installationPath 2>/dev/null | tr -d '\r')"
            fi
            llvm_bin="$(cygpath -u "$vs_root")/VC/Tools/Llvm/$llvm_host/bin"
            [[ -x "$llvm_bin/clang.exe" ]] \
                || die "windows/arm64 needs clang for the ring crate, and there is none on PATH or at $llvm_bin -- install Visual Studio's \"C++ Clang tools for Windows\" component, or put an LLVM bin directory on PATH"
            note "clang for ring: $llvm_bin"
            export PATH="$PATH:$llvm_bin"
        fi
        case "$KIND" in
            server)
                note "cargo build --release --bins ${target_args[*]}"
                (cd "$ROOT_DIR" && cargo build --release --bins "${target_args[@]}") || die "the build failed" ;;
            desktop)
                note "cargo build --release -p romexplorer-gui ${target_args[*]}"
                (cd "$ROOT_DIR" && cargo build --release -p romexplorer-gui "${target_args[@]}") || die "the build failed" ;;
            chdlite)
                # Two builds, because features are per package and not per binary: the console
                # tools without the embedded libraries (they share lib/ beside them), then the
                # GUI. On Windows the GUI carries the libraries itself so it is one file.
                note "cargo build --release -p chdlite --bins ${target_args[*]}"
                (cd "$ROOT_DIR" && cargo build --release -p chdlite --bins "${target_args[@]}") || die "the build failed"
                gui_features="gui"; [[ "$OS" == "windows" ]] && gui_features="gui,bundled-natives"
                note "cargo build --release -p chdlite --features $gui_features --bin chdlite-gui ${target_args[*]}"
                (cd "$ROOT_DIR" && CHDLITE_NATIVE_LIB_DIR="$(native_path "$SRC_LIB")"                     cargo build --release -p chdlite --features "$gui_features" --bin chdlite-gui "${target_args[@]}") || die "the build failed" ;;
            rommusic)
                # The console tool and the window, which share lib/ beside them.
                note "cargo build --release -p rommusic-cli --features gui --bins ${target_args[*]}"
                (cd "$ROOT_DIR" && cargo build --release -p rommusic-cli --features gui --bins "${target_args[@]}") || die "the build failed" ;;
        esac
        unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS OPENSSL_INCLUDE_DIR OPENSSL_LIB_DIR ROMEXPLORER_SOURCE_ROOT
    fi
fi

# --- binaries -----------------------------------------------------------------------------
case "$KIND" in
    server)  BINS=(romexplorer_server romexplorer_worker romexplorer_dbtool) ;;
    desktop)    BINS=(romexplorer-gui) ;;
    chdlite)    BINS=(chdlite chdcomp chdread chdhash chdlite-gui) ;;
    rommusic)   BINS=(rommusic rommusic-gui) ;;
esac
for b in "${BINS[@]}"; do
    src="$BIN_DIR/$b$EXE"
    [[ -f "$src" ]] || die "missing $src -- build it first (cargo build --release$([[ "$KIND" == desktop ]] && echo " -p romexplorer-gui")${TARGET_TRIPLE:+ --target $TARGET_TRIPLE})"
    # The GUI's crate name has a hyphen; the thing a user double-clicks should not.
    dest="$b"; [[ "$b" == "romexplorer-gui" ]] && dest="romexplorer"
    cp -f "$src" "$STAGE/$dest$EXE"
    chmod +x "$STAGE/$dest$EXE"
    note "$dest$EXE"
done

# --- native libraries ---------------------------------------------------------------------
# cp -a, never a hard link or --link-dest: the SONAME duplicates below have to survive as
# real files (or as symlinks this script makes deliberately).
mkdir -p "$APP/lib/$OS/$ARCH"
if [[ "$KIND" == "chdlite" ]]; then
    # Only what chdlite loads: chdman and dolphinrvz, and the optional cemuwua (Wii U,
    # .zar), exiso (XISO), psxpbp (PBP), zlib-ng (CSO) and RAHasher (the RetroAchievements
    # hash), each self-contained apart from the C runtime. No MariaDB or 7-Zip. OpenSSL only
    # for chdlite-gui off Windows, below: its window's toolkit links libssl/libcrypto.
    for f in "$SRC_LIB"/*cemuwua* "$SRC_LIB"/*exiso* "$SRC_LIB"/*psxpbp* "$SRC_LIB"/libz.* "$SRC_LIB"/zlib.dll "$SRC_LIB"/libRAHasher.*; do
        [[ -f "$f" ]] || continue
        cp -a "$f" "$APP/lib/$OS/$ARCH/"; note "lib/$OS/$ARCH/$(basename "$f")"
    done
    have_chdman=0 have_dolphin=0
    for f in "$SRC_LIB"/*chdman* "$SRC_LIB"/*dolphinrvz*; do
        [[ -f "$f" ]] || continue
        cp -a "$f" "$APP/lib/$OS/$ARCH/"; note "lib/$OS/$ARCH/$(basename "$f")"
        case "$(basename "$f")" in *chdman*) have_chdman=1 ;; *dolphinrvz*) have_dolphin=1 ;; esac
    done
    if [[ "$OS" != "windows" && -d "$SRC_LIB/openssl/lib" ]]; then
        mkdir -p "$APP/lib/$OS/$ARCH/openssl/lib"
        for f in "$SRC_LIB"/openssl/lib/libssl* "$SRC_LIB"/openssl/lib/libcrypto*; do
            case "$f" in *.so|*.so.*|*.dylib) cp -a "$f" "$APP/lib/$OS/$ARCH/openssl/lib/" ;; esac
        done
        note "lib/$OS/$ARCH/openssl/lib (for chdlite-gui)"
    fi
    [[ "$have_chdman" -eq 1 && "$have_dolphin" -eq 1 ]] || die "chdman and dolphinrvz are not both in $SRC_LIB -- build them with scripts/build_native_deps.sh --only chdman-simd,dolphin $OS $ARCH, then scripts/move_libs_to_app_root.sh"
    mkdir -p "$APP/lib/LICENSES"
    for l in chdman-simd mame libchdr dolphin cemu ZArchive zstd fmt extract-xiso beetle-psx-libretro zlib-ng RALibretro openssl; do
        [[ -f "$LIB_ROOT/LICENSES/$l.LICENSE" ]] && cp -a "$LIB_ROOT/LICENSES/$l.LICENSE" "$APP/lib/LICENSES/"
    done
    note "lib/LICENSES/"
elif [[ "$KIND" == "rommusic" ]]; then
    # What rommusic reads music with: the chip players (rommusic), the stream decoder
    # (rvgmstream, FFmpeg inside), and the disc and archive readers (chdman, dolphinrvz,
    # cemuwua, exiso, 7-Zip with the zlib-ng it links). No MariaDB, AAC encoder or OpenSSL.
    have_players=0
    for f in "$SRC_LIB"/*rommusic* "$SRC_LIB"/*rvgmstream* "$SRC_LIB"/*chdman* "$SRC_LIB"/*dolphinrvz* "$SRC_LIB"/*cemuwua* "$SRC_LIB"/*exiso* "$SRC_LIB"/7z* "$SRC_LIB"/libz.* "$SRC_LIB"/zlib.dll; do
        [[ -f "$f" ]] || continue
        cp -a "$f" "$APP/lib/$OS/$ARCH/"; note "lib/$OS/$ARCH/$(basename "$f")"
        case "$(basename "$f")" in *rommusic*|*rvgmstream*) have_players=$((have_players + 1)) ;; esac
    done
    [[ "$have_players" -ge 2 ]] || die "rommusic and rvgmstream are not both in $SRC_LIB -- build them with scripts/build_native_deps.sh --only ffmpeg,vgmstream,rommusic $OS $ARCH, then scripts/move_libs_to_app_root.sh"
    # rommusic-gui's window toolkit links libssl/libcrypto off Windows, as chdlite-gui's does.
    if [[ "$OS" != "windows" && -d "$SRC_LIB/openssl/lib" ]]; then
        mkdir -p "$APP/lib/$OS/$ARCH/openssl/lib"
        for f in "$SRC_LIB"/openssl/lib/libssl* "$SRC_LIB"/openssl/lib/libcrypto*; do
            case "$f" in *.so|*.so.*|*.dylib) cp -a "$f" "$APP/lib/$OS/$ARCH/openssl/lib/" ;; esac
        done
        note "lib/$OS/$ARCH/openssl/lib (for rommusic-gui)"
    fi
    mkdir -p "$APP/lib/LICENSES"
    for l in gme gme-kode54 agbplay SSEQPlayer lazyusf2 highly_experimental highly_theoretical mgba beetle-wswan libvgm fmt vgmstream ffmpeg chdman-simd mame libchdr dolphin cemu ZArchive zstd extract-xiso 7-Zip-zstd zlib-ng; do
        [[ -f "$LIB_ROOT/LICENSES/$l.LICENSE" ]] && cp -a "$LIB_ROOT/LICENSES/$l.LICENSE" "$APP/lib/LICENSES/"
    done
    note "lib/LICENSES/"
else
    cp -a "$SRC_LIB/." "$APP/lib/$OS/$ARCH/"
    [[ -d "$LIB_ROOT/LICENSES" ]] && cp -a "$LIB_ROOT/LICENSES" "$APP/lib/" && note "lib/LICENSES/"
fi

DEST_LIB="$APP/lib/$OS/$ARCH"

# The MariaDB client tools are never executed: src/database/server.rs shells out to mariadbd
# and nothing else (bootstrap is reimplemented in Rust). Keep the interactive client only on
# request -- it is genuinely useful for support.
if [[ -d "$DEST_LIB/mariadb/bin" ]]; then
    keep=("mariadbd$EXE")
    [[ "$DB_TOOLS" -eq 1 ]] && keep+=("mariadb$EXE")
    for f in "$DEST_LIB/mariadb/bin"/*; do
        [[ -e "$f" ]] || continue
        base="$(basename "$f")"
        wanted=0
        for k in "${keep[@]}"; do [[ "$base" == "$k" ]] && wanted=1; done
        [[ "$wanted" -eq 1 ]] || rm -f "$f"
    done
    note "mariadb/bin pruned to: ${keep[*]}"
fi

# openssl is a *build* prefix (OPENSSL_INCLUDE_DIR/OPENSSL_LIB_DIR). The server links no
# OpenSSL at all, so it needs none of it; the GUI links libssl/libcrypto and needs the libs
# but never the headers.
if [[ -d "$DEST_LIB/openssl" ]]; then
    case "$KIND" in
        server)
            rm -rf "$DEST_LIB/openssl"
            note "openssl/ dropped (the server links none)" ;;
        desktop)
            if [[ "$OS" == "windows" ]]; then
                # The Windows GUI does its TLS through the OS (SChannel) and imports no
                # libssl/libcrypto at all -- checked with objdump on the built exe -- and
                # openssl/lib there holds only .lib import libraries, which nothing loads.
                rm -rf "$DEST_LIB/openssl"
                note "openssl/ dropped (the Windows GUI imports no OpenSSL)"
            else
                rm -rf "$DEST_LIB/openssl/include"
                note "openssl/include dropped (build-time only)"
            fi ;;
    esac
fi

# --- assets ---------------------------------------------------------------------------------
# The music maps bundle (cart_music --write-maps, then --pack-maps): where each matched game's
# music is in its ROM, numbers only. rom_songs reads <app root>/assets/music_maps.zst; without
# it, cartridge music falls back to the boot search.
ship_music_maps() {
    local maps="$ROOT_DIR/app_root_ex/assets/music_maps.zst"
    if [[ -f "$maps" ]]; then
        mkdir -p "$APP/assets"
        cp -f "$maps" "$APP/assets/music_maps.zst" && note "assets/music_maps.zst"
    else
        note "no app_root_ex/assets/music_maps.zst -- cartridge music uses the boot search only"
    fi
}
# The chdlite tools read nothing from assets/: their images and stylesheet are compiled in.
if [[ "$KIND" == "chdlite" ]]; then
    for doc in chdlite_release_notes.md chdlite_release_notes.ja.md; do
        [[ -f "$ROOT_DIR/docs/$doc" ]] && cp -f "$ROOT_DIR/docs/$doc" "$STAGE/$doc" && note "$doc"
    done
elif [[ "$KIND" == "rommusic" ]]; then
    for doc in README.md README.ja.md; do
        [[ -f "$ROOT_DIR/rommusic-cli/$doc" ]] && cp -f "$ROOT_DIR/rommusic-cli/$doc" "$STAGE/$doc" && note "$doc"
    done
    for doc in rommusic_release_notes.md rommusic_release_notes.ja.md; do
        [[ -f "$ROOT_DIR/docs/$doc" ]] && cp -f "$ROOT_DIR/docs/$doc" "$STAGE/$doc" && note "$doc"
    done
    ship_music_maps
else
mkdir -p "$APP/assets/icons" "$APP/assets/media" "$APP/assets/download/dats" "$APP/assets/cache" "$APP/data"
if [[ -d "$ROOT_DIR/app_root_ex/assets/icons" ]]; then
    cp -a "$ROOT_DIR/app_root_ex/assets/icons/." "$APP/assets/icons/" 2>/dev/null || true
fi
# The GUI's web build is what the server serves at /app/. The desktop renders natively and
# never reads it (only src/server/mod.rs does), so it is server-only weight.
if [[ "$KIND" != "desktop" && -d "$ROOT_DIR/app_root_ex/assets/gui_web" ]]; then
    cp -a "$ROOT_DIR/app_root_ex/assets/gui_web" "$APP/assets/" && note "assets/gui_web/ (served at /app/)"
elif [[ "$KIND" != "desktop" ]]; then
    note "no assets/gui_web/ -- run scripts/build_gui_web.sh if you want /app/ to work"
fi
# The UI's language files. Compiled into the binaries as well, but the file on disk wins, so a
# translation can be fixed in place.
[[ -d "$ROOT_DIR/app_root_ex/assets/i18n" ]] || die "missing app_root_ex/assets/i18n -- the build embeds it, so the checkout is broken"
cp -a "$ROOT_DIR/app_root_ex/assets/i18n" "$APP/assets/" && note "assets/i18n/"
# fcdb.bin and biosdb.bin: FcDb::load / BiosDb::load read <app root>/assets/<name> first and
# fall back to the copy compiled into the binary, so shipping them is what lets a newer
# database be dropped in without a rebuild. 400 KB together.
for db in fcdb.bin biosdb.bin; do
    [[ -f "$ROOT_DIR/app_root_ex/assets/$db" ]] || die "missing app_root_ex/assets/$db -- the build embeds it, so the checkout is broken"
    cp -f "$ROOT_DIR/app_root_ex/assets/$db" "$APP/assets/$db"
    note "assets/$db"
done
ship_music_maps
fi

[[ -f "$ROOT_DIR/LICENSE" ]] && cp -f "$ROOT_DIR/LICENSE" "$STAGE/" || true

# --- the server's installer ------------------------------------------------------------------
# What makes the package self-installing: install.sh finds the package it sits in, so an
# operator who unpacks the tarball runs ./install.sh and needs nothing else. The same script
# is what `curl ... | sh -s -- --release latest` runs, which is why it lives at the top level
# with everything else.
#
# romexplorer-server-start is the unit's ExecStart rather than the server itself: see the
# comment at the top of that file for why first-run setup has to be the service's own job.
if [[ "$KIND" == "server" ]]; then
    # The installer for the platform the package is *for*, not the one building it: install.sh
    # is bash and Windows has no bash, install.ps1 is PowerShell and needs no interpreter on
    # any Windows since 2009.
    if [[ "$OS" == "windows" ]]; then
        installers=(install.ps1)
    else
        installers=(install.sh)
    fi
    for f in "${installers[@]}"; do
        [[ -f "$ROOT_DIR/scripts/$f" ]] || die "missing scripts/$f -- the server package is meant to be self-installing"
        cp -f "$ROOT_DIR/scripts/$f" "$STAGE/$f"
        [[ "$f" == *.sh ]] && chmod +x "$STAGE/$f"
        note "$f"
    done
    [[ -f "$ROOT_DIR/scripts/dist/romexplorer.conf.example" ]] || die "missing scripts/dist/romexplorer.conf.example -- install.sh falls back to it for its defaults"
    cp -f "$ROOT_DIR/scripts/dist/romexplorer.conf.example" "$STAGE/romexplorer.conf.example"
    note "romexplorer.conf.example"

    [[ -f "$ROOT_DIR/scripts/dist/romexplorer-server-start" ]] || die "missing scripts/dist/romexplorer-server-start -- the service unit has nothing to run"
    if [[ "$OS" == "windows" ]]; then
        note "romexplorer-server-start skipped on windows (install.ps1 starts the server directly)"
    else
        cp -f "$ROOT_DIR/scripts/dist/romexplorer-server-start" "$STAGE/romexplorer-server-start"
        chmod +x "$STAGE/romexplorer-server-start"
        note "romexplorer-server-start"
    fi
fi

# --- strip ----------------------------------------------------------------------------------
# mariadbd alone is 330 MB with debug_info and 29.5 MB without. strip touches .symtab /
# .strtab / .debug_*, never .dynsym -- which is the only table dlopen()+dlsym() consults, and
# every native library here is reached that way.
strip_tree() {
    local n=0
    [[ "$OS" == "windows" ]] && { note "no strip on windows (debug info lives in separate .pdb files, which we don't ship)"; return; }
    local stripper=strip stripargs=(--strip-unneeded)
    [[ "$OS" == "macos" ]] && stripargs=(-x)
    # The host's strip cannot touch another architecture's ELF -- and every failure below is
    # swallowed per file, so a linux/arm64 package built on x64 used to ship unstripped:
    # measured 428 MB staged against 146 MB for x64. Use the cross binutils' strip instead.
    if [[ "$OS" == "linux" && -n "$TARGET_TRIPLE" && "$ARCH" == "arm64" ]]; then
        stripper="${LINUX_ARM64_CROSS_PREFIX:-aarch64-linux-gnu-}strip"
    fi
    command -v "$stripper" >/dev/null 2>&1 || { note "no strip on PATH; leaving debug sections in place"; return; }
    while IFS= read -r -d '' f; do
        case "$(head -c4 "$f" | tr -d '\0')" in
            *ELF*) ;;
            *) [[ "$OS" == "macos" ]] || continue ;;
        esac
        "$stripper" "${stripargs[@]}" "$f" 2>/dev/null && n=$((n+1)) || true
    done < <(find "$STAGE" -type f \( -perm -u+x -o -name '*.so*' -o -name '*.dylib' \) -print0)
    note "stripped $n files"
}
[[ "$STRIP" -eq 1 ]] && strip_tree || note "strip skipped (--no-strip)"

# --- SONAME duplicates -> symlinks ------------------------------------------------------------
# move_libs_to_app_root.sh deploys some libraries twice: once under the name the Rust side
# dlopen()s (libz.so) and once under the SONAME a sibling library records in its NEEDED
# (libz.so.1). Both names must resolve; they need not both be 200 KB of file. Never on
# Windows, where a zip cannot carry symlinks without developer mode. Restricted to lib/:
# two byte-identical icons in assets/ are a coincidence, not an aliasing requirement, and
# quietly turning one into a symlink to the other would be a surprise to whoever edits them.
dedupe_tree() {
    [[ "$OS" == "windows" ]] && { note "no symlink dedupe on windows"; return; }
    local saved=0
    while IFS= read -r d; do
        python3 - "$d" <<'PY'
import hashlib, os, sys
d = sys.argv[1]
by_hash = {}
for name in sorted(os.listdir(d)):
    p = os.path.join(d, name)
    if not os.path.isfile(p) or os.path.islink(p):
        continue
    h = hashlib.sha256(open(p, 'rb').read()).hexdigest()
    by_hash.setdefault(h, []).append(name)
for names in by_hash.values():
    if len(names) < 2:
        continue
    # Keep the shortest name as the real file: that is the one the app dlopen()s by path.
    names.sort(key=lambda n: (len(n), n))
    keep, rest = names[0], names[1:]
    for name in rest:
        p = os.path.join(d, name)
        size = os.path.getsize(p)
        os.remove(p)
        os.symlink(keep, p)
        print(f"{name} -> {keep} ({size} bytes)")
PY
    done < <(find "$APP/lib" -type d) | while read -r line; do note "dedupe $line"; done
}
[[ "$DEDUPE" -eq 1 ]] && dedupe_tree || note "dedupe skipped (--no-dedupe)"

# --- gates: fail here rather than shipping something broken ------------------------------------
fail=0

# What src/database/server.rs::check_layout insists on. Getting this wrong means the server
# and the desktop both refuse to start, which is exactly how the Windows tree is broken today.
md="$DEST_LIB/mariadb"
if [[ "$KIND" != "chdlite" && "$KIND" != "rommusic" ]]; then
for required in "bin/mariadbd$EXE" "share/english/errmsg.sys"; do
    [[ -f "$md/$required" ]] || { echo "error: bundled MariaDB is incomplete: missing $required under $md" >&2; fail=1; }
done
[[ -d "$md/share" ]] && [[ -n "$(find "$md/share" -maxdepth 1 -name '*.sql' -print -quit)" ]] || { echo "error: no bootstrap SQL in $md/share -- the first run cannot create a database" >&2; fail=1; }
fi

# The server package is the install medium, so the things that do the installing have to be
# in it and have to be runnable. A tarball that unpacks to something you cannot install is
# the failure this catches.
if [[ "$KIND" == "server" ]]; then
    installer="install.sh"; [[ "$OS" == "windows" ]] && installer="install.ps1"
    for required in "$installer" romexplorer.conf.example; do
        [[ -f "$STAGE/$required" ]] || { echo "error: the server package has no $required -- it would not be installable" >&2; fail=1; }
    done
    if [[ "$OS" == "windows" ]]; then
        # No shebang to honour and no executable bit on NTFS; what matters is that PowerShell
        # can parse it, which is checked on a host that has pwsh rather than here.
        grep -q '\[CmdletBinding()\]' "$STAGE/install.ps1" 2>/dev/null || { echo "error: install.ps1 does not look like the installer (no [CmdletBinding()])" >&2; fail=1; }
    else
        [[ -x "$STAGE/install.sh" ]] || { echo "error: $STAGE/install.sh is not executable" >&2; fail=1; }
        [[ -x "$STAGE/romexplorer-server-start" ]] || { echo "error: romexplorer-server-start is missing or not executable -- the service unit's ExecStart would not run" >&2; fail=1; }
        # Both are shell, and a syntax error in either only shows up at install time or at
        # first boot -- the two worst moments to find out. Checked with the interpreter its
        # own shebang names, so that romexplorer-server-start claiming #!/bin/sh is also a
        # claim that it parses as POSIX sh, and not merely as bash.
        for s in "$STAGE/install.sh" "$STAGE/romexplorer-server-start"; do
            case "$(head -n1 "$s")" in
                *bash*) checker=bash ;;
                *) checker=sh ;;
            esac
            "$checker" -n "$s" || { echo "error: $s does not parse as $checker" >&2; fail=1; }
        done
    fi
fi

# Relocatability. A RUNPATH naming a build machine means the package only runs on that
# machine; a build-machine path anywhere in a binary also leaks where it was built.
if [[ "$OS" == "linux" ]] && command -v readelf >/dev/null 2>&1; then
    while IFS= read -r -d '' f; do
        head -c4 "$f" | grep -q ELF || continue
        rp="$(readelf -d -W "$f" 2>/dev/null | awk '/R(UN)?PATH/ {sub(/.*\[/,""); sub(/\].*/,""); print}')"
        [[ -z "$rp" ]] && continue
        IFS=':' read -ra entries <<< "$rp"
        for e in "${entries[@]}"; do
            case "$e" in
                '$ORIGIN'*) ;;
                *) echo "error: $(basename "$f") has an absolute rpath entry '$e'; it will not run off this machine (drop --no-build, or rebuild with the RUSTFLAGS this script sets)" >&2; fail=1 ;;
            esac
        done
    done < <(find "$STAGE" -type f \( -perm -u+x -o -name '*.so*' \) -print0)
elif [[ "$OS" == "macos" ]] && command -v otool >/dev/null 2>&1; then
    while IFS= read -r -d '' f; do
        file "$f" | grep -q Mach-O || continue
        bad="$(otool -L "$f" 2>/dev/null | tail -n +2 | awk '{print $1}' | grep -vE '^(@loader_path|@rpath|@executable_path|/usr/lib/|/System/)' || true)"
        [[ -z "$bad" ]] || { echo "error: $(basename "$f") loads $bad from an absolute path" >&2; fail=1; }
    done < <(find "$STAGE" -type f -print0)
fi

if [[ "$CROSS" -eq 0 ]]; then
    # The form the binary would actually contain (C:\Users\... on Windows, not /c/Users/...).
    # On Windows that is the profile, not $HOME: an MSYS2 shell's $HOME is its own
    # /home/<you>, which no binary ever contains, so the check could not fire there.
    if [[ "$HOST_OS_APP" == "windows" && -n "${USERPROFILE:-}" ]]; then home_native="$USERPROFILE"; else home_native="$(native_path "$HOME")"; fi
    while IFS= read -r -d '' f; do
        if grep -qaF "$home_native" "$f" 2>/dev/null; then
            echo "warning: $(basename "$f") contains a build-machine path ($home_native...)" >&2
        fi
    done < <(find "$STAGE" -maxdepth 1 -type f -print0)
fi

[[ "$fail" -eq 0 ]] || die "package gates failed; nothing archived"

# --- archive -----------------------------------------------------------------------------------
SIZE="$(du -sh "$STAGE" | cut -f1)"
echo "staged $SIZE in $STAGE"

if [[ "$ARCHIVE" -eq 1 ]]; then
    mkdir -p "$DIST"
    tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
    cp -a "$STAGE" "$tmp/$NAME"
    # zip for Windows and macOS, tar.gz for Linux: each is what that platform opens with a
    # double-click and nothing installed. Deflate, not the zstd this repo otherwise prefers --
    # Explorer and Finder's Archive Utility cannot read a zstd zip member, and an archive the
    # recipient cannot open is not smaller in any way that matters. Measured on the macos/arm64
    # server package (118 MB staged): 46.1 MB as tar.gz, 46.2 MB as zip -- nothing is given up.
    if [[ "$OS" == "windows" || "$OS" == "macos" ]]; then
        out="$DIST/$NAME.zip"
        rm -f "$out"
        if command -v zip >/dev/null 2>&1; then
            # -y: symlinks stay symlinks. The macOS tree depends on it (libssl.dylib ->
            # libssl.4.dylib, and the SONAME dedupe above); Info-ZIP also records the unix
            # mode, so the executable bits survive `unzip` and Archive Utility alike.
            (cd "$tmp" && zip -qry "$out" "$NAME")
        elif [[ "$OS" == "macos" ]]; then
            # The Python fallback below writes neither symlinks nor modes, which is harmless
            # for a Windows tree and would ship a broken macOS one.
            die "no zip(1) on PATH -- needed to archive a macOS package with its symlinks and executable bits intact"
        else
            # zip(1) is not part of Git for Windows or a default MSYS2; Python's zipfile is the
            # fallback, and it is already a requirement of this script (version, dedupe).
            (cd "$tmp" && python3 - "$(native_path "$out")" "$NAME" <<'PY'
import os, sys, zipfile
out, top = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for root, dirs, files in os.walk(top):
        dirs.sort()
        for name in sorted(files):
            z.write(os.path.join(root, name))
PY
            ) || die "archiving failed"
        fi
    else
        out="$DIST/$NAME.tar.gz"
        (cd "$tmp" && tar czf "$out" "$NAME")
    fi
    (cd "$DIST" && sha256sum "$(basename "$out")" > "$(basename "$out").sha256")
    echo "archived $(du -h "$out" | cut -f1) -> ${out#$ROOT_DIR/}"
    echo "         ${out#$ROOT_DIR/}.sha256"
fi
