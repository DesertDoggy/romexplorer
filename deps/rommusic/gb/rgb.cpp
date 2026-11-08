// rgb: the Game Boy sound chip (Gb_Apu, from upstream libgme in external/game-music-emu,
// LGPL-2.1+) behind a flat C API. The app runs the game's own code on its Game Boy CPU and
// hands every write to $FF10-$FF3F here, in time; this turns them into stereo samples.
//
// Time is in CPU clocks (4,194,304 a second, single speed) since the last rgb_end_frame.
#include <cstdint>
#include <new>

#include "Gb_Apu.h"
#include "Multi_Buffer.h"

#if defined(_WIN32)
#define RGB_API extern "C" __declspec(dllexport)
#else
#define RGB_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

const long CLOCK = 4194304;
// What libgme's GBS player sets: its gain, and its equalizer (treble -1 dB, bass from 120 Hz).
const double GAIN = 1.2;
const double TREBLE_DB = -1.0;
const int BASS_HZ = 120;

struct Handle
{
    Stereo_Buffer buf;
    Gb_Apu apu;
};

}    // namespace

/// A powered-on Game Boy sound chip rendering stereo at `rate`. Null if out of memory.
RGB_API void *rgb_new(int rate)
{
    auto *h = new (std::nothrow) Handle;
    if (!h)
        return nullptr;
    if (h->buf.set_sample_rate(rate, 1000)) {
        delete h;
        return nullptr;
    }
    h->buf.clock_rate(CLOCK);
    h->buf.bass_freq(BASS_HZ);
    h->apu.output(h->buf.center(), h->buf.left(), h->buf.right());
    h->apu.volume(GAIN);
    h->apu.treble_eq(blip_eq_t(TREBLE_DB));
    h->apu.reset();
    return h;
}

RGB_API void rgb_delete(void *handle)
{
    delete static_cast<Handle *>(handle);
}

/// A write to $FF10-$FF3F at CPU clock `time`.
RGB_API void rgb_write(void *handle, int time, int addr, int data)
{
    auto &h = *static_cast<Handle *>(handle);
    if (addr >= 0xFF10 && addr <= 0xFF3F)
        h.apu.write_register(time, addr, data);
}

/// A read of $FF10-$FF3F at CPU clock `time` (the status bits the game polls).
RGB_API int rgb_read(void *handle, int time, int addr)
{
    auto &h = *static_cast<Handle *>(handle);
    if (addr >= 0xFF10 && addr <= 0xFF3F)
        return h.apu.read_register(time, addr);
    return 0xFF;
}

/// Ends the frame at CPU clock `time`. Returns the samples (left and right counted
/// separately) now waiting.
RGB_API long rgb_end_frame(void *handle, int time)
{
    auto &h = *static_cast<Handle *>(handle);
    h.apu.end_frame(time);
    h.buf.end_frame(time);
    return h.buf.samples_avail();
}

/// Reads up to `max` waiting samples (interleaved stereo; `max` even) into `out`.
RGB_API long rgb_read_samples(void *handle, short *out, long max)
{
    return static_cast<Handle *>(handle)->buf.read_samples(out, max);
}
