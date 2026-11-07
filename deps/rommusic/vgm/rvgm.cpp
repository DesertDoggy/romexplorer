/* rvgm_*: VGM logs through libvgm (external/libvgm), for the chips gme does not have: the
 * Neo Geo Pocket's T6W28 (a dual SN76489 sharing its tone registers) and the Virtual
 * Boy's VSU.
 *
 * Only libvgm's VGM player and the sound cores named in deps/rommusic/CMakeLists.txt are
 * built: no file, zlib or audio-output code. The caller hands over the whole VGM,
 * unpacked (a .vgz is unpacked on the Rust side), and decides lengths and fades itself:
 * the song loops for ever here. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "stdtype.h"
#include "utils/DataLoader.h"
#include "player/playerbase.hpp"
#include "player/vgmplayer.hpp"
#include "player/playera.hpp"

#if defined(_WIN32)
#define RVGM_API extern "C" __declspec(dllexport)
#else
#define RVGM_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

const UINT32 RATE = 44100;
const UINT32 BUFFER_FRAMES = 2048;

/* A DATA_LOADER over memory the handle owns. */
struct Memory {
    const UINT8 *data;
    UINT32 size;
    UINT32 pos;
};

UINT8 m_open(void *c) {
    ((Memory *)c)->pos = 0;
    return 0;
}
UINT32 m_read(void *c, UINT8 *buf, UINT32 n) {
    Memory *m = (Memory *)c;
    if (m->pos >= m->size) {
        return 0;
    }
    if (n > m->size - m->pos) {
        n = m->size - m->pos;
    }
    memcpy(buf, m->data + m->pos, n);
    m->pos += n;
    return n;
}
UINT8 m_seek(void *c, UINT32 offset, UINT8 whence) {
    Memory *m = (Memory *)c;
    UINT32 base = whence == 1 ? m->pos : whence == 2 ? m->size : 0;
    m->pos = base + offset > m->size ? m->size : base + offset;
    return 0;
}
UINT8 m_close(void *) {
    return 0;
}
INT32 m_tell(void *c) {
    return (INT32)((Memory *)c)->pos;
}
UINT32 m_length(void *c) {
    return ((Memory *)c)->size;
}
UINT8 m_eof(void *c) {
    Memory *m = (Memory *)c;
    return m->pos >= m->size;
}

const DATA_LOADER_CALLBACKS MEMORY_CALLBACKS = {
    0x6D656D21, /* 'mem!' */
    "rommusic memory",
    m_open, m_read, m_seek, m_close, m_tell, m_length, m_eof, NULL,
};

struct Rvgm {
    UINT8 *data;
    Memory memory;
    DATA_LOADER loader;
    PlayerA player;
    bool started;
};

}  // namespace

/* A player over one VGM (unpacked). NULL when libvgm cannot read it. */
RVGM_API void *rvgm_new(const uint8_t *data, size_t size) {
    if (size < 0x40 || size > 0x7fffffff || memcmp(data, "Vgm ", 4) != 0) {
        return NULL;
    }
    Rvgm *p = new Rvgm();
    p->data = (UINT8 *)malloc(size);
    if (!p->data) {
        delete p;
        return NULL;
    }
    memcpy(p->data, data, size);
    p->memory.data = p->data;
    p->memory.size = (UINT32)size;
    p->memory.pos = 0;
    DataLoader_Setup(&p->loader, &MEMORY_CALLBACKS, &p->memory);
    p->started = false;
    if (DataLoader_Load(&p->loader)) {
        free(p->data);
        delete p;
        return NULL;
    }
    p->player.RegisterPlayerEngine(new VGMPlayer);
    if (p->player.SetOutputSettings(RATE, 2, 16, BUFFER_FRAMES)) {
        DataLoader_Reset(&p->loader);
        free(p->data);
        delete p;
        return NULL;
    }
    PlayerA::Config cfg = p->player.GetConfiguration();
    cfg.masterVol = 0x10000;
    cfg.loopCount = 0; /* for ever: the caller decides where it ends */
    cfg.fadeSmpls = 0;
    cfg.endSilenceSmpls = 0;
    cfg.pbSpeed = 1.0;
    p->player.SetConfiguration(cfg);
    if (p->player.LoadFile(&p->loader)) {
        p->player.UnregisterAllPlayers();
        DataLoader_Reset(&p->loader);
        free(p->data);
        delete p;
        return NULL;
    }
    return p;
}

/* One pass through the song and the part of it that repeats, in milliseconds (loop 0 when
 * it plays once and ends). */
RVGM_API void rvgm_length(void *handle, uint32_t *total_ms, uint32_t *loop_ms) {
    Rvgm *p = (Rvgm *)handle;
    PlayerBase *engine = p->player.GetPlayer();
    *total_ms = (uint32_t)(engine->Tick2Second(engine->GetTotalTicks()) * 1000.0 + 0.5);
    *loop_ms = (uint32_t)(engine->Tick2Second(engine->GetLoopTicks()) * 1000.0 + 0.5);
}

/* From the start again. */
RVGM_API int rvgm_start(void *handle) {
    Rvgm *p = (Rvgm *)handle;
    if (p->started) {
        p->player.Stop();
    }
    p->started = p->player.Start() == 0;
    return p->started ? 0 : -1;
}

/* Fills `frames` stereo frames at 44100 Hz. 0 while it plays, 1 once a song that does not
 * loop has ended (the rest is silence), -1 when not started. */
RVGM_API int rvgm_render(void *handle, int16_t *out, size_t frames) {
    Rvgm *p = (Rvgm *)handle;
    if (!p->started) {
        return -1;
    }
    while (frames > 0) {
        UINT32 n = frames > BUFFER_FRAMES ? BUFFER_FRAMES : (UINT32)frames;
        UINT32 got = p->player.Render(n * 4, out) / 4;
        if (got < n) {
            memset(out + (size_t)got * 2, 0, (size_t)(n - got) * 4);
        }
        out += (size_t)n * 2;
        frames -= n;
        if (p->player.GetState() & PLAYSTATE_END) {
            memset(out, 0, frames * 4);
            return 1;
        }
    }
    return 0;
}

RVGM_API void rvgm_free(void *handle) {
    Rvgm *p = (Rvgm *)handle;
    if (!p) {
        return;
    }
    if (p->started) {
        p->player.Stop();
    }
    p->player.UnloadFile();
    p->player.UnregisterAllPlayers();
    DataLoader_Reset(&p->loader);
    free(p->data);
    delete p;
}
