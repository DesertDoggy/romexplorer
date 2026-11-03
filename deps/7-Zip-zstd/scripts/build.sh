#!/usr/bin/env bash
set -euo pipefail

# Builds sevenzip.dll / libsevenzip.so / libsevenzip.dylib: a zip+7z-only archive
# library (see deps/7-Zip-zstd/sevenzip_lib.h) using deps/7-Zip-zstd/Makefile.sevenzip_lib.
# The 7-Zip sources are the upstream submodule external/7-Zip-zstd (override with
# SEVENZIP_SOURCE_DIR); deps/7-Zip-zstd/patches/*.patch are applied to it first. This
# wrapper avoids touching core makefiles -- see Makefile.sevenzip_lib's own header comment for
# why it is invoked from inside CPP/7zip/Bundles/Format7zF/ (an existing, untouched
# directory used purely as a stable relative-path anchor for 7zip_gcc.mak's rules).
#
# Usage:
#   deps/7-Zip-zstd/scripts/build.sh                 # auto-detect platform/arch
#   deps/7-Zip-zstd/scripts/build.sh <platform> <arch>
#   deps/7-Zip-zstd/scripts/build.sh --nostreaming [<platform> <arch>]
#
# Builds the streaming-capable variant by default (-DSEVENZIP_WITH_STREAMING, which
# compiles in sevenzip_extract_entry_stream -- see sevenzip_lib.h) into
# deps/7-Zip-zstd/release/with_streaming/<platform>/<arch>/<version>/. --nostreaming builds
# the plain variant into deps/7-Zip-zstd/release/<platform>/<arch>/<version>/ instead (--streaming is still
# accepted, as a no-op). Both variants can coexist: the default
# build's sources, flags, export list and output path are untouched by this flag, so
# the two can be built from the same commit and benchmarked against each other.

streaming=1
args=()
for arg in "$@"; do
  case "$arg" in
    --streaming) streaming=1 ;;
    --nostreaming) streaming=0 ;;
    *) args+=("$arg") ;;
  esac
done
set -- ${args[@]+"${args[@]}"}

if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "[ERROR] Usage: $0 [--nostreaming]  OR  $0 [--nostreaming] <platform> <arch>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
# The upstream 7-Zip-zstd tree; this script, its shim and its build output live in user_dir.
repo_root="${SEVENZIP_SOURCE_DIR:-$main_root/external/7-Zip-zstd}"
repo_root="$(cd "$repo_root" && pwd)"
make_dir="$repo_root/CPP/7zip/Bundles/Format7zF"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-sevenzip-$(date +%Y%m%d-%H%M%S).log"

if [[ $# -eq 0 ]]; then
  os_name="$(uname -s)"
  cpu_name="$(uname -m)"

  case "$os_name" in
    Darwin) platform="mac" ;;
    Linux) platform="linux" ;;
    MINGW*|MSYS*|CYGWIN*) platform="windows" ;;
    *) echo "[ERROR] Unsupported OS for auto-detect: $os_name" >&2; exit 2 ;;
  esac

  case "$cpu_name" in
    arm64|aarch64) arch="arm64" ;;
    x86_64|amd64) arch="x64" ;;
    i386|i686|x86) arch="x86" ;;
    *) echo "[ERROR] Unsupported arch for auto-detect: $cpu_name" >&2; exit 2 ;;
  esac
else
  platform="$1"
  arch="$2"
fi

output_version="$(git -C "$repo_root" describe --tags 2>/dev/null || date +%Y%m%d-%H%M%S)"

# Our changes to 7-Zip's own sources (the streaming extract callbacks the hashing path uses),
# kept as patches so the submodule can stay on an upstream commit. Applied straight to the
# checkout, idempotently -- the same scheme as deps/mariadb-server's build script. They show
# as modified in `git status` inside external/7-Zip-zstd afterwards; that is expected.
for p in "$user_dir"/patches/*.patch; do
  [[ -e "$p" ]] || continue
  if (cd "$repo_root" && patch -p1 -R -f --dry-run <"$p") >/dev/null 2>&1; then
    echo "[INFO] patch already applied: $(basename "$p")" | tee -a "$log_file"
  elif (cd "$repo_root" && patch -p1 -N -f --dry-run <"$p") >/dev/null 2>&1; then
    (cd "$repo_root" && patch -p1 -N -f <"$p") | tee -a "$log_file"
    echo "[INFO] applied patch: $(basename "$p")" | tee -a "$log_file"
  else
    echo "[ERROR] patch $(basename "$p") does not apply to $repo_root (forward or reverse) -- upstream moved; refresh the patch" | tee -a "$log_file" >&2
    exit 1
  fi
done

# Disposable scratch build dir -- see build.sh's own history for why: object paths get
# repeated once per linked object, and a long path here can blow past Windows' ~32K
# command-line limit. Always removed on exit, success or failure.
build_dir="$(mktemp -d /tmp/sevenzip_build.XXXXXX)"
trap 'rm -rf "$build_dir"' EXIT

if (( streaming )); then
  out_dir="$user_dir/release/with_streaming/$platform/$arch/$output_version"
else
  out_dir="$user_dir/release/$platform/$arch/$output_version"
fi
mkdir -p "$build_dir" "$out_dir"

arch_flags=()
case "$arch" in
  arm64)
    # USE_ASM intentionally omitted, not set to 0: GNU Make's `ifdef` only checks
    # whether a variable is defined, so USE_ASM=0 would still count as defined and
    # re-trigger the ASM rules in 7zip_gcc.mak/LzmaDec_gcc.mak.
    arch_flags=(IS_ARM64=1)
    ;;
  x64|x86_64)
    if [[ "$platform" == "windows" ]]; then
      # These specific object files (Sha1Opt/Sha256Opt/Sha512Opt/7zCrcOpt/XzCrc64Opt) are
      # MASM-family hand-tuned x86_64 asm, gated behind USE_ASM in 7zip_gcc.mak -- same
      # asm dependency build.sh already solved. uasm before asmc: verified directly (see
      # build.sh's own history) that asmc fails to assemble Sha1Opt.asm's SHA1RNDS4
      # macro while uasm handles the same file cleanly.
      asm_tool=""
      for candidate in uasm uasm64 UASM UASM64 jwasm jwasm64 asmc asmc64 ml64; do
        if command -v "$candidate" >/dev/null 2>&1; then
          asm_tool="$candidate"
          break
        fi
      done
      if [[ -z "$asm_tool" ]]; then
        for fixed_path in /c/uasm/uasm64.exe /c/uasm/uasm.exe \
          "/c/Program Files/uasm/uasm64.exe" "/c/Program Files/uasm/uasm.exe" \
          /c/jwasm/jwasm.exe \
          /c/asmc/asmc64.exe /c/asmc/asmc.exe; do
          if [[ -f "$fixed_path" ]]; then
            asm_tool="$fixed_path"
            break
          fi
        done
      fi
      if [[ -n "$asm_tool" ]]; then
        arch_flags=(IS_X64=1 USE_ASM=1 "MY_ASM=$asm_tool" "AFLAGS_ABI=-win64 -c")
      else
        echo "[WARN] No MASM-compatible assembler (uasm/asmc/jwasm/ml64) found in PATH or under C:\\uasm/C:\\asmc." >&2
        echo "[WARN] Building without hand-tuned x86_64 ASM; functionally identical, just C fallback for those routines." >&2
        arch_flags=(IS_X64=1)
      fi
    elif [[ "$platform" == "linux" ]]; then
      # Linux only needs LzmaDecOpt.asm -- hashing (Sha1/Sha256/CrcOpt) goes through
      # Rust crates, not this library, and AesOpt.asm isn't needed either. LzmaDecOpt
      # is gated independently via USE_LZMA_DEC_ASM (decoupled from USE_ASM, which
      # would also pull in Sha1Opt/Sha256Opt/AesOpt/CrcOpt). jwasm verified clean on
      # LzmaDecOpt.asm; asmc has a confirmed evaluator bug on Sha1Opt.asm's SHA1RNDS4
      # macro (irrelevant here since USE_ASM stays unset), and jwasm separately lacks
      # the AES-NI/AVX2 opcodes AesOpt.asm needs (also irrelevant here).
      asm_tool=""
      for candidate in jwasm jwasm64 uasm uasm64; do
        if command -v "$candidate" >/dev/null 2>&1; then
          asm_tool="$candidate"
          break
        fi
      done
      if [[ -n "$asm_tool" ]]; then
        arch_flags=(IS_X64=1 USE_LZMA_DEC_ASM=1 "MY_ASM=$asm_tool" "AFLAGS_ABI=-elf64 -DABI_LINUX -c")
      else
        echo "[WARN] No MASM-compatible assembler (jwasm/uasm) found in PATH." >&2
        echo "[WARN] Building without hand-tuned LzmaDecOpt ASM; functionally identical, just C fallback." >&2
        arch_flags=(IS_X64=1)
      fi
    else
      echo "[INFO] ASM not supported for mac in this makefile (no Mach-O AFLAGS_ABI branch); using C fallback." >&2
      arch_flags=(IS_X64=1)
    fi
    ;;
  x86|i386)
    arch_flags=(IS_X86=1)
    ;;
  *)
    echo "[ERROR] unsupported arch: $arch" >&2
    exit 2
    ;;
esac

o_value="$build_dir"
if [[ "$platform" == "windows" ]] && command -v cygpath >/dev/null 2>&1; then
  o_value="$(cygpath -m "$build_dir")"
fi

user_value="$user_dir"
if [[ "$platform" == "windows" ]] && command -v cygpath >/dev/null 2>&1; then
  user_value="$(cygpath -m "$user_dir")"
fi

make_vars=("O=$o_value" "USER_DIR=$user_value")
if (( streaming )); then
  # CXXFLAGS_EXTRA is an unassigned hook in 7zip_gcc.mak, so a command-line assignment
  # is the whole change; the streaming .def adds the one new export (listing it in the
  # default sevenzip_lib.def would break that build's link, since the symbol isn't
  # compiled there).
  make_vars+=("CXXFLAGS_EXTRA=-DSEVENZIP_WITH_STREAMING")
  make_vars+=("DEF_FILE=$user_value/sevenzip_lib_streaming.def")
fi
if [[ "$platform" == "windows" ]]; then
  # See build.sh's own history: needed because C/fast-lzma2/util.c's Windows
  # CPU-count detection casts GetProcAddress()'s FARPROC to a specific function
  # pointer type -- the standard, unavoidable idiom for calling a dynamically
  # resolved WinAPI function, which newer GCC's -Wcast-function-type (implied by
  # -Wextra) flags as an error under -Werror.
  warn="-Werror -Wall -Wextra -Wno-cast-function-type"
  # windows/arm64 is llvm-mingw clang on a path upstream never builds with -Werror, and it
  # trips upstream code rather than ours: C/Threads.h's x86-only force_align_arg_pointer
  # applied to any `_WIN32 && __GNUC__`, and C/Sha512.c's aarch64 SIGILL probe (an unused
  # parameter, printf without <stdio.h>). Warnings stay visible; they just are not fatal on
  # this one target, instead of patching upstream sources one warning at a time.
  [[ "$arch" == "arm64" ]] && warn="-Wall -Wextra -Wno-cast-function-type -Wno-unknown-attributes"
  make_vars+=("CFLAGS_WARN_WALL=$warn")
fi

# zlib-ng, for zlibng_deflate.cpp -- the Deflate decoder this library registers in place
# of 7-Zip's own (see that file for the measurements). Taken from deps/zlib-ng/release,
# newest version by mtime, matching how the consuming app locates
# every other vendored library. The *dynamic* library is what gets linked (see ZLIBNG_LIB
# below), so the produced 7z.so needs zlib-ng's shared library deployed beside it -- the
# static archive is not built -fPIC and cannot go into a shared object at all.
zlibng_root="$main_root/deps/zlib-ng/release/$platform/$arch"
zlibng_version_dir=""
if [[ -d "$zlibng_root" ]]; then
  # `|| true`: ls exits non-zero when one of the two globs matches nothing (zlib-ng has no
  # v-prefixed versions), and under `set -e` that status propagates out of the assignment and
  # kills the script before it can report anything.
  zlibng_version_dir="$(ls -1dt "$zlibng_root"/[0-9]*/ "$zlibng_root"/v[0-9]*/ 2>/dev/null | head -1 || true)"
  zlibng_version_dir="${zlibng_version_dir%/}"
fi
case "$platform" in
  windows) zlibng_shared="$zlibng_version_dir/dynamic/zlib.dll" ;;
  mac)     zlibng_shared="$zlibng_version_dir/dynamic/libz.dylib" ;;
  *)       zlibng_shared="$zlibng_version_dir/dynamic/libz.so" ;;
esac
if [[ -z "$zlibng_version_dir" || ! -f "$zlibng_shared" || ! -f "$zlibng_version_dir/include/zlib.h" ]]; then
  echo "[ERROR] zlib-ng not found under $zlibng_root (need <version>/include/zlib.h and the dynamic library)." >&2
  echo "[ERROR] Build it first: deps/zlib-ng/scripts/build-zlib-ng-release.sh --platform $platform" >&2
  echo "[ERROR] (or scripts/build_native_deps.sh, which builds zlib-ng before this.)" >&2
  exit 1
fi
# -rpath $ORIGIN: the produced 7z.so is dlopen()ed from the app's library directory, and the
# zlib-ng shared library is deployed into that same directory, so resolve it relative to 7z.so
# rather than falling through to the system zlib.
#
# Quoting, in three layers, because getting any one wrong fails silently with an *empty*
# RUNPATH rather than an error: `\$\$` makes bash emit `$$`, make turns that into a single `$`,
# and the surrounding single quotes stop make's recipe shell from expanding `$ORIGIN` to
# nothing.
if [[ "$platform" == "windows" ]]; then
  # Windows links 7z.dll with -static, and `-lz` under -static is resolved by a *search*: on
  # an MSYS2 host that search finds MSYS2's own stock /mingw64/lib/libz.a first, so the
  # windows/x64 7z.dll used to link plain zlib rather than zlib-ng -- silently, with no
  # zlib import to give it away. (llvm-mingw, which windows/arm64 uses, has no libz to
  # fall back on and failed with "unable to find library -lz", which is how this came to
  # light.) Naming zlib-ng's static archive by path links exactly that, into 7z.dll itself;
  # PE has no rpath, so there is nothing else to say.
  [[ -f "$zlibng_version_dir/static/libz.a" ]] || { echo "[ERROR] no $zlibng_version_dir/static/libz.a -- rebuild zlib-ng" >&2; exit 1; }
  make_vars+=("ZLIBNG_INC=$zlibng_version_dir/include" "ZLIBNG_LIB=$zlibng_version_dir/static/libz.a")
else
  make_vars+=("ZLIBNG_INC=$zlibng_version_dir/include" "ZLIBNG_LIB=-L$zlibng_version_dir/dynamic -lz -Wl,-rpath,'\$\$ORIGIN'")
fi

{
  echo "[INFO] platform=$platform arch=$arch output_version=$output_version streaming=$streaming"
  echo "[INFO] zlib-ng=$zlibng_version_dir"
  echo "[INFO] make_dir=$make_dir"
  echo "[INFO] build_dir=$build_dir"
  echo "[INFO] out_dir=$out_dir"
} | tee -a "$log_file"

cd "$make_dir"

set -x
MSYS2_ARG_CONV_EXCL="-Fo" \
make -f "$user_dir/Makefile.sevenzip_lib" "${arch_flags[@]}" "${make_vars[@]}" 2>&1 | tee -a "$log_file"
set +x

case "$platform" in
  windows) out_name="7z.dll" ;;
  mac) out_name="7z.dylib" ;;
  *) out_name="7z.so" ;;
esac

built_bin=""
if [[ "$platform" == "mac" ]]; then
  # Upstream makefile typically emits 7z.so even on mac; expose a mac-friendly
  # 7z.dylib in release output (same fallback build.sh used).
  if [[ -f "$build_dir/7z.dylib" ]]; then
    built_bin="$build_dir/7z.dylib"
  elif [[ -f "$build_dir/7z.so" ]]; then
    built_bin="$build_dir/7z.so"
  fi
elif [[ "$platform" == "windows" ]]; then
  [[ -f "$build_dir/7z.dll" ]] && built_bin="$build_dir/7z.dll"
else
  [[ -f "$build_dir/7z.so" ]] && built_bin="$build_dir/7z.so"
fi

if [[ -z "$built_bin" || ! -f "$built_bin" ]]; then
  echo "[ERROR] Built binary not found in: $build_dir" | tee -a "$log_file"
  exit 5
fi

cp -f "$built_bin" "$out_dir/$out_name"

if [[ "$platform" == "mac" ]] && command -v install_name_tool >/dev/null 2>&1; then
  install_name_tool -id "@rpath/$out_name" "$out_dir/$out_name"
fi

echo "[INFO] build finished, binary: $built_bin" | tee -a "$log_file"
echo "[INFO] copied output to: $out_dir/$out_name" | tee -a "$log_file"
