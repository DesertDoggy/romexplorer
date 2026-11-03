# chdman shared library

Builds chdman (MAME's CHD manager, with chdman-simd's SIMD/pipeline patches) as a single
self-contained shared library instead of the `chdman.exe` CLI, exposing a small C API in
[`chdman_lib.h`](chdman_lib.h) that takes the same arguments the CLI does.

## Output layout
- `deps/chdman-simd/release/{platform}/{arch}/{version}/dynamic/chdman.dll|libchdman.so|libchdman.dylib`
- `deps/chdman-simd/release/{platform}/{arch}/{version}/include/chdman_lib.h`
- intermediate objects: `deps/chdman-simd/_build`

Nothing is ever written outside `deps/chdman-simd/` -- the `chdman-simd` and `mame` submodule
checkouts stay pristine.

## Build
```bash
deps/chdman-simd/scripts/build_chdman_lib.sh                  # auto-detect host platform/arch
deps/chdman-simd/scripts/build_chdman_lib.sh linux x64
deps/chdman-simd/scripts/build_chdman_lib.sh mac arm64
deps/chdman-simd/scripts/build_chdman_lib.sh windows x64       # run from an MSYS2 MinGW64 shell
deps/chdman-simd/scripts/build_chdman_lib.sh android arm64     # requires ANDROID_NDK_HOME
deps/chdman-simd/scripts/build_chdman_lib.sh ios arm64         # requires Xcode; experimental, see below
```

Requires the `mame` and `zlib-ng` submodules initialized as siblings of `chdman-simd`
under `submodules/`.

### Prerequisites per platform
- **windows**: MSYS2 MinGW64 shell, `pacman -S mingw-w64-x86_64-gcc mingw-w64-x86_64-cmake mingw-w64-x86_64-make`. Optional: `mingw-w64-x86_64-uasm` for the hand-written LZMA ASM decoder.
- **linux**: gcc/g++, cmake, make.
- **mac**: Xcode command line tools (clang, cmake).
- **android**: Android NDK r26+, `ANDROID_NDK_HOME` set.
- **ios**: Xcode. **Experimental** -- MAME/chdman upstream have no official iOS target; this only works because chdman is a batch CLI tool with no JIT/emulation core, but expect to iterate on real build errors here.

## API
Header: `deps/chdman-simd/chdman_lib.h`
- `chdman_run(argc, argv, out_log, on_progress, user_data)` -- runs chdman with the same
  command-line arguments the CLI takes (`createcd`, `extractcd`, `createdvd`, `createhd`,
  `info`, `verify`, ...). `argv` excludes the program name: `argv[0]` is the command,
  followed by its options and values, exactly as documented by `chdman help <command>`.
  Returns the same exit code the CLI would. `on_progress` (may be NULL) is called live,
  once per line chdman writes to stdout/stderr, with a parsed percentage when the line
  matches chdman's `"<float>% complete"` format (-1.0f otherwise) -- this is how a caller
  sees progress during long-running commands. The full accumulated stdout/stderr text is
  also returned via `out_log` regardless (caller frees with `chdman_free_log`).
- `chdman_free_log(log)`

Not safe to call concurrently with itself -- chdman's command handlers use process-wide
state (the shared CHD compressor thread pool). Serialize calls.

## Design notes
- `chdman.cpp`'s own `main()` is never modified. It's compiled with `-Dmain=chdman_cli_entry`
  (see `Makefile.chdman_lib`), renaming the entry point at the preprocessor level so the
  wrapper in `chdman_lib.cpp` can call it as an ordinary function while reusing chdman's
  real, unmodified command dispatch (`s_commands[]` lookup, option parsing, error handling).
- Same trick, same file, for one more call: `main()`'s first line calls
  `osd_get_command_line(argc, argv)` expecting it to just wrap the two into a
  `vector<string>` -- but on Windows that function ignores both and instead re-fetches the
  real OS process command line via `GetCommandLineW()`. That's correct for a real
  `chdman.exe` process, and wrong for chdman running as a library call inside another
  process (it would see *that other process's* command line, not the argv `chdman_run()`
  was actually given). `-Dosd_get_command_line=chdman_lib_get_command_line` redirects that
  one call, inside this one translation unit only, to a matching function in
  `chdman_lib.cpp` that does only the straightforward wrap-argc/argv-into-a-vector job.
  `src/osd/osdcore.cpp`'s real `osd_get_command_line` is untouched.
- On non-Windows platforms, MAME's own `osdlib_unix.cpp`/`osdlib_macosx.cpp` are **not**
  used -- both unconditionally pull in SDL2, which a batch CLI tool never needs.
  `deps/chdman-simd/osdlib_posix_min.cpp` implements just the handful of `osd_*` symbols chdman's link
  graph actually requires, with zero extra dependencies.
- x64 builds use an SSE2 baseline (portable to all x86-64 CPUs) with AVX2/SHA-NI compiled
  in per-file and selected at runtime via CPUID dispatch (chdman-simd's own patches) --
  never assumed present. arm64 builds use NEON unconditionally (mandatory on AArch64); no
  x86 intrinsics are compiled in.
- All dependencies (LZMA, zstd, expat, utf8proc, FLAC, zlib-ng) are statically linked into
  the one output shared library -- nothing else needs to ship alongside it at runtime.
