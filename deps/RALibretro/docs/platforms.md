# Platform hash types

This document summarizes rcheevos/RAHasher hash behavior in this fork.

## Global behavior
- Final output is lowercase 32-char MD5 hex.
- Source: src/rcheevos/src/rhash/hash.c (rc_hash_finalize)
- Many file paths cap processed data at MAX_BUFFER_SIZE (64 MiB).
- Source: src/rcheevos/src/rhash/rc_hash_internal.h

## 1) Whole-file MD5 (no special preprocessing)
- Arcadia 2001
- Atari 2600
- Atari Jaguar
- ColecoVision
- Elektor TV Games Computer
- Fairchild Channel F
- Game Boy
- Game Boy Advance
- Game Boy Color
- Game Gear
- Intellivision
- Interton VC 4000
- Magnavox Odyssey2
- Master System
- Mega Duck
- Neo Geo Pocket
- Oric
- Pokemon Mini
- Sega 32X
- SG-1000
- Supervision
- TI-83
- TIC-80
- Uzebox
- Vectrex
- Virtual Boy
- WASM-4
- WonderSwan
- ZX Spectrum

Hash type:
- MD5(file bytes) using whole-file path.

## 2) Whole-file MD5 with m3u support
- Mega Drive
- Amstrad PC
- Apple II
- Commodore 64
- MSX
- PC-8800

Hash type:
- Non-m3u: MD5(file bytes)
- m3u: parse playlist, then hash first referenced item.

## 3) Buffered MD5 with header normalization
- Atari 7800: skip 128-byte header when signature matches.
- Atari Lynx: skip 64-byte header when signature matches.
- Nintendo and FDS: skip 16-byte header when signature matches.
- PC Engine: skip 512-byte header when size pattern matches.
- Super Cassette Vision: skip 32-byte header when signature matches.
- Super Nintendo: skip 512-byte header when size pattern matches.

Hash type:
- MD5(normalized payload after optional header skip).

## 4) Special non-disc preprocessing (keep RAHasher)
- Arcade
  - .neo: skip 4096-byte header, then MD5 payload.
  - non-.neo: MD5 of filename (with optional subsystem folder), not file bytes.
- Arduboy
  - .arduboy package handling or Intel HEX normalized text hashing.
- Nintendo 64
  - detect z64/v64/n64 and byte-swap to canonical order before MD5.
- Nintendo DS and Nintendo DSi
  - hash selected sections (header subset + ARM9 + ARM7 + icon block).
- Nintendo 3DS
  - encrypted-format aware parsing/decryption/canonicalization before MD5.

## 5) Disc-oriented hashing (keep RAHasher)
- 3DO
- Atari Jaguar CD
- Dreamcast
- GameCube
- Neo Geo CD
- PC Engine CD
- PC-FX
- PlayStation
- PlayStation 2
- PSP
- Sega CD
- Saturn
- Wii

Hash type:
- MD5 of platform-specific canonical content derived from disc structures.
- Not a naive full-file digest.

## 6) MS-DOS
- Uses dedicated rc_hash_ms_dos handler.

## 7) Console IDs present but not explicitly handled by current file-hash dispatch
- Wii U
- Xbox
- ZX81
- VIC-20
- Amiga
- Atari ST
- CD-i
- PC-9800
- Atari 5200
- X68000
- Cassette Vision
- FM Towns
- Game and Watch
- Nokia N-Gage
- Sharp X1
- Thomson TO8
- PC-6000
- Pico
- Zeebo
- PlayStation 3

## External bypass summary
External app can usually bypass RAHasher for:
- Simple whole-file MD5 platforms.
- Header-skip MD5 platforms if skip condition is verified.

Keep RAHasher for:
- Disc-oriented platforms.
- N64, NDS/DSi, 3DS, Arcade special handling.
