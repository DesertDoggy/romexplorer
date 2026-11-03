# RAHasher dylib stream plan

## Goal
Build a macOS dylib for RAHasher with stream/chunk input for ISO and special formats, while keeping new files under deps/RALibretro/ only and minimizing edits to existing source.

## Constraints
- No done flag in API. Return only hash or error.
- Caller controls abort by stopping feed or destroying context.
- New files must be under deps/RALibretro/.
- Existing repo code edits only when required for linkage/reuse.

## API direction
- C ABI: begin, feed, finish, destroy, get_last_error.
- Progress callback: stage + bytes_read + bytes_expected.
- Notice callback: verbose/error message forwarding.

## Progress model
- ISO/disc formats: parse header/metadata first, then compute bytes_expected when possible.
- Before expected bytes are known: emit stage-only progress.
- After known: app may derive percent = bytes_read / bytes_expected.
- Non-ISO small files: external app may bypass RAHasher when safe.

## Hash routing policy
Use RAHasher for:
- All platforms.
- bypass was planned for md5 platforms but calculation estimate <10ms per file, less complicated with no bypass.


## Build output
- deps/RALibretro/release/{platform}/{arch}/{version}/{type(dynamic/static/bin)}/libRAHasher.ext(dylib etc)
- interemediate build dir deps/RALibretro/_build
- one build script for all platforms. auto detect if no target option. if option (mac/windows/linux/iphone/android) then buid for those.
- mac/iphone/android only arm64, win/linux only x64
- use avx2/avx512 if possible.
- use c/c++ 23 if possible

## Planned files under deps/RALibretro/
- deps/RALibretro/rahasher_stream.h
- deps/RALibretro/rahasher_stream.cpp
- deps/RALibretro/Makefile.rahasher.dylib
- deps/RALibretro/sample_stream_client.cpp
- deps/RALibretro/README.rahasher-dylib.md
- deps/RALibretro/docs/platforms.md

## Verification checklist
1. Build dylib and confirm exported symbols.
2. Same input with different chunk sizes produces same hash.
3. Compare dylib result with existing RAHasher CLI result.
4. Confirm progress callback behavior for known/unknown expected bytes.
5. Confirm notice callback forwarding for verbose/error events.
