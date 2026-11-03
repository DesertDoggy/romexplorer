# dolphinrvz shared library

Builds Dolphin's ISO/GCZ/WIA/RVZ disc converter -- the same functionality as
`dolphin-tool convert` -- as a standalone shared library instead of a CLI executable,
exposing a small C API in [`dolphinrvz.h`](dolphinrvz.h) that takes the conversion
settings (input/output paths, format, scrub, block size, compression method/level) as
direct function arguments instead of parsed command-line flags.

## Output layout
- `deps/dolphin/release/{platform}/{arch}/{version}/dynamic/dolphinrvz.dll|libdolphinrvz.so|libdolphinrvz.dylib`
- `deps/dolphin/release/{platform}/{arch}/{version}/include/dolphinrvz.h`
- intermediate objects: `deps/dolphin/_build`

Nothing is ever written outside `deps/dolphin/` -- CMake is configured with `dolphin/` itself as
the source root (via `-S`), with the `dolphinrvz` target spliced in through
`-DCMAKE_PROJECT_INCLUDE=deps/dolphin/dolphinrvz_project_include.cmake` (see that file for why a
plain `add_subdirectory(user)` from a separate wrapper project doesn't work: some of
Dolphin's own CMake code, e.g. the Achievements-hash generation rule in
`Source/Core/Core/CMakeLists.txt`, references `${CMAKE_SOURCE_DIR}` directly and assumes
it is always the top-level project). `dolphin/`'s own build files are never edited.

## Build
```bash
deps/dolphin/scripts/build_dolphin_rvz.sh                 # auto-detect host platform/arch
deps/dolphin/scripts/build_dolphin_rvz.sh linux x64
deps/dolphin/scripts/build_dolphin_rvz.sh mac arm64
deps/dolphin/scripts/build_dolphin_rvz.sh windows x64
deps/dolphin/scripts/build_dolphin_rvz.sh android arm64    # requires ANDROID_NDK_HOME
deps/dolphin/scripts/build_dolphin_rvz.sh ios arm64         # requires Xcode; experimental, see below
```

### Prerequisites per platform
Same as building Dolphin itself for that platform -- see the upstream
[`BuildMacOSUniversalBinary.py`](../BuildMacOSUniversalBinary.py), [`AndroidSetup.md`](../AndroidSetup.md),
and Dolphin's own README for the full dependency list (cmake, ninja, a C++20 compiler,
and Dolphin's usual third-party libs -- SDL2, ffmpeg, etc). `dolphinrvz` links `discio` +
`uicommon`, the same targets `dolphin-tool` links, so it needs the exact same toolchain
and dependencies Dolphin's own build already requires on each platform -- nothing extra.

- **windows**: MSVC (Visual Studio, any edition with the "Desktop development with
  C++" workload). Dolphin's own `CMakePresets.json` has no MinGW generator, and its
  CMake has real MSVC-only assumptions (WIL in UICommon; `Externals/libusb/CMakeLists.txt`'s
  `if(WIN32)` branch, which unconditionally includes an MSVC-specific `config.h` and hard
  `#error`s under any other compiler) -- MinGW is not viable here even though
  chdman-simd (a separate, MinGW-first codebase) builds fine under MSYS2 MinGW64.
  `build_dolphin_rvz.sh` imports the MSVC environment itself (via `vcvarsall.bat`) even
  when run from an MSYS2 shell -- no separate "Developer PowerShell for VS" needed. If
  Visual Studio isn't installed at `C:\Visual Studio\18\Community`, set `VS_INSTALL_DIR`
  to your install root first.
- **linux**: cmake, ninja, a C++20 compiler, Dolphin's usual Linux dependency set.
- **mac**: Xcode command line tools.
- **android**: Android NDK r26+, `ANDROID_NDK_HOME` set. This drives CMake directly
  (not through Gradle) since only `discio`/`uicommon`/`dolphinrvz` are needed, not the
  Android UI/JNI layer -- if this diverges from what Dolphin's Gradle build configures,
  expect to reconcile CMake args against `Source/Android/app/build.gradle`.
- **ios**: Xcode. **Experimental** -- Dolphin upstream has no official iOS target (no
  `IOS`-specific handling anywhere in its CMakeLists.txt). This may need real fixes to
  `core`/`uicommon` that don't exist upstream; expect to iterate here.

## API
Header: `deps/dolphin/dolphinrvz.h`
- `dolphinrvz_convert(const DolphinRvzConvertOptions*)` -- runs one conversion
  synchronously. Same validation Dolphin's own convert CLI performs (required
  input/output/format, block size required and range-checked for GCZ/WIA/RVZ,
  compression method/level required and range-checked for WIA/RVZ, WIA rejects Zstd,
  RVZ rejects Purge). Returns 0 on success, negative on failure -- see
  `dolphinrvz_get_last_error()`. Returns exactly -6 specifically when `on_progress`
  returned 0 (the caller cancelled), distinguishable from every other failure.
- `dolphinrvz_get_last_error()`
- `dolphinrvz_get_allowed_compression_levels(compression, &min, &max)`

`DolphinRvzConvertOptions::on_progress` is called periodically with a status message and
percent complete (0-100); returning 0 from it cancels the conversion.

Not safe to call concurrently with itself -- shares process-wide DiscIO/Common state.
Serialize calls.

## Design notes
- Deliberately does **not** call `UICommon::Init()`. That also activates a video backend
  (`VideoBackendBase::ActivateBackend`), starts Discord Rich Presence, and sets up
  controller subsystems -- none of which disc conversion needs. `dolphinrvz.cpp`'s
  `EnsureMinimalInit()` does only what DiscIO/Common actually require: sets the user
  directory (DiscIO's WIA/RVZ writer needs one for temporary files, same as the official
  CLI's `--user` option), initializes the config system, and starts the logger.
- `deps/dolphin/dolphinrvz_target.cmake` still links the same `discio` + `uicommon` targets (and
  therefore the same third-party dependencies) the official `dolphin-tool` CLI links --
  this was a deliberate choice over trying to strip `discio`'s `PUBLIC core` dependency,
  since that dependency is real (not an upstream mistake) and stripping it would mean
  patching Dolphin's own source rather than just wrapping it. Discord Rich Presence is
  turned off at the CMake level (`USE_DISCORD_PRESENCE OFF`) since it's skipped at
  runtime anyway; the Qt GUI, test suite, autoupdate, analytics, retroachievements, and
  mGBA integration are turned off too since they're unrelated to disc conversion and
  `dolphin-tool` doesn't need them either.
- Adding the `dolphinrvz` target is deferred (`cmake_language(DEFER)`, CMake 3.25+) to
  the end of `dolphin/CMakeLists.txt`'s processing, since `discio`/`uicommon` don't exist
  as targets yet at the point `CMAKE_PROJECT_INCLUDE` runs (right after dolphin's own
  `project()` call, before its `add_subdirectory(Source)`).
