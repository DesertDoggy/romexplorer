// rspc: blargg's SNES sound chip emulator (Snes_Spc, from upstream libgme in
// external/game-music-emu, LGPL-2.1) behind a flat C API, for driving it the way the SNES does:
// through its four I/O ports, in time. The app runs a game's main CPU against it to record what
// the game uploads (driver, songs, samples), and plays rebuilt sound memory by sending a song
// number through the ports.
//
// Time is in sound-chip clocks (1,024,000 a second) since the last rspc_end_frame.
#include "Snes_Spc.h"

#include <cstring>
#include <new>
#include <vector>

#if defined(_WIN32)
#define RSPC_API extern "C" __declspec(dllexport)
#else
#define RSPC_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

// The sound chip's 64-byte boot ROM. Snes_Spc leaves it to the caller; without it the chip
// cannot take an upload.
const uint8_t IPL_ROM[64] = {
    0xCD, 0xEF, 0xBD, 0xE8, 0x00, 0xC6, 0x1D, 0xD0, 0xFC, 0x8F, 0xAA, 0xF4, 0x8F, 0xBB, 0xF5, 0x78,
    0xCC, 0xF4, 0xD0, 0xFB, 0x2F, 0x19, 0xEB, 0xF4, 0xD0, 0xFC, 0x7E, 0xF4, 0xD0, 0x0B, 0xE4, 0xF5,
    0xCB, 0xF4, 0xD7, 0x00, 0xFC, 0xD0, 0xF3, 0xAB, 0x01, 0x10, 0xEF, 0x7E, 0xF4, 0x10, 0xEB, 0xBA,
    0xF6, 0xDA, 0x00, 0xBA, 0xF4, 0xC4, 0xF4, 0xDD, 0x5D, 0xD0, 0xDB, 0x1F, 0x00, 0x00, 0xC0, 0xFF,
};

struct Handle
{
    Snes_Spc spc;
    // Samples of the frame that ended last; a frame is at most a few thousand clocks of
    // output at 32 kHz.
    std::vector<short> out = std::vector<short>(1 << 16);
};

}    // namespace

/// A powered-on sound chip running its boot ROM, ready for an upload. Null if out of memory.
RSPC_API void *rspc_new()
{
    auto *h = new (std::nothrow) Handle;
    if (!h)
        return nullptr;
    if (h->spc.init()) {
        delete h;
        return nullptr;
    }
    h->spc.init_rom(IPL_ROM);
    h->spc.reset();
    h->spc.set_output(h->out.data(), static_cast<int>(h->out.size()));
    return h;
}

RSPC_API void rspc_delete(void *handle)
{
    delete static_cast<Handle *>(handle);
}

/// Loads an .spc snapshot (sound memory, registers, CPU state). 0 on success.
RSPC_API int rspc_load_spc(void *handle, const void *data, long size)
{
    auto &h = *static_cast<Handle *>(handle);
    if (h.spc.load_spc(data, size))
        return 1;
    h.spc.clear_echo();
    h.spc.set_output(h.out.data(), static_cast<int>(h.out.size()));
    return 0;
}

RSPC_API int rspc_read_port(void *handle, int time, int port)
{
    return static_cast<Handle *>(handle)->spc.read_port(time, port & 3);
}

RSPC_API void rspc_write_port(void *handle, int time, int port, int data)
{
    static_cast<Handle *>(handle)->spc.write_port(time, port & 3, data & 0xFF);
}

/// Runs the chip to `time` and starts a new frame at 0. Returns how many samples (both
/// channels, 32 kHz) the frame produced; rspc_samples points at them until the next call.
RSPC_API int rspc_end_frame(void *handle, int time)
{
    auto &h = *static_cast<Handle *>(handle);
    h.spc.end_frame(time);
    const int n = h.spc.sample_count();
    h.spc.set_output(h.out.data(), static_cast<int>(h.out.size()));
    return n;
}

RSPC_API const short *rspc_samples(void *handle)
{
    return static_cast<Handle *>(handle)->out.data();
}

/// Plays `count` samples (both channels, 32 kHz) with nothing written to the ports.
RSPC_API int rspc_play(void *handle, int count, short *out)
{
    auto &h = *static_cast<Handle *>(handle);
    const int r = h.spc.play(count & ~1, out) ? 1 : 0;
    h.spc.set_output(h.out.data(), static_cast<int>(h.out.size()));
    return r;
}

/// The chip's 64 KB of sound memory, live (the RAM under the boot ROM, not the ROM).
RSPC_API uint8_t *rspc_ram(void *handle)
{
    return static_cast<Handle *>(handle)->spc.smp_ram();
}

/// The sound CPU's program counter: where its code is running now.
RSPC_API int rspc_pc(void *handle)
{
    return static_cast<Handle *>(handle)->spc.smp_regs().pc;
}
