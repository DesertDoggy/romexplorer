/* rwsr_*: WonderSwan music from WSR rips through Beetle WonderSwan
 * (external/beetle-wswan-libretro, Mednafen's WonderSwan core, GPL-2.0+).
 *
 * A WSR is a cut-down cartridge image with a "WSRF" header 32 bytes from its end (version,
 * first song); the console boots it with the song number in AX and SP at 0x2000, and its
 * code plays that song from the timer interrupts. The core keeps its state in globals, so
 * one player exists at a time: rwsr_new fails while another is open. */
#include <stdlib.h>
#include <string.h>

#include "mednafen/mednafen-types.h"
#include "mednafen/video.h"
#include "mednafen/mempatcher.h"
#include "mednafen/wswan/wswan.h"
#include "mednafen/wswan/gfx.h"
#include "mednafen/wswan/wswan-memory.h"
#include "mednafen/wswan/sound.h"
#include "mednafen/wswan/v30mz.h"
#include "mednafen/wswan/interrupt.h"
#include "mednafen/wswan/rtc.h"
#include "mednafen/wswan/eeprom.h"
#include "mednafen/wswan/comm.h"
#include "mednafen/wswan/start.inc"

#if defined(_WIN32)
#define RWSR_API __declspec(dllexport)
#else
#define RWSR_API __attribute__((visibility("default")))
#endif

#define RWSR_RATE 44100

/* What the core's frontend (libretro.c) defines, which is not built here. */
int wsc = 1;
uint32 rom_size;
uint16 WSButtonStatus;

/* Save states belong to the frontend; a player never makes one. */
struct StateMem;
int StateAction(struct StateMem *sm, int load, int data_only) {
    (void)sm, (void)load, (void)data_only;
    return 0;
}

static int open_count;
static uint16_t pixels[256 * 160];
static MDFN_Surface surface;
static int16 *sound;
static int32 sound_size;
/* Rendered but not yet handed out, interleaved stereo. */
static int16 *pending;
static size_t pending_frames, pending_at;

static uint32 next_pow2(uint32 v) {
    v--;
    v |= v >> 1; v |= v >> 2; v |= v >> 4; v |= v >> 8; v |= v >> 16;
    return v + 1;
}

/* Starts song `song` of the WSR `data` (copied). NULL when it is not a WSR, or while
 * another player is open. */
RWSR_API void *rwsr_new(const uint8_t *data, size_t size, int song) {
    uint32 real, i;
    if (open_count || size < 0x20 || size > 0x1000000 || memcmp(data + size - 0x20, "WSRF", 4)) {
        return NULL;
    }
    real = (uint32)((size + 0xFFFF) & ~0xFFFF);
    rom_size = next_pow2(real < 0x10000 ? 0x10000 : real);
    wsCartROM = (uint8 *)calloc(1, rom_size);
    if (!wsCartROM) {
        return NULL;
    }
    /* Laid at the top of the cartridge space, the gap below filled as unprogrammed ROM. */
    memset(wsCartROM, 0xFF, rom_size - real);
    memcpy(wsCartROM + (rom_size - real) + (real - size), data, size);
    eeprom_size = 0;
    MDFNMP_Init(16384, (1 << 20) / 1024);
    v30mz_init(WSwan_readmem20, WSwan_writemem20, WSwan_readport, WSwan_writeport);
    WSwan_MemoryInit(0, wsc, 0, false);
    WSwan_GfxInit();
    WSwan_SoundInit();
    WSwan_RTCInit();
    wsMakeTiles();
    WSwan_SetSoundRate(RWSR_RATE);

    v30mz_reset();
    WSwan_MemoryReset();
    Comm_Reset();
    WSwan_GfxReset();
    WSwan_SoundReset();
    WSwan_InterruptReset();
    WSwan_RTCReset();
    WSwan_EEPROMReset();
    for (i = 0; i < 0xc8; i++) {
        if (i != 0xC4 && i != 0xC5 && i != 0xBA && i != 0xBB) {
            WSwan_writeport(i, startio[i]);
        }
    }
    v30mz_set_reg(NEC_SS, 0);
    v30mz_set_reg(NEC_SP, 0x2000);
    v30mz_set_reg(NEC_AW, (unsigned)song);

    memset(&surface, 0, sizeof surface);
    surface.pixels = pixels;
    surface.width = 256;
    surface.height = 160;
    surface.pitch = 256;
    surface.depth = 16;
    pending_frames = pending_at = 0;
    open_count = 1;
    return &open_count;
}

/* The song number the WSR starts with (its header's "first song"). */
RWSR_API int rwsr_first_song(const uint8_t *data, size_t size) {
    return size >= 0x20 && !memcmp(data + size - 0x20, "WSRF", 4) ? data[size - 0x20 + 5] : -1;
}

/* Fills `frames` stereo frames at 44100 Hz. 0 on success, -1 on error. */
RWSR_API int rwsr_render(void *handle, int16_t *out, size_t frames) {
    int guard = 0;
    if (handle != &open_count || !open_count) {
        return -1;
    }
    while (frames > 0) {
        if (pending_at < pending_frames) {
            size_t take = pending_frames - pending_at;
            if (take > frames) {
                take = frames;
            }
            memcpy(out, pending + pending_at * 2, take * 2 * sizeof *out);
            out += take * 2;
            frames -= take;
            pending_at += take;
            continue;
        }
        if (++guard > 100000) {
            return -1;
        }
        /* One video frame: lines until the frame is done. */
        while (!wsExecuteLine(&surface, true)) {
        }
        if (!sound) {
            sound_size = 4096;
            sound = (int16 *)malloc(sound_size * sizeof *sound);
            if (!sound) {
                return -1;
            }
        }
        pending_frames = (size_t)WSwan_SoundFlush(&sound, &sound_size);
        pending = sound;
        pending_at = 0;
        v30mz_timestamp = 0;
    }
    return 0;
}

RWSR_API void rwsr_free(void *handle) {
    if (handle != &open_count || !open_count) {
        return;
    }
    WSwan_MemoryKill();
    WSwan_SoundKill();
    free(wsCartROM);
    wsCartROM = NULL;
    open_count = 0;
}
