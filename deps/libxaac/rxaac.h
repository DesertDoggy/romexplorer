/* rxaac: a flat C API over libxaac's encoder, for romexplorer.
 *
 * libxaac's own API is three calls taking two large structs whose layout the caller has to
 * mirror, plus a third struct (the DRC configuration) that USAC mode reads and writes through
 * a pointer the caller must have allocated -- left NULL it crashes. This wraps all of that
 * so the Rust side (src/rom/xaac.rs) only passes numbers and byte buffers.
 *
 * One encoder handles one stream. Not thread-safe per handle; separate handles are
 * independent.
 */
#ifndef RXAAC_H
#define RXAAC_H

#include <stdint.h>

#if defined(_WIN32)
#define RXAAC_API __declspec(dllexport)
#else
#define RXAAC_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct rxaac rxaac;

/* Bumped whenever a signature below changes, so a stale library is refused, not misused. */
#define RXAAC_ABI_VERSION 1
RXAAC_API int32_t rxaac_abi_version(void);

/* Opens an encoder for 16-bit interleaved PCM.
 *   xhe      0 = AAC-LC, 1 = xHE-AAC (USAC, no SBR, frequency-domain coding).
 *   bitrate  bits per second for the whole stream.
 *   loudness the programme loudness in LKFS the stream declares (xHE-AAC only; a decoder
 *            turns the track up or down by the distance to its own target, so the value
 *            decides the playback level -- romexplorer passes its target to mean "leave
 *            the level alone").
 * Returns NULL when libxaac refuses the configuration; *error then holds its error code. */
RXAAC_API rxaac *rxaac_open(int32_t sample_rate, int32_t channels, int32_t bitrate, int32_t xhe, double loudness, int32_t *error);

/* Samples per channel one rxaac_encode call takes. */
RXAAC_API int32_t rxaac_frame_samples(const rxaac *enc);

/* The AudioSpecificConfig (what an MP4 `esds` box carries). Valid until rxaac_close. */
RXAAC_API const uint8_t *rxaac_config(const rxaac *enc, int32_t *len);

/* Encodes exactly rxaac_frame_samples() * channels samples. Returns the number of bytes of
 * one access unit now at *out (valid until the next call), 0 when the encoder is still
 * priming and has produced nothing, or a negative libxaac error code. */
RXAAC_API int32_t rxaac_encode(rxaac *enc, const int16_t *pcm, const uint8_t **out);

RXAAC_API void rxaac_close(rxaac *enc);

#ifdef __cplusplus
}
#endif

#endif
