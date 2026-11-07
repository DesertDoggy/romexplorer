/* rchip_*: sound chips from libvgm (external/libvgm), one register write at a time, for
 * consoles whose game code the app runs itself (the Master System and Game Gear's PSG and
 * FM unit, and more as they are added). The app hands every write over with its time in the
 * frame; this renders the chips up to that moment first, so notes start where the game
 * started them, and mixes the chips to stereo at the requested rate.
 *
 * Only the sound cores named in deps/rommusic/CMakeLists.txt are built. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <new>
#include <vector>

#include "stdtype.h"
#include "emu/EmuStructs.h"
#include "emu/SoundEmu.h"
#include "emu/SoundDevs.h"
#include "emu/Resampler.h"
#include "emu/cores/sn764intf.h"

#if defined(_WIN32)
#define RCHIP_API extern "C" __declspec(dllexport)
#else
#define RCHIP_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

/* What rchip_add takes as `variant` for the SN76496 family. */
enum {
    PSG_SEGA = 0,     /* Master System, Mega Drive */
    PSG_GAME_GEAR = 1, /* with the Game Gear's stereo register */
    PSG_SN76489 = 2,  /* ColecoVision, SG-1000 */
};

struct Chip {
    DEV_INFO dev;
    RESMPL_STATE rs;
    DEVFUNC_WRITE_A8D8 write;
};

struct Host {
    std::vector<Chip *> chips;
    UINT32 rate;
    double clock; /* write times count these a second */
    /* Samples rendered in this frame so far, and the part of a sample the last frame ended
     * on (so frames that aren't a whole number of samples don't drift). */
    UINT32 done;
    double carry;
    std::vector<WAVE_32BS> mix;
    std::vector<INT16> out;
};

/* Each chip's level in the mix, as libvgm's VGM player sets it (0x100 = 1.0). */
UINT16 volume_of(UINT8 dev) {
    switch (dev) {
    case DEVID_SN76496:
        return 0x80;
    case DEVID_YM2413:
        return 0x200;
    default:
        return 0x100;
    }
}

void render_to(Host &h, UINT32 upto) {
    if (upto <= h.done)
        return;
    UINT32 n = upto - h.done;
    size_t at = h.mix.size();
    h.mix.resize(at + n);
    memset(&h.mix[at], 0, n * sizeof(WAVE_32BS));
    for (Chip *c : h.chips)
        Resmpl_Execute(&c->rs, n, &h.mix[at]);
    h.done = upto;
}

double position(const Host &h, int time) {
    return (double)time * h.rate / h.clock + h.carry;
}

UINT32 sample_at(const Host &h, int time) {
    if (time <= 0)
        return h.done;
    return (UINT32)position(h, time);
}

} // namespace

/* A set of chips rendering stereo at `rate`; write times count `clock` a second. */
RCHIP_API void *rchip_new(int rate, double clock) {
    Host *h = new (std::nothrow) Host;
    if (!h)
        return NULL;
    h->rate = (UINT32)rate;
    h->clock = clock;
    h->done = 0;
    h->carry = 0;
    return h;
}

RCHIP_API void rchip_delete(void *handle) {
    Host *h = (Host *)handle;
    for (Chip *c : h->chips) {
        Resmpl_Deinit(&c->rs);
        SndEmu_Stop(&c->dev);
        delete c;
    }
    delete h;
}

/* Adds chip `dev` (a libvgm DEVID_) clocked at `clock`. Returns its number, or -1. */
RCHIP_API int rchip_add(void *handle, int dev, int clock, int variant) {
    Host &h = *(Host *)handle;
    Chip *c = new (std::nothrow) Chip;
    if (!c)
        return -1;
    memset(c, 0, sizeof(*c));
    UINT8 err;
    if (dev == DEVID_SN76496) {
        SN76496_CFG cfg;
        memset(&cfg, 0, sizeof(cfg));
        cfg._genCfg.clock = (UINT32)clock;
        cfg._genCfg.srMode = DEVRI_SRMODE_NATIVE;
        cfg._genCfg.smplRate = h.rate;
        cfg.clkDiv = 8;
        if (variant == PSG_SN76489) {
            cfg.noiseTaps = 0x03;
            cfg.shiftRegWidth = 15;
            cfg.negate = 1;
        } else {
            cfg.noiseTaps = 0x09;
            cfg.shiftRegWidth = 16;
            cfg.negate = 1;
            cfg.segaPSG = 1;
            cfg.stereo = variant == PSG_GAME_GEAR;
        }
        err = SndEmu_Start((DEV_ID)dev, (DEV_GEN_CFG *)&cfg, &c->dev);
    } else {
        DEV_GEN_CFG cfg;
        memset(&cfg, 0, sizeof(cfg));
        cfg.clock = (UINT32)clock;
        cfg.srMode = DEVRI_SRMODE_NATIVE;
        cfg.smplRate = h.rate;
        err = SndEmu_Start((DEV_ID)dev, &cfg, &c->dev);
    }
    if (err) {
        delete c;
        return -1;
    }
    SndEmu_FreeDevLinkData(&c->dev);
    if (SndEmu_GetDeviceFunc(c->dev.devDef, RWF_REGISTER | RWF_WRITE, DEVRW_A8D8, 0, (void **)&c->write)) {
        SndEmu_Stop(&c->dev);
        delete c;
        return -1;
    }
    c->dev.devDef->Reset(c->dev.dataPtr);
    Resmpl_SetVals(&c->rs, RSMODE_LINEAR, volume_of((UINT8)dev), h.rate);
    Resmpl_DevConnect(&c->rs, &c->dev);
    Resmpl_Init(&c->rs);
    h.chips.push_back(c);
    return (int)h.chips.size() - 1;
}

/* A write to chip `chip` at `time` into the frame: `port` is the chip's own register port
 * (the SN76496's 0 for data, 1 for the Game Gear stereo register; the YM2413's 0 for the
 * address, 1 for data). */
RCHIP_API void rchip_write(void *handle, int time, int chip, int port, int value) {
    Host &h = *(Host *)handle;
    if (chip < 0 || (size_t)chip >= h.chips.size())
        return;
    render_to(h, sample_at(h, time));
    Chip *c = h.chips[chip];
    c->write(c->dev.dataPtr, (UINT8)port, (UINT8)value);
}

/* Ends the frame at `time`. Returns the samples (left and right counted separately) now
 * waiting. */
RCHIP_API long rchip_end_frame(void *handle, int time) {
    Host &h = *(Host *)handle;
    double end = position(h, time);
    render_to(h, (UINT32)end);
    h.carry = end - (double)h.done;
    h.done = 0;
    for (const WAVE_32BS &s : h.mix) {
        INT32 l = s.L >> 8, r = s.R >> 8;
        h.out.push_back((INT16)(l < -32768 ? -32768 : l > 32767 ? 32767 : l));
        h.out.push_back((INT16)(r < -32768 ? -32768 : r > 32767 ? 32767 : r));
    }
    h.mix.clear();
    return (long)h.out.size();
}

/* Reads up to `max` waiting samples (interleaved stereo; `max` even) into `out`. */
RCHIP_API long rchip_read(void *handle, short *out, long max) {
    Host &h = *(Host *)handle;
    long n = (long)h.out.size() < max ? (long)h.out.size() : max;
    memcpy(out, h.out.data(), n * sizeof(short));
    h.out.erase(h.out.begin(), h.out.begin() + n);
    return n;
}
