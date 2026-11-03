/* lzma_shim.c -- thin one-shot decode wrapper around the vendored LZMA SDK's LzmaDec.c.
 *
 * CHD's `lzma`/`cdlz` codecs encode each hunk independently (the encoder is re-primed for every
 * hunk), so a full streaming decoder context is unnecessary: `LzmaDecode()` (already provided by
 * LzmaDec.c) does exactly what we need in one call, decoding straight into the caller-provided
 * output buffer.
 *
 * This file intentionally contains no decode logic of its own -- it only supplies the malloc/free
 * based ISzAlloc that LzmaDec.c requires, and adapts LzmaDecode()'s signature to something easy to
 * bind from Rust via a plain `extern "C"` function (see src/modules/lzma_native.rs).
 */

#include <stdlib.h>

#include "LzmaDec.h"

static void *RomExplorer_LzmaAlloc(ISzAllocPtr p, size_t size)
{
    (void)p;
    return malloc(size);
}

static void RomExplorer_LzmaFree(ISzAllocPtr p, void *address)
{
    (void)p;
    free(address);
}

static const ISzAlloc g_RomExplorerLzmaAlloc = { RomExplorer_LzmaAlloc, RomExplorer_LzmaFree };

/* Decodes exactly `dest_len` bytes of raw LZMA1 data from `src` (`src_len` bytes) into `dest`.
 *
 * `props` must point to the 5-byte LZMA1 properties header (LZMA_PROPS_SIZE) as produced by
 * chdman's LZMA encoder (see `lzma_codec_init` in libchdr_chd.c for how it's derived: level 9,
 * `reduceSize = hunkbytes`).
 *
 * Returns 0 on success (and exactly dest_len bytes were produced), or a negative value on
 * failure (property parsing error, allocation failure, or decode/data error).
 */
int romexplorer_lzma_decode(
    const unsigned char *props, unsigned props_size,
    const unsigned char *src, size_t src_len,
    unsigned char *dest, size_t dest_len)
{
    SRes res;
    ELzmaStatus status;
    SizeT out_len = dest_len;
    SizeT in_len = src_len;

    res = LzmaDecode(
        dest, &out_len,
        src, &in_len,
        props, props_size,
        LZMA_FINISH_END,
        &status,
        &g_RomExplorerLzmaAlloc);

    if (res != SZ_OK)
        return -1;
    if (out_len != dest_len)
        return -2;

    return 0;
}
