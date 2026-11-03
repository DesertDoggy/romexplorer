# Hasher Design Plan

## Goal

Build a streaming, threaded hash pipeline for CHD and RVZ content that matches the CHDLite model: process each chunk once, fan out the same bytes to all active hash engines in parallel, and aggregate the final digest set without re-reading the archive.

## Core rule

The hasher must process each chunk exactly once and pass the same chunk buffer to every selected algorithm in the same pass. It should not re-read the source for every hash variant.

## Default policy

By default, the hasher should compute only the standard compatibility set:

- CRC32
- MD5
- SHA1

This keeps the default path fast and low-overhead for common verification and metadata checks.

## Extended policy

The hash engine should not be limited to a single predefined “expanded profile.” Instead, the caller should be able to select any combination of the supported algorithms from the full extended set:

- CRC32
- MD5
- SHA1
- SHA256
- XXH3_128
- RAHash

The default behavior, when no algorithm selection is specified, is the compatibility baseline:

- CRC32
- MD5
- SHA1

Any other set is an explicit opt-in. For example, the caller may request only SHA256 + RAHash, or CRC32 + SHA256 + XXH3_128, or the full extended set. The engine should treat these as independent bitmask selections rather than as a fixed profile.

## Parallel streaming design

The chunked hasher should be threaded and use a worker fan-out model:

1. The archive reader emits chunk boundaries with offsets and lengths.
2. A producer thread pushes each chunk into a work queue.
3. A fixed worker pool processes chunks in parallel.
4. Each worker updates the enabled hash engines for that chunk.
5. CRC32 and MD5 may remain in a lightweight fast path when the caller is using the default profile, but they still flow through the same threaded chunk pipeline.
6. The final state for each hash algorithm is merged after the chunk stream ends.

This is the CHDLite-style parallel hashing model: preserve per-chunk concurrency, but still maintain a single stream pass over the archive data.

## RAHasher integration

RAHash should be treated as another engine in the same chunked pipeline, not as a separate full-file pass:

- RAHash is inserted alongside the native algorithms in the chunk fan-out
- when enabled, its state is updated per chunk in the same worker model
- it is optional and feature-gated behind the external dylib path
- the same API shape is used for native hashing and RAHash-backed hashing

The library should expose a common `HashProfile` / `HashFlags` contract as a set of selectable flags, not as a fixed two-tier profile. The default should be:

- `HashFlags::default()` => `CRC32 + MD5 + SHA1`

And any combination of the remaining extended algorithms should be valid, such as:

- `SHA256`
- `CRC32 + SHA256 + XXH3_128`
- `RAHash + MD5`
- `CRC32 + MD5 + SHA1 + SHA256 + XXH3_128 + RAHash`

In other words, the extended set is the full supported universe, while the default remains the standard three-hash baseline when no selection is explicitly supplied.

## Why this is the right model

- one read of each chunk
- one fan-out to all selected algorithms
- parallel processing across chunks
- no redundant archive scans
- compatible with CHD and RVZ streaming readers
- keeps the default path lean while still supporting full verification parity when requested

## API sketch

```rust
bitflags! {
    pub struct HashFlags: u32 {
        const CRC32 = 1 << 0;
        const MD5 = 1 << 1;
        const SHA1 = 1 << 2;
        const SHA256 = 1 << 3;
        const XXH3_128 = 1 << 4;
        const RAHASH = 1 << 5;
    }
}

impl Default for HashFlags {
    fn default() -> Self {
        Self::CRC32 | Self::MD5 | Self::SHA1
    }
}

pub struct HashRequest {
    pub algorithms: HashFlags,
    pub chunk_size: usize,
    pub stop_after: Option<u64>,
}

pub struct HashReport {
    pub crc32: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub xxh3_128: Option<String>,
    pub rahash: Option<String>,
}
```

## Validation


- default selection computes CRC32/MD5/SHA1 only when no flags are specified
- arbitrary combinations of the extended set are accepted and produce the matching digest set
- full extended set includes CRC32/MD5/SHA1/SHA256/XXH3_128/RAHash
- threaded chunk processing yields the same final digest as a single-threaded pass
- no chunk is read twice when multiple algorithms are active
- RAHash produces the expected result when the dylib is present
