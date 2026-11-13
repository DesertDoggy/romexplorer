/* rgsf_*: GBA music from GSF rips through mGBA (external/mgba, MPL-2.0), the way the GSF
 * players of today drive it: the rip is a cut-down cartridge image, booted without a BIOS
 * and run frame by frame while its sound is taken.
 *
 * The Rust side reads the _lib chain and lays every section at its offset in one image. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <mgba/core/core.h>
#include <mgba/core/log.h>
#include <mgba/gba/core.h>
#include <mgba-util/audio-buffer.h>
#include <mgba-util/audio-resampler.h>
#include <mgba-util/vfs.h>

#if defined(_WIN32)
#define RGSF_API __declspec(dllexport)
#else
#define RGSF_API __attribute__((visibility("default")))
#endif

#define RGSF_RATE 44100

struct rgsf {
    struct mCore *core;
    struct mAudioResampler resampler;
    struct mAudioBuffer out;
};

static void quiet(struct mLogger *logger, int category, enum mLogLevel level, const char *format, va_list args) {
    (void)logger, (void)category, (void)level, (void)format, (void)args;
}
static struct mLogger silent = { .log = quiet };

/* A console running the cartridge image `rom` (copied). NULL when mGBA will not take it. */
RGSF_API void *rgsf_new(const uint8_t *rom, size_t size) {
    struct rgsf *g;
    struct VFile *vf;
    struct mCoreOptions opts = { 0 };
    mLogSetDefaultLogger(&silent);
    g = (struct rgsf *)calloc(1, sizeof *g);
    if (!g) {
        return NULL;
    }
    g->core = GBACoreCreate();
    if (!g->core || !g->core->init(g->core)) {
        free(g);
        return NULL;
    }
    mCoreInitConfig(g->core, NULL);
    opts.skipBios = true;
    opts.useBios = false;
    opts.sampleRate = RGSF_RATE;
    mCoreConfigLoadDefaults(&g->core->config, &opts);
    vf = VFileMemChunk(rom, size);
    if (!vf || !g->core->loadROM(g->core, vf)) {
        if (vf) {
            vf->close(vf);
        }
        mCoreConfigDeinit(&g->core->config);
        g->core->deinit(g->core);
        free(g);
        return NULL;
    }
    g->core->reset(g->core);
    mAudioBufferInit(&g->out, 0x4000, 2);
    mAudioResamplerInit(&g->resampler, mINTERPOLATOR_SINC);
    mAudioResamplerSetDestination(&g->resampler, &g->out, RGSF_RATE);
    return g;
}

/* Fills `frames` stereo frames at 44100 Hz. 0 on success, -1 when a minute of the game's
 * frames gave no sound at all. */
RGSF_API int rgsf_render(void *handle, int16_t *out, size_t frames) {
    struct rgsf *g = (struct rgsf *)handle;
    int idle = 0;
    while (frames > 0) {
        size_t got = mAudioBufferRead(&g->out, out, frames);
        if (got) {
            out += got * 2;
            frames -= got;
            idle = 0;
            continue;
        }
        if (++idle > 3600) {
            return -1;
        }
        g->core->runFrame(g->core);
        mAudioResamplerSetSource(&g->resampler, g->core->getAudioBuffer(g->core), g->core->audioSampleRate(g->core), true);
        mAudioResamplerProcess(&g->resampler);
    }
    return 0;
}

RGSF_API void rgsf_free(void *handle) {
    struct rgsf *g = (struct rgsf *)handle;
    if (!g) {
        return;
    }
    mAudioResamplerDeinit(&g->resampler);
    mAudioBufferDeinit(&g->out);
    mCoreConfigDeinit(&g->core->config);
    g->core->deinit(g->core);
    free(g);
}
