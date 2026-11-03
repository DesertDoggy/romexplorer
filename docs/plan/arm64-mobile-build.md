# arm64 + mobile across every native submodule, and a local release build script

> **Executed locally**, where the submodule repos are writable. The survey below is the
> useful part: it was measured, and a fresh session would otherwise have to re-derive it.

## Status (2026-10-01): done for linux and windows arm64; mobile left as placeholders

- **The native builds are verified.** Every `SUPPORTS_ARM64_{LINUX,WINDOWS}` entry is 1. Each
  was flipped after its output was checked to be AArch64: ELF with `readelf`, PE with
  `llvm-objdump`. linux/arm64 was built in WSL (Ubuntu 26.04), and windows/arm64 on the
  Windows x64 host.
- **Where the survey below was wrong:**
  - **7-Zip was not "nothing to do".**
    - It takes cross compilers from the environment: `build_native_deps.sh`'s `cross_env`.
    - windows/arm64 hit upstream warnings under `-Werror`, and a stray `printf` in
      `C/Sha512.c`.
    - Its Windows `-lz` linked MSYS2's stock zlib rather than zlib-ng, on x64 as well.
  - **mariadb-server cross builds need host tools.** Linux runs them under qemu
    (`CMAKE_CROSSCOMPILING_EMULATOR`). Windows builds them for x64 first and imports them
    (`IMPORT_EXECUTABLES`).
  - **windows/arm64 needs llvm-mingw** for the four MinGW-built libraries: MSYS2 has no
    aarch64 gcc that runs on an x64 host.
  - **Upstream patches assumed x86.** chdman-simd's patches use x86 `cpuid` unguarded; the fix
    is in `user/patches/arm64/`. RAHasher had a missing `<stdlib.h>`.
  - **Dolphin needed less.** It needs udev only for evdev/hwdb, now off, so no
    `libudev-dev:arm64`.
- **Mobile:** android/ios are placeholders, as decided. mariadb-server's script accepts the
  targets and exits 2 with "not implemented yet".
- **Release script and packages:** `scripts/build_release.sh` exists. The `cli` kind this
  plan describes below was built and then **removed**: it packaged the server without
  MariaDB. `cli` now names the standalone tools: `scripts/package.sh --kind cli` packages
  chdlite (the `chdlite/` crate, its own version) into `release/cli/<version>/`.

## Context

Two related goals:

1. `build_native_deps.sh` refuses linux/arm64 and windows/arm64 for every submodule, so only
   x64 can be built. The blockers are in the submodule build scripts, not in romexplorer.
2. Producing a local artifact set means running four scripts in a load-bearing order that is
   written down nowhere. A single `scripts/build_release.sh` should do it, taking multiple
   platforms, arches and modes.

Mobile is **scripts only** — the submodule build scripts gain android/ios arms so the
libraries can be produced, but romexplorer gains no mobile package (the GUI's `mobile`
feature exists, but `build_gui_web.sh` says android/ios are separate `dx` commands that do
not come through it, and no apk/ipa pipeline exists).

## Survey: what each submodule already has

Measured by grepping `build_one <platform> <arch>` in each checked-out script. **This is the
part worth keeping** — it is smaller than it looks.

| submodule | linux/x64 | mac/arm64 | win/x64 | android | ios | **linux/arm64** | **win/arm64** |
|---|---|---|---|---|---|---|---|
| openssl | ✓ | ✓ | ✓ | ✓ | ✓ | add | add |
| zlib-ng | ✓ | ✓ | ✓ | ✓ | ✓ | add | add |
| mariadb-connector-c | ✓ | ✓ | ✓ | ✓ | ✓ | add | add |
| mariadb-server | ✓ | ✓ | ✓ | **add** | **add** | add | add |
| 7-Zip-zstd | — | — | — | ? | ? | **nothing to do** | nothing to do |
| chdman-simd | ? | ? | ? | ? | ? | add | add |
| dolphin | ? | ? | ? | ? | ? | add | add |
| RALibretro | ? | ? | ? | ? | ? | add | add |

Key facts behind that table:

- **7-Zip needs no change.** Its `build.sh` already does `arm64|aarch64) arch="arm64"` and has
  an `arm64)` branch. It fails only because it links zlib-ng's release tree for the same arch
  — unblocked for free once zlib-ng builds arm64, exactly as `SUPPORTS_ARM64`'s comment says.
- **openssl, zlib-ng and mariadb-connector-c already build arm64** for mac, iOS and Android,
  with `ARM_ISA_DEFS` and toolchain-file plumbing in place. They lack only the *linux* and
  *windows* arm64 arms — this is adding a case, not inventing cross-compilation.
- **mariadb-server is the big one.** It has only `build_linux`, `build_mac`, `build_windows`
  and no arch selector at all (`--platform` only). It needs android and iOS from scratch plus
  arch parameterisation.
- **chdman-simd, dolphin, RALibretro are not checked out** here, so their state is unknown and
  must be surveyed the same way before editing. `chdman-simd` additionally extracts sources
  from `external/mame`, a large fetch.

## What is verifiable, and where

Be explicit per target rather than claiming a uniform "done":

| target | verifiable locally? |
|---|---|
| linux/arm64 | **yes** — `gcc-aarch64-linux-gnu` installs from apt; cross-build and check with `file`/`readelf` |
| windows/arm64 | only on Windows with the MSVC ARM64 toolset |
| android/arm64 | needs the NDK; `ANDROID_NDK_ROOT` is already consumed by the existing android arms |
| ios/arm64 | macOS + Xcode only |

So: build linux/arm64 for real and prove it. Write the other three carefully, follow each
script's existing arm64 arm as the template, and **say in the commit that they are unverified**
rather than implying otherwise.

## The work

### A. Submodule scripts (7 repos, one pass)

Per submodule, mirroring its own existing arm64 arm:

1. Add `build_one linux arm64` with a cross-toolchain guard — follow the existing
   `LINUX_X64_TOOLCHAIN_FILE` / `LINUX_X64_CC` pattern (zlib-ng already has that idea for
   cross-building linux/x64 from a non-Linux host) and add the arm64 equivalent.
2. Add `build_one windows arm64`. For MSVC-driven builds that means `vcvarsall.bat arm64`
   instead of `x64`.
3. For **mariadb-server only**: add android and iOS arms, and make arch selectable rather than
   implied by platform.
4. Do **not** touch 7-Zip.

Each script's own `--list`/usage text and any platform allowlist needs the new targets too,
or the script will refuse what it can now do.

### B. romexplorer side

- `scripts/build_native_deps.sh`: flip `SUPPORTS_ARM64` to 1 per submodule **as each one
  actually lands**, not up front. The refusal message it prints is the best documentation of
  what remains; keep it honest.
- `scripts/build_release.sh` (new) — one command, multiple targets:

  ```
  --platform linux,windows,macos   --arch x64,arm64   --mode server,desktop,cli
  --no-clean --skip-native --native-only --core --no-gui-web --jobs N --continue
  ```

  Selectors take comma-separated lists and expand to their cartesian product. Libraries and
  the wasm build run **once per run, not per combination**. Order: preflight (reuse
  `build_native_deps.sh --check`) → natives → `build_gui_web.sh` → `package.sh` per
  combination → `appimage.sh`/`macos_app.sh` where the host can run them → `SHA256SUMS`
  → summary with per-step timings. Non-zero exit if every requested combination was skipped.
- Package kinds become **server / desktop / cli** (`package.sh:85` accepts only
  `server|desktop` today; six places branch on `"$KIND" == "server"` — convert to a `case`).
  `cli` is the server binaries without `app_root_ex/lib/<os>/<arch>/mariadb`: verified that
  `romexplorer_server` links **no OpenSSL at all** (rustls on `ring`), so this drops ~118 MB to
  roughly 30 MB. **Constraint:** `romexplorer_server.rs:154` calls `check_layout` in the
  `--init-from-config` branch, so a `cli` package cannot run first-run setup — gate it in
  `install.sh` with a message pointing at an external database.
- **No `mobile` kind** — mobile is submodule scripts only, per the above.

### C. Release layout

Already applied to three scripts (see "Carry these across" below):

    release/<version>/<os>/<arch>/<kind>/    staged package
    release/<version>/dist/                  archives + SHA256SUMS

Still to update: `.github/workflows/release-packages.yml` (six references; add a step reading
the workspace version into `$GITHUB_ENV`, and recheck `gather`'s
`incoming/packages-*-staged/*/*/` glob depth, which changes), `Dockerfile` (drop the stale
`ARG PACKAGE_DIR` default), `.dockerignore` (`!release` plus re-excluding `release/*/dist`).

### D. `docs/release_notes.md` — 0.1.0 alpha

New file beside `docs/*.md`. Measured facts, not estimates: server package 118 MB staged /
43 MB archived; native lib tree 410 MB (171 MB compressed); full core build ~24 min on 4
cores (openssl 122s, zlib-ng 13s, 7-Zip 65s, connector 15s, mariadb-server 721s, Rust 473s).
Known gaps stated plainly: Windows/macOS packaging never executed on those hosts; the Windows
MariaDB `share/` gap and its workaround; Docker image unbuilt; `chdman`/`dolphinrvz`/`RAHasher`
absent unless built, so CHD/RVZ and RetroAchievements hashing are unavailable without them.
Alpha caveat: no database upgrade-compatibility promise across 0.1.x.

## Verification

- `apt-get install gcc-aarch64-linux-gnu`, then `build_native_deps.sh linux arm64 --only
  zlib-ng,openssl` and confirm with `file`/`readelf -h` that the output is genuinely
  `AArch64`, not x86-64 with an arm64 path.
- Then `--only 7-Zip-zstd` for linux/arm64 **without editing 7-Zip**, proving the transitive
  unblock claim rather than assuming it.
- `build_release.sh --skip-native --mode server,cli` → `release/<version>/linux/x64/{server,cli}/`
  and `release/<version>/dist/` with a `sha256sum -c`-clean `SHA256SUMS`.
- `cli`: `mariadb/` absent, `ldd` shows no OpenSSL, extracted tarball's `romexplorer_dbtool
  check` reports MariaDB missing rather than crashing.
- `--platform linux --arch arm64` before the submodule work lands must skip with
  `build_native_deps.sh`'s own per-submodule reason, not attempt it.
- Workflow YAML parses; still `contents: read`, no release-publishing step.

## Carry these across before leaving the cloud session

`scripts/package.sh`, `scripts/appimage.sh` and `scripts/macos_app.sh` are **edited and
uncommitted** in the cloud container with the new layout. They are lost when it is reclaimed.
Commit and push them first, then `git pull` locally — otherwise section C is redone from
scratch.

Also uncommitted/ephemeral there: `app_root_ex/lib/linux/x64` (410 MB, 15 min to rebuild,
pin-key `b380937f6af56efb`) and `release/` output. All reproducible; none worth migrating.
