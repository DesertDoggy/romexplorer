/* flac_shim.c -- thin wrapper around libchdr's vendored FLAC decoder (libchdr_flac.c, which
 * itself wraps dr_flac.h) for CHD's `flac`/`cdfl` codecs.
 *
 * This links libchdr_flac.c (and, transitively, dr_flac.h with DR_FLAC_IMPLEMENTATION) as-is
 * rather than reimplementing FLAC decoding in Rust: CHD's `cdfl` codec needs to know exactly how
 * many *bytes* of the compressed input the FLAC bitstream decoder consumed (so the remaining
 * bytes, a separately zlib-compressed subcode stream, can be located and inflated) -- this relies
 * on dr_flac's internal bit-cache bookkeeping (see `flac_decoder_finish` in libchdr_flac.c), which
 * would be very difficult to reproduce bit-exactly with an independent Rust FLAC decoder crate.
 * Reusing the vendored C implementation directly guarantees byte-for-byte compatibility with the
 * reference chdman/libchdr behavior.
 */

#include <stdint.h>

#include <libchdr/flac.h>

/* Decodes a standalone (non-CD) `flac` codec hunk. `src[0]` is a 'L'/'B' marker (chdman writes
 * whichever endianness of raw PCM samples compressed better); the rest of `src` is the FLAC
 * stream produced by `flac_decoder_reset`'s synthetic-header framing. Writes exactly
 * `num_samples` interleaved stereo `int16_t` sample-frames (`num_samples * 2` int16 values) to
 * `dest`. Returns 0 on success, negative on failure. */
int romexplorer_flac_decode_plain(
    const unsigned char *src, unsigned src_len,
    int16_t *dest, unsigned num_samples)
{
    flac_decoder decoder;
    int swap_endian;
    uint16_t native_endian = 0;
    *(uint8_t *)(&native_endian) = 1;

    if (src_len < 1)
        return -1;
    if (src[0] == 'L')
        swap_endian = !(native_endian & 1);
    else if (src[0] == 'B')
        swap_endian = (native_endian & 1);
    else
        return -1;

    if (flac_decoder_init(&decoder))
        return -2;
    if (!flac_decoder_reset(&decoder, 44100, 2, num_samples, src + 1, src_len - 1)) {
        flac_decoder_free(&decoder);
        return -3;
    }
    if (!flac_decoder_decode_interleaved(&decoder, dest, num_samples, swap_endian)) {
        flac_decoder_free(&decoder);
        return -4;
    }
    flac_decoder_finish(&decoder);
    return 0;
}

/* Decodes the FLAC-compressed audio portion of a CD-frontend `cdfl` codec hunk. Unlike the plain
 * `flac` codec, there is no leading endian-marker byte -- the swap flag is fixed to whatever this
 * host's native endianness is (matches `cdfl_codec_init`'s `cdfl->swap_endian = native_endian`).
 * `block_size` should be computed the same way as `cdfl_codec_blocksize()` in libchdr_chd.c.
 * Writes exactly `num_samples` interleaved stereo `int16_t` sample-frames to `dest`.
 * On success, `*consumed_out` receives the number of bytes of `src` the FLAC bitstream actually
 * used, so the caller can locate the trailing zlib-compressed subcode stream (if any) at
 * `src + *consumed_out`. Returns 0 on success, negative on failure. */
int romexplorer_flac_decode_cd(
    const unsigned char *src, unsigned src_len,
    int16_t *dest, unsigned num_samples, unsigned block_size,
    unsigned *consumed_out)
{
    flac_decoder decoder;
    uint16_t native_endian = 0;
    *(uint8_t *)(&native_endian) = 1;
    int swap_endian = (native_endian & 1);

    if (flac_decoder_init(&decoder))
        return -2;
    if (!flac_decoder_reset(&decoder, 44100, 2, block_size, src, src_len)) {
        flac_decoder_free(&decoder);
        return -3;
    }
    if (!flac_decoder_decode_interleaved(&decoder, dest, num_samples, swap_endian)) {
        flac_decoder_free(&decoder);
        return -4;
    }
    *consumed_out = flac_decoder_finish(&decoder);
    return 0;
}
