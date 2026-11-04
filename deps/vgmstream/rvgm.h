/* rvgm: a flat C API over libvgmstream, for romexplorer.
 *
 * libvgmstream's own API (external/vgmstream/src/libvgmstream.h) hands back structs whose
 * layout the caller has to mirror, and reads its input through a `libstreamfile_t` of
 * function pointers taking `void *` user data. This keeps all of that on the C side: the
 * Rust side (src/rom/vgmstream.rs) passes three callbacks and gets numbers, short strings
 * and 16-bit PCM back.
 *
 * Input is never a path: every read goes through the caller's `read` callback, so a track
 * can be decoded straight out of a CHD, an RVZ, an ISO or a cached archive member. A format
 * that needs a companion file (a `.sd` beside its `.hd`, a `.awb` beside its `.acb`) asks
 * for it by name through `open`.
 *
 * One handle decodes one stream. Not thread-safe per handle; separate handles are
 * independent.
 */
#ifndef RVGM_H
#define RVGM_H

#include <stdint.h>

#if defined(_WIN32)
#define RVGM_API __declspec(dllexport)
#else
#define RVGM_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped whenever a signature or struct below changes, so a stale library is refused. */
#define RVGM_ABI_VERSION 1
RVGM_API int32_t rvgm_abi_version(void);

/* The caller's input. `user` is opaque here and handed back to every callback.
 *   read   copies up to `len` bytes at `offset` into `dst`; returns the count, 0 at the end
 *          or on failure.
 *   open   opens a file next to this one by name (vgmstream passes the full name it built
 *          from rvgm_open's `name`). Returns new user data and its size in *size, or NULL
 *          when there is no such file. It is also how vgmstream reopens the same file.
 *   close  releases user data returned by `open` (never the one passed to rvgm_open). */
typedef struct {
    int32_t (*read)(void *user, uint8_t *dst, int64_t offset, int32_t len);
    void *(*open)(void *user, const char *name, int64_t *size);
    void (*close)(void *user);
} rvgm_io;

/* What a stream is. Strings are UTF-8 and always terminated. */
typedef struct {
    int32_t channels;
    int32_t sample_rate;
    int32_t subsong_index;   /* 1-based; 0 when the format has no subsongs */
    int32_t subsong_count;   /* 0 when the format has no subsongs */
    int64_t stream_samples;  /* the stream's length, one pass, per channel */
    int64_t loop_start;
    int64_t loop_end;
    int32_t loop_flag;
    int64_t play_samples;    /* what rvgm_render will produce with the open's loop settings */
    int32_t bitrate;         /* bits per second of the stored stream, 0 when unknown */
    char codec[128];
    char meta[128];
    char stream_name[256];
} rvgm_info;

typedef struct rvgm rvgm;

/* Opens subsong `subsong` (0 = the first) of the file `name`, `size` bytes, read through
 * `io` with `user`. `name` needs only to look like a file name: its extension is what
 * vgmstream identifies most formats by.
 *   loops         how many times a looping stream plays its loop (1 = once through)
 *   fade_seconds  fade-out after the last loop
 *   ignore_loop   1 = play a looping stream once, as stored, with no fade
 * Returns NULL when vgmstream does not recognise the file. */
RVGM_API rvgm *rvgm_open(const rvgm_io *io, void *user, int64_t size, const char *name, int32_t subsong, double loops, double fade_seconds, int32_t ignore_loop);

RVGM_API void rvgm_info_get(const rvgm *h, rvgm_info *out);

/* Decodes the next run of interleaved 16-bit samples. Returns samples per channel now at
 * *out (valid until the next call), 0 when the stream is done, or a negative error. */
RVGM_API int32_t rvgm_render(rvgm *h, const int16_t **out);

/* Seeks to a sample (per channel) from the start. */
RVGM_API void rvgm_seek(rvgm *h, int64_t sample);

RVGM_API void rvgm_close(rvgm *h);

/* 1 when vgmstream knows the file's extension. A cheap first filter before rvgm_open: it
 * reads no data. Common extensions (wav, ogg, mp3...) count as known. */
RVGM_API int32_t rvgm_known_extension(const char *name);

/* 1 when FFmpeg was linked in (WMA, XMA, ATRAC3...). Added after ABI 1 without a bump: a
 * caller treats a library without this symbol as 0. */
RVGM_API int32_t rvgm_has_ffmpeg(void);

#ifdef __cplusplus
}
#endif

#endif
