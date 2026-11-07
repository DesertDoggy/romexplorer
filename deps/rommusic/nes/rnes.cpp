// rnes: the NES sound chip (2A03 APU) and the cartridge sound chips some Famicom games carry
// (Konami VRC6, Namco 163, Sunsoft 5B, Nintendo MMC5) and the Famicom Disk System's, from upstream libgme
// (external/game-music-emu, LGPL-2.1+), behind a flat C API. The app runs the game's own code
// on its 6502 and hands every write to a sound register here, in time; this turns them into
// samples. Nothing of an NSF player is used: the game's code is the player.
//
// Time is in CPU clocks since the last rnes_end_frame.
#include <cstdint>
#include <new>
#include <vector>

#include "Blip_Buffer.h"
#include "Nes_Apu.h"
#include "Nes_Fds_Apu.h"
#include "Nes_Fme7_Apu.h"
#include "Nes_Mmc5_Apu.h"
#include "Nes_Namco_Apu.h"
#include "Nes_Vrc6_Apu.h"

#if defined(_WIN32)
#define RNES_API extern "C" __declspec(dllexport)
#else
#define RNES_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

// The CPU clock: NTSC and PAL consoles.
const long NTSC_CLOCK = 1789773;
const long PAL_CLOCK = 1662607;

// What libgme's NSF player sets: its gain, and the NES equalizer (treble -1 dB at the top,
// bass rolled off below 80 Hz).
const double GAIN = 1.4;
const double TREBLE_DB = -1.0;
const int BASS_HZ = 80;

// Where a write goes. The app decodes the cartridge's address lines (VRC6 boards differ in
// them) and says which register it means.
enum Target {
    APU = 0,          // $4000-$4017: addr is the register
    VRC6 = 1,         // addr = osc * 4 + reg (osc 0-2, reg 0-2)
    N163_ADDR = 2,    // $F800
    N163_DATA = 3,    // $4800
    FME7_LATCH = 4,   // $C000
    FME7_DATA = 5,    // $E000
    MMC5 = 6,         // $5000-$5015: addr is the register
    FDS = 7,          // $4040-$4092: addr is the register
};

struct Handle
{
    Blip_Buffer buf;
    Nes_Apu apu;
    Nes_Vrc6_Apu vrc6;
    Nes_Namco_Apu n163;
    Nes_Fme7_Apu fme7;
    Nes_Mmc5_Apu mmc5;
    Nes_Fds_Apu fds;
    // Which cartridge chips the game has: bit per Target group (1 VRC6, 2 N163, 4 FME7, 8 MMC5,
    // 16 FDS).
    int chips = 0;
    int (*reader)(void *, unsigned) = nullptr;
    void *reader_data = nullptr;
};

int read_dmc(void *data, nes_addr_t addr)
{
    auto &h = *static_cast<Handle *>(data);
    return h.reader ? h.reader(h.reader_data, addr) & 0xFF : 0;
}

}    // namespace

/// A powered-on NES sound chip rendering mono samples at `rate`, with the cartridge chips in
/// `chips` (bits: 1 VRC6, 2 Namco 163, 4 Sunsoft 5B, 8 MMC5, 16 Disk System). Null if out of memory.
RNES_API void *rnes_new(int rate, int pal, int chips)
{
    auto *h = new (std::nothrow) Handle;
    if (!h)
        return nullptr;
    if (h->buf.set_sample_rate(rate, 1000)) {
        delete h;
        return nullptr;
    }
    h->buf.clock_rate(pal ? PAL_CLOCK : NTSC_CLOCK);
    h->buf.bass_freq(BASS_HZ);
    h->chips = chips;
    blip_eq_t eq(TREBLE_DB);
    // Each chip on the cartridge mixes in with the console's, a little quieter overall, as
    // libgme's NSF player does it.
    double gain = GAIN;
    if (chips & 1)
        gain *= 0.75;
    if (chips & 2)
        gain *= 0.75;
    if (chips & 4)
        gain *= 0.75;
    if (chips & 16)
        gain *= 0.75;
    h->apu.reset(pal != 0);
    h->apu.output(&h->buf);
    h->apu.volume(gain);
    h->apu.treble_eq(eq);
    h->apu.dmc_reader(read_dmc, h);
    h->vrc6.reset();
    h->vrc6.output(&h->buf);
    h->vrc6.volume(gain);
    h->vrc6.treble_eq(eq);
    h->n163.reset();
    h->n163.output(&h->buf);
    h->n163.volume(gain);
    h->n163.treble_eq(eq);
    h->fme7.reset();
    h->fme7.output(&h->buf);
    h->fme7.volume(gain);
    h->fme7.treble_eq(eq);
    h->mmc5.reset(pal != 0);
    h->mmc5.output(&h->buf);
    h->mmc5.volume(gain);
    h->mmc5.treble_eq(eq);
    h->fds.reset();
    h->fds.osc_output(0, &h->buf);
    h->fds.volume(gain);
    h->fds.treble_eq(eq);
    return h;
}

RNES_API void rnes_delete(void *handle)
{
    delete static_cast<Handle *>(handle);
}

/// How the DMC channel reads its samples from the CPU's memory: `reader(data, addr)`.
RNES_API void rnes_set_reader(void *handle, int (*reader)(void *, unsigned), void *data)
{
    auto &h = *static_cast<Handle *>(handle);
    h.reader = reader;
    h.reader_data = data;
}

/// A write to a sound register at CPU clock `time` (see Target).
RNES_API void rnes_write(void *handle, int time, int target, int addr, int data)
{
    auto &h = *static_cast<Handle *>(handle);
    switch (target) {
    case APU:
        if (addr >= 0x4000 && addr <= 0x4017 && addr != 0x4014 && addr != 0x4016)
            h.apu.write_register(time, addr, data);
        break;
    case VRC6:
        if (h.chips & 1)
            h.vrc6.write_osc(time, (addr >> 2) % 3, addr & 3, data);
        break;
    case N163_ADDR:
        if (h.chips & 2)
            h.n163.write_addr(data);
        break;
    case N163_DATA:
        if (h.chips & 2)
            h.n163.write_data(time, data);
        break;
    case FME7_LATCH:
        if (h.chips & 4)
            h.fme7.write_latch(data);
        break;
    case FME7_DATA:
        if (h.chips & 4)
            h.fme7.write_data(time, data);
        break;
    case MMC5:
        if (h.chips & 8)
            h.mmc5.write_register(time, addr, data);
        break;
    case FDS:
        if ((h.chips & 16) && addr >= 0x4040 && addr < 0x4040 + 0x53)
            h.fds.write(time, addr, data);
        break;
    default:
        break;
    }
}

/// $4015 as the game reads it at `time`.
RNES_API int rnes_read_status(void *handle, int time)
{
    return static_cast<Handle *>(handle)->apu.read_status(time);
}

/// $4800 (Namco 163 sound memory) as the game reads it.
RNES_API int rnes_read_n163(void *handle)
{
    return static_cast<Handle *>(handle)->n163.read_data();
}

/// When the chip next raises its frame interrupt, in CPU clocks from the frame's start, or a
/// large number when it won't.
RNES_API int rnes_irq_time(void *handle, int time)
{
    return static_cast<Handle *>(handle)->apu.earliest_irq(time);
}

/// Ends the frame at CPU clock `time` (the next frame starts at 0). Returns the samples now
/// waiting to be read.
RNES_API long rnes_end_frame(void *handle, int time)
{
    auto &h = *static_cast<Handle *>(handle);
    h.apu.end_frame(time);
    if (h.chips & 1)
        h.vrc6.end_frame(time);
    if (h.chips & 2)
        h.n163.end_frame(time);
    if (h.chips & 4)
        h.fme7.end_frame(time);
    if (h.chips & 8)
        h.mmc5.end_frame(time);
    if (h.chips & 16)
        h.fds.end_frame(time);
    h.buf.end_frame(time);
    return h.buf.samples_avail();
}

/// Reads up to `max` waiting mono samples into `out`. Returns how many.
RNES_API long rnes_read(void *handle, short *out, long max)
{
    return static_cast<Handle *>(handle)->buf.read_samples(out, max);
}
