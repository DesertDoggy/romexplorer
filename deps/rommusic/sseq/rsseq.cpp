// rsseq: a flat C API over kode54's SSEQ Player (external/SSEQPlayer, WTFPL; by CyberBotX,
// from fincs' FeOS Sound System), which plays the sequenced music of Nintendo DS games -- the
// SSEQ songs in a sound archive (SDAT), with their instrument banks and wave archives --
// straight from the archive, as the DS does.
//
//   rsseq_list     the archive's songs: id, name (from its symbol table, when it has one),
//                  bank, and volume
//   rsseq_open     one song, ready to play at `rate`
//   rsseq_render   interleaved stereo int16
//   rsseq_ended    every track has reached its end and every note has died away; a song that
//                  loops never does
//   rsseq_timing   runs the sequencer alone, without sound, to find where a song ends or how it
//                  loops (each track's jumps back), much faster than playing it
//
// The mix is played 6 dB down: the hardware's own sum of 16 channels clips on loud songs, and
// the player's normalising turns quiet ones up again.
//
// deps/rommusic/patches/sseq/ bounds-checks the player's reading of the archive, so damaged
// data throws, and nothing here lets an exception cross the API.
#include "SDAT.h"
#include "Player.h"
#include "NDSStdHeader.h"
#include "SYMBSection.h"
#include "INFOSection.h"

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <vector>

#if defined(_WIN32)
#define RSSEQ_API extern "C" __declspec(dllexport)
#else
#define RSSEQ_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

struct Handle
{
    std::vector<uint8_t> bytes;
    std::unique_ptr<SDAT> sdat;
    Player player;
    std::vector<uint8_t> scratch;
};

void copy_error(char *err, size_t len, const char *what)
{
    if (err && len) {
        std::strncpy(err, what, len - 1);
        err[len - 1] = '\0';
    }
}

} // namespace

typedef void (*rsseq_song_fn)(uint32_t id, const char *name, uint32_t bank, uint32_t volume, void *user);

// Calls `each` for every song in the archive. Returns the number of songs, or -1 (with `err`
// filled in) when the archive cannot be read.
RSSEQ_API int rsseq_list(const uint8_t *sdat, size_t len, rsseq_song_fn each, void *user, char *err, size_t err_len)
{
    try {
        std::vector<uint8_t> bytes(sdat, sdat + len);
        PseudoFile file;
        file.data = &bytes;
        NDSStdHeader header;
        header.Read(file);
        header.Verify("SDAT", 0x0100FEFF);
        uint32_t symbOffset = file.ReadLE<uint32_t>();
        file.ReadLE<uint32_t>();
        uint32_t infoOffset = file.ReadLE<uint32_t>();
        SYMBSection symb;
        if (symbOffset) {
            file.pos = symbOffset;
            try {
                symb.Read(file);
            } catch (const std::exception &) {
                // Names are a nicety: the songs play without them.
                symb = SYMBSection();
            }
        }
        file.pos = infoOffset;
        INFOSection info;
        info.Read(file);
        int count = 0;
        for (const auto &entry : info.SEQrecord.entries) {
            auto name = symb.SEQrecord.entries.find(entry.first);
            if (each)
                each(entry.first, name == symb.SEQrecord.entries.end() ? "" : name->second.c_str(), entry.second.bank, entry.second.vol, user);
            ++count;
        }
        return count;
    } catch (const std::exception &e) {
        copy_error(err, err_len, e.what());
    } catch (...) {
        copy_error(err, err_len, "the sound archive could not be read");
    }
    return -1;
}

// Opens song `id` of the archive for playing at `rate` Hz. Null, with `err` filled in, when the
// song, its bank or its wave archives cannot be read.
RSSEQ_API void *rsseq_open(const uint8_t *sdat, size_t len, uint32_t id, uint32_t rate, char *err, size_t err_len)
{
    try {
        auto h = std::make_unique<Handle>();
        h->bytes.assign(sdat, sdat + len);
        PseudoFile file;
        file.data = &h->bytes;
        h->sdat = std::make_unique<SDAT>(file, id);
        h->player.sampleRate = rate;
        h->player.interpolation = INTERPOLATION_4POINTLEGRANGE;
        h->player.sseqVol = Cnv_Scale(h->sdat->sseq->info.vol);
        h->player.mixGain = 128;
        if (!h->player.Setup(h->sdat->sseq.get())) {
            copy_error(err, err_len, "the song could not be set up");
            return nullptr;
        }
        h->player.Timer();
        return h.release();
    } catch (const std::exception &e) {
        copy_error(err, err_len, e.what());
    } catch (...) {
        copy_error(err, err_len, "the song could not be read");
    }
    return nullptr;
}

RSSEQ_API void rsseq_close(void *handle)
{
    delete static_cast<Handle *>(handle);
}

// Fills `out` with `frames` frames of interleaved stereo. Returns the frames written: `frames`,
// or 0 on an error inside the player.
RSSEQ_API size_t rsseq_render(void *handle, int16_t *out, size_t frames)
{
    auto *h = static_cast<Handle *>(handle);
    if (!h || !out)
        return 0;
    try {
        h->scratch.resize(frames * 4);
        h->player.GenerateSamples(h->scratch, 0, static_cast<unsigned>(frames));
        for (size_t i = 0; i < frames * 2; ++i)
            out[i] = static_cast<int16_t>(h->scratch[2 * i] | (h->scratch[2 * i + 1] << 8));
        return frames;
    } catch (...) {
        return 0;
    }
}

RSSEQ_API int rsseq_ended(void *handle)
{
    auto *h = static_cast<Handle *>(handle);
    if (!h)
        return 1;
    for (uint8_t i = 0; i < h->player.nTracks; ++i)
        if (!h->player.tracks[h->player.trackIds[i]].state[TS_END])
            return 0;
    for (const auto &chn : h->player.channels)
        if (chn.state != CS_NONE)
            return 0;
    return 1;
}

// Runs song `id` for up to `max_seconds` of song time without making sound. Returns 0 when every
// track ends (`*end_seconds` is when), 1 when it loops (`*intro_seconds` and `*loop_seconds`:
// the longest loop of any track, and the latest any track's loop starts), 2 when it does
// neither in that time, -1 on an error (with `err` filled in).
RSSEQ_API int rsseq_timing(const uint8_t *sdat, size_t len, uint32_t id, double max_seconds, double *end_seconds, double *intro_seconds, double *loop_seconds, char *err, size_t err_len)
{
    try {
        std::vector<uint8_t> bytes(sdat, sdat + len);
        PseudoFile file;
        file.data = &bytes;
        SDAT archive(file, id);
        auto player = std::make_unique<Player>();
        player->sampleRate = 32768;
        if (!player->Setup(archive.sseq.get())) {
            copy_error(err, err_len, "the song could not be set up");
            return -1;
        }
        const uint64_t limit = static_cast<uint64_t>(max_seconds / SecondsPerClockCycle);
        while (player->clocks < limit) {
            player->Timer();
            bool all_ended = true, all_known = true;
            int64_t intro = 0, loop = 0;
            for (uint8_t i = 0; i < player->nTracks; ++i) {
                int t = player->trackIds[i];
                if (player->tracks[t].state[TS_END])
                    continue;
                all_ended = false;
                const int64_t *j = player->jumps[t];
                if (j[1] < 0) {
                    all_known = false;
                    break;
                }
                int64_t lp = j[1] - j[0];
                loop = std::max(loop, lp);
                intro = std::max(intro, j[0] - lp);
            }
            if (all_ended) {
                if (end_seconds)
                    *end_seconds = player->clocks * SecondsPerClockCycle;
                return 0;
            }
            if (all_known && loop > 0) {
                if (intro_seconds)
                    *intro_seconds = intro * SecondsPerClockCycle;
                if (loop_seconds)
                    *loop_seconds = loop * SecondsPerClockCycle;
                return 1;
            }
        }
        return 2;
    } catch (const std::exception &e) {
        copy_error(err, err_len, e.what());
    } catch (...) {
        copy_error(err, err_len, "the song could not be read");
    }
    return -1;
}
