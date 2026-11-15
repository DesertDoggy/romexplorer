/* PC-98 music through 98fmplayer (external/98fmplayer, BSD-2-Clause): PMD and FMP songs
 * played by its C ports of the two drivers on its own OPNA (YM2608) emulation.
 *
 * The caller reads the files: the song, then each PCM file the song names (asked for with
 * rpc98_pcm_name and handed over with rpc98_pcm_load), and the YM2608's rhythm ROM from the
 * user's own files when there is one (without it the rhythm part is silent). Output is
 * stereo at RPC98_RATE, resampled from the chip's own 55467 Hz. */

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "fmdriver/fmdriver.h"
#include "fmdriver/fmdriver_fmp.h"
#include "fmdriver/fmdriver_pmd.h"
#include "fmdriver/ppz8.h"
#include "libopna/opna.h"
#include "libopna/opnatimer.h"
#include "libopna/opnadrum.h"

#if defined(_WIN32)
#define RPC98_API __declspec(dllexport)
#else
#define RPC98_API __attribute__((visibility("default")))
#endif

#define RPC98_RATE 44100
#define CHIP_RATE 55467
#define PPZ8_MIX 0xa000
#define CHUNK 1024

struct rpc98 {
    int fmp;
    union {
        struct driver_pmd pmd;
        struct driver_fmp fmp;
    } driver;
    uint8_t *song;
    struct fmdriver_work work;
    struct opna opna;
    struct opna_timer timer;
    struct ppz8 ppz8;
    uint8_t adpcm_ram[256 * 1024];
    uint8_t drums[OPNA_ROM_SIZE];
    int16_t *ppzbuf[2];
    /* The chip's output not yet resampled, and where the next output sample falls in it. */
    int16_t chip[(CHUNK + 2) * 2];
    unsigned chip_len;
    double pos;
};

static void opna_writereg_cb(struct fmdriver_work *work, unsigned addr, unsigned data) {
    opna_timer_writereg((struct opna_timer *)work->opna, addr, data);
}

static unsigned opna_readreg_cb(struct fmdriver_work *work, unsigned addr) {
    return opna_readreg(((struct opna_timer *)work->opna)->opna, addr);
}

static uint8_t opna_status_cb(struct fmdriver_work *work, bool a1) {
    uint8_t status = opna_timer_status((struct opna_timer *)work->opna);
    return a1 ? status : (status & 0x83);
}

static void int_cb(void *user) {
    struct fmdriver_work *work = (struct fmdriver_work *)user;
    work->driver_opna_interrupt(work);
}

static void mix_cb(void *user, int16_t *buf, unsigned samples) {
    ppz8_mix((struct ppz8 *)user, buf, samples);
}

/* A song from its file's bytes (copied). NULL when it is neither PMD nor FMP. */
RPC98_API void *rpc98_new(const uint8_t *data, size_t size) {
    struct rpc98 *p;
    if (!data || size < 2 || size > 0xffff) {
        return NULL;
    }
    p = (struct rpc98 *)calloc(1, sizeof *p);
    if (!p) {
        return NULL;
    }
    p->song = (uint8_t *)malloc(size);
    if (!p->song) {
        free(p);
        return NULL;
    }
    memcpy(p->song, data, size);
    if (!pmd_load(&p->driver.pmd, p->song, (uint16_t)size)) {
        memset(&p->driver, 0, sizeof p->driver);
        memcpy(p->song, data, size);
        if (!fmp_load(&p->driver.fmp, p->song, (uint16_t)size)) {
            free(p->song);
            free(p);
            return NULL;
        }
        p->fmp = 1;
    }
    opna_reset(&p->opna);
    opna_adpcm_set_ram_256k(&p->opna.adpcm, p->adpcm_ram);
    opna_timer_reset(&p->timer, &p->opna);
    ppz8_init(&p->ppz8, CHIP_RATE, PPZ8_MIX);
    p->work.opna_writereg = opna_writereg_cb;
    p->work.opna_readreg = opna_readreg_cb;
    p->work.opna_status = opna_status_cb;
    p->work.opna = &p->timer;
    p->work.ppz8 = &p->ppz8;
    p->work.ppz8_functbl = &ppz8_functbl;
    opna_timer_set_int_callback(&p->timer, int_cb, &p->work);
    opna_timer_set_mix_callback(&p->timer, mix_cb, &p->ppz8);
    if (p->fmp) {
        fmp_init(&p->work, &p->driver.fmp);
    } else {
        pmd_init(&p->work, &p->driver.pmd);
    }
    return p;
}

/* The YM2608 rhythm ROM (8 KB). 0 on success. */
RPC98_API int rpc98_set_drums(void *handle, const uint8_t *rom, size_t size) {
    struct rpc98 *p = (struct rpc98 *)handle;
    if (!p || size != OPNA_ROM_SIZE) {
        return -1;
    }
    memcpy(p->drums, rom, size);
    opna_drum_set_rom(&p->opna.drum, p->drums);
    return 0;
}

/* The PCM file the song names in slot `slot` (0-2), with the extensions to try in order
 * (".PPC", ".PVI.PZI", ...); 0 when the slot is empty. */
RPC98_API int rpc98_pcm_name(void *handle, int slot, char *name, size_t name_len, char *exts, size_t exts_len) {
    struct rpc98 *p = (struct rpc98 *)handle;
    const char *n = "", *e = "";
    if (!p || !name || !exts || name_len == 0 || exts_len == 0) {
        return 0;
    }
    if (p->fmp) {
        if (slot == 0) { n = p->driver.fmp.pvi_name; e = ".PVI"; }
        if (slot == 1) { n = p->driver.fmp.ppz_name; e = ".PVI"; }
    } else {
        if (slot == 0) { n = p->driver.pmd.ppcfile; e = ".PPC"; }
        if (slot == 1) { n = p->driver.pmd.ppzfile; e = ".PVI.PZI"; }
        if (slot == 2) { n = p->driver.pmd.ppzfile2; e = ".PVI.PZI"; }
    }
    if (!n[0]) {
        return 0;
    }
    strncpy(name, n, name_len - 1);
    name[name_len - 1] = 0;
    strncpy(exts, e, exts_len - 1);
    exts[exts_len - 1] = 0;
    return 1;
}

/* Hands over slot `slot`'s PCM file, read as `ext` (".PPC", ".PVI" or ".PZI"). 0 on success. */
RPC98_API int rpc98_pcm_load(void *handle, int slot, const char *ext, const uint8_t *data, size_t size) {
    struct rpc98 *p = (struct rpc98 *)handle;
    uint8_t *copy;
    int ok = 0;
    if (!p || !data || !ext) {
        return -1;
    }
    copy = (uint8_t *)malloc(size ? size : 1);
    if (!copy) {
        return -1;
    }
    memcpy(copy, data, size);
    if (p->fmp && slot == 0) {
        ok = fmp_adpcm_load(&p->work, copy, size);
    } else if (!p->fmp && slot == 0) {
        ok = pmd_ppc_load(&p->work, copy, size);
    } else if (slot == 1 || slot == 2) {
        int bank = p->fmp ? 0 : slot - 1;
        int pzi = !strcmp(ext, ".PZI");
        uint32_t samples = pzi ? ppz8_pzi_decodebuf_samples((uint32_t)size) : ppz8_pvi_decodebuf_samples((uint32_t)size);
        /* A file too short for its header is refused before the loader reads that header. */
        int16_t *buf = samples ? (int16_t *)calloc(samples, 2) : NULL;
        if (buf) {
            ok = pzi ? ppz8_pzi_load(&p->ppz8, (uint8_t)bank, copy, (uint32_t)size, buf) : ppz8_pvi_load(&p->ppz8, (uint8_t)bank, copy, (uint32_t)size, buf);
            if (ok) {
                free(p->ppzbuf[bank]);
                p->ppzbuf[bank] = buf;
            } else {
                free(buf);
            }
        }
    }
    free(copy);
    return ok ? 0 : -1;
}

/* Fills `frames` stereo frames at RPC98_RATE. 0 on success, -1 on a bad handle. */
RPC98_API int rpc98_render(void *handle, int16_t *out, size_t frames) {
    struct rpc98 *p = (struct rpc98 *)handle;
    const double step = (double)CHIP_RATE / RPC98_RATE;
    if (!p || !out) {
        return -1;
    }
    while (frames > 0) {
        unsigned i = (unsigned)p->pos;
        if (i + 1 >= p->chip_len) {
            /* Keep the last frame for interpolating into the next chunk. */
            int16_t last[2] = { 0, 0 };
            if (p->chip_len > 0) {
                last[0] = p->chip[(p->chip_len - 1) * 2];
                last[1] = p->chip[(p->chip_len - 1) * 2 + 1];
                p->pos -= p->chip_len - 1;
            } else {
                p->pos = 0;
            }
            memset(p->chip, 0, sizeof p->chip);
            p->chip[0] = last[0];
            p->chip[1] = last[1];
            opna_timer_mix(&p->timer, p->chip + 2, CHUNK);
            p->chip_len = CHUNK + 1;
            continue;
        }
        {
            double f = p->pos - i;
            int c;
            for (c = 0; c < 2; c++) {
                double a = p->chip[i * 2 + c], b = p->chip[(i + 1) * 2 + c];
                *out++ = (int16_t)(a + (b - a) * f);
            }
        }
        p->pos += step;
        frames--;
    }
    return 0;
}

/* How far the song is: its loop count so far, whether it still plays (an unlooped song
 * stops), and its loop length in timer-B ticks (0 when it has no loop). */
RPC98_API void rpc98_status(void *handle, uint32_t *loops, int *playing, uint32_t *ticks) {
    struct rpc98 *p = (struct rpc98 *)handle;
    if (!p) {
        return;
    }
    if (loops) *loops = p->work.loop_cnt;
    if (playing) *playing = p->work.playing;
    if (ticks) *ticks = p->work.timerb_cnt;
}

/* A line of the song's text (CP932): PMD's #Title, #Composer, #Arranger, memo lines; FMP's
 * three comment lines. NULL past the last. */
RPC98_API const char *rpc98_comment(void *handle, int line) {
    struct rpc98 *p = (struct rpc98 *)handle;
    if (!p || !p->work.get_comment || line < 0) {
        return NULL;
    }
    if (!p->work.comment_mode_pmd && line >= 3) {
        return NULL;
    }
    return p->work.get_comment(&p->work, line);
}

RPC98_API void rpc98_free(void *handle) {
    struct rpc98 *p = (struct rpc98 *)handle;
    if (!p) {
        return;
    }
    free(p->ppzbuf[0]);
    free(p->ppzbuf[1]);
    free(p->song);
    free(p);
}
