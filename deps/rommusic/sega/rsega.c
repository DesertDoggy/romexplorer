/* rsega_*: Saturn and Dreamcast music from SSF/DSF rips through Highly Theoretical
 * (external/highly_theoretical, GPL-3.0), with its C68K core (GPL-2.0+) for the Saturn's
 * sound CPU.
 *
 * Each file's program area is a load address and the bytes to put there in the sound
 * RAM; the Rust side reads the _lib chain and uploads them in psf_load's order. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "sega.h"

#if defined(_WIN32)
#define RSEGA_API __declspec(dllexport)
#else
#define RSEGA_API __attribute__((visibility("default")))
#endif

static int initialised;

/* A fresh sound system: version 1 (Saturn) or 2 (Dreamcast). NULL on failure. */
RSEGA_API void *rsega_new(int version) {
    void *state;
    if (version != 1 && version != 2) {
        return NULL;
    }
    if (!initialised) {
        if (sega_init() != 0) {
            return NULL;
        }
        initialised = 1;
    }
    state = malloc(sega_get_state_size((uint8)version));
    if (!state) {
        return NULL;
    }
    sega_clear_state(state, (uint8)version);
    /* What the SSF/DSF players default to: the dry signal and the effects DSP, interpreted. */
    sega_enable_dry(state, 1);
    sega_enable_dsp(state, 1);
    sega_enable_dsp_dynarec(state, 0);
    return state;
}

/* One program area (with its 4-byte load address), deepest _lib first. 0 or -1. */
RSEGA_API int rsega_upload(void *state, const uint8_t *program, size_t size) {
    if (size < 5 || size > 0x7fffffff) {
        return -1;
    }
    return sega_upload_program(state, (void *)program, (uint32)size) == 0 ? 0 : -1;
}

/* Fills `frames` stereo frames at 44100 Hz. 0 on success, -1 on an emulation error. */
RSEGA_API int rsega_render(void *state, int16_t *out, size_t frames) {
    int stalled = 0;
    while (frames > 0) {
        uint32 got = frames > 0x10000 ? 0x10000 : (uint32)frames;
        if (sega_execute(state, 0x7fffffff, out, &got) < 0) {
            return -1;
        }
        if (got == 0 && ++stalled > 1000) {
            return -1;
        }
        out += (size_t)got * 2;
        frames -= got;
    }
    return 0;
}

RSEGA_API void rsega_free(void *state) {
    free(state);
}
