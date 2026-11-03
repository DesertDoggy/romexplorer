# Stream Hash Callback Contract

This document defines the extraction data callback added for 7z extraction.

## Interface

File: CPP/7zip/Archive/IArchive.h
Interfaces:

1) IArchiveExtractCallbackData2
Method:

OnData(
  UInt32 index,
  UInt64 offsetInFile,
  const void *data,
  UInt32 size,
  Int32 askExtractMode,
  Int32 *action)

2) IArchiveExtractCallbackDataFile2
Methods:

OnFileBegin(
  UInt32 index,
  UInt64 unpackSize,
  Int32 askExtractMode,
  Int32 *action)

OnFileEnd(
  UInt32 index,
  Int32 opRes,
  Int32 askExtractMode)

## Behavior

- The callback is optional.
- The archive handler queries these interfaces from IArchiveExtractCallback via QueryInterface.
- It is invoked from the 7z extraction output path for each unpacked chunk.
- For archives with multiple files, chunks are reported per file via `index`.
- `offsetInFile` is always relative to the current file identified by `index`.
- askExtractMode is one of NArchive::NExtract::NAskMode values.
- action is one of NArchive::NExtract::NDataAction values:
  - kContinue (0): continue extraction.
  - kStop (1): request stop.
- OnFileBegin/OnFileEnd give explicit file boundaries for per-file hash lifecycle.

## Multi-file hashing model

- Treat each unique `index` as an independent hash stream.
- Initialize hash state in `OnFileBegin(index, ...)`.
- Feed each `OnData()` chunk to that file's hash state.
- Finalize and publish that file hash in `OnFileEnd(index, opRes, ...)`.
- Keep output as one record per file (for example: `{ index, path, size, digest }`).

This allows an archive with many files to produce many independent file hashes in one extraction pass.

## Early-stop rule

- Fast stop (kStop) is honored only in test mode (askExtractMode == kTest).
- This avoids writing truncated extracted files in extract mode.
- When stop is honored, extraction returns success path with partial completion semantics.
- `kStop` can be requested from `OnFileBegin()` or `OnData()`.

## Intended usage

- Hash-only mode (no file writes): run archive extraction in test mode and consume chunk data in OnData().
- Partial hashing: stop once enough bytes are consumed for the external hasher.
- Full-content per-file hashing: keep `action = kContinue` and finalize each file separately.

## Notes

- Hash calculation itself is not implemented in 7-Zip-zstd by this change.
- External project is responsible for algorithm state and digest finalization.
- For solid archives, decompression may still need to process data before target offset/file.
