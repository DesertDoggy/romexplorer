/* rusf_*: N64 music from USF rips through lazyusf2 (external/lazyusf2, GPL-2.0+).
 *
 * A USF is a PSF-family file: its reserved sections hold the parts of the game's ROM and a
 * save state the music needs. The Rust side reads the files and their _lib chain and hands
 * the sections over in psf_load's order. */
#include <stdlib.h>
#include <string.h>

#include "usf/usf.h"

#if defined(_WIN32)
#define RUSF_API __declspec(dllexport)
#else
#define RUSF_API __attribute__((visibility("default")))
#endif

RUSF_API void *rusf_new(void) {
    void *state = calloc(1, usf_get_state_size());
    if (state) {
        usf_clear(state);
    }
    return state;
}

/* One reserved section, deepest _lib first. 0 on success, -1 on bad data. */
RUSF_API int rusf_upload(void *state, const uint8_t *data, size_t size) {
    return usf_upload_section(state, data, size);
}

/* The _enablecompare and _enablefifofull tags, and whether to use the faster high-level
 * audio (some games need the low-level path to sound right). */
RUSF_API void rusf_set_flags(void *state, int compare, int fifo_full, int hle_audio) {
    usf_set_compare(state, compare);
    usf_set_fifo_full(state, fifo_full);
    usf_set_hle_audio(state, hle_audio);
}

/* Renders `frames` stereo frames resampled to `rate` into `out` (interleaved). NULL on
 * success, else the error, copied into `err`. */
RUSF_API int rusf_render(void *state, int16_t *out, size_t frames, int32_t rate, char *err, size_t err_len) {
    const char *e = usf_render_resampled(state, out, frames, rate);
    if (!e) {
        return 0;
    }
    if (err && err_len) {
        strncpy(err, e, err_len - 1);
        err[err_len - 1] = 0;
    }
    return -1;
}

RUSF_API void rusf_free(void *state) {
    if (state) {
        usf_shutdown(state);
        free(state);
    }
}
