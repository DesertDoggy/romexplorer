# RAHasher stream library inputs

This page describes the input values your app must provide to use libRAHasher.

## 1) Call order
1. rahasher_stream_begin(...)
2. rahasher_stream_feed(ctx, chunk, size) repeated
3. rahasher_stream_finish(ctx, out_hash)
4. rahasher_stream_destroy(ctx)

If you abort early, call rahasher_stream_destroy(ctx) directly.

## 2) Inputs for rahasher_stream_begin
Function:
- rahasher_stream_begin(uint32_t console_id, const char* source_name, const char* system_dir, uint64_t expected_total_bytes, rahasher_progress_callback progress_cb, rahasher_notice_callback notice_cb, void* userdata)

### Required inputs
1. console_id (uint32_t)
- Specific RetroAchievements console id.
- Must be a specific id in current implementation (<= 90).
- Examples: Game Boy Advance = 5, PlayStation = 12, PlayStation 2 = 21, Nintendo 3DS = 62.
- Console id list source: src/rcheevos/include/rc_consoles.h

2. source_name (const char*)
- Logical file name/path used by hash logic for extension-sensitive behavior.
- Can be only a filename, does not need to be a real file path.
- Example values:
  - "game.iso"
  - "rom.gba"
  - "archive.zip"

### Optional inputs
3. system_dir (const char*)
- Path to system assets used by some formats (notably 3DS keys).
- Pass "." if not needed.
- For 3DS, this should contain files like aes_keys.txt (and seeddb.bin when required).

4. expected_total_bytes (uint64_t)
- Total stream size if known.
- Pass 0 when unknown.
- Used only for progress reporting (bytes_expected).

5. progress_cb (rahasher_progress_callback)
- Optional callback. Pass NULL if you do not need progress events.
- Signature:
  - (const char* stage, uint64_t bytes_read, uint64_t bytes_expected, void* userdata)
- stage values currently emitted:
  - feed-start
  - feed
  - feed-complete
  - hash-read
  - done

6. notice_cb (rahasher_notice_callback)
- Optional callback. Pass NULL if you do not need notices.
- Signature:
  - (int severity, const char* message, void* userdata)
- severity:
  - 0 = verbose
  - 1 = error

7. userdata (void*)
- Optional pointer returned to callbacks.
- Pass NULL if unused.

## 3) Inputs for rahasher_stream_feed
Function:
- rahasher_stream_feed(rahasher_stream_ctx_t* ctx, const uint8_t* data, size_t size)

Required inputs:
1. ctx
- Context returned by rahasher_stream_begin.

2. data
- Pointer to chunk bytes.

3. size
- Number of bytes in this chunk.
- Must be > 0.

Notes:
- Feed chunks in correct file order.
- You can use any chunk size (for example 4 KB, 64 KB, 1 MB).

## 4) Inputs for rahasher_stream_finish
Function:
- rahasher_stream_finish(rahasher_stream_ctx_t* ctx, char out_hash[33])

Required inputs:
1. ctx
- Active context.

2. out_hash
- Caller-owned buffer of at least 33 bytes.
- On success returns 32-char lowercase MD5 plus null terminator.

## 5) Return codes
- 0: RAHASHER_STREAM_OK
- -1: RAHASHER_STREAM_ERR_INVALID_ARG
- -2: RAHASHER_STREAM_ERR_IO
- -3: RAHASHER_STREAM_ERR_HASH
- -4: RAHASHER_STREAM_ERR_UNSUPPORTED

On non-zero return, call:
- rahasher_stream_get_last_error(ctx)

## 6) Practical minimum you need
If you want the simplest integration, provide only:
1. console_id
2. source_name
3. stream chunks (feed)
4. out_hash buffer (33 bytes)

Use these defaults:
- system_dir = "."
- expected_total_bytes = 0
- progress_cb = NULL
- notice_cb = NULL
- userdata = NULL

## 7) Current limitations
- .m3u stream input is not supported in this initial implementation.
- For 3DS hashing, required key files must exist under system_dir.
