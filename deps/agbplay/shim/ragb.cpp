// ragb: a flat C API over agbplay (external/agbplay, LGPL-3.0), which plays GBA music made with
// Nintendo's standard sound engine (m4a / MusicPlayer2000) straight from the ROM.
//
//   ragb_open      ROM bytes -> handle, after agbplay's scanner found the song table, the
//                  music player table and the engine's mixing settings; null when it found none
//   ragb_song_*    the table: how many entries, and each one's track count and music player
//   ragb_start     start one song, playing its loop `loops` times
//   ragb_render    interleaved stereo int16 at the rate given to ragb_open
//
// Errors are C strings copied into the caller's buffer; nothing here throws across the API.
#include "MP2KContext.hpp"
#include "MP2KScanner.hpp"
#include "Rom.hpp"
#include "Types.hpp"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <vector>

#if defined(_WIN32)
#define RAGB_API extern "C" __declspec(dllexport)
#else
#define RAGB_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

struct Handle
{
    std::vector<uint8_t> bytes;
    std::unique_ptr<Rom> rom;
    uint32_t rate = 48000;
    MP2KScanner::Result scan;
    std::unique_ptr<MP2KContext> ctx;
    std::vector<int16_t> pending;    // rendered but not yet handed out
    size_t pendingAt = 0;
    bool ended = true;
};

void copy_error(char *err, size_t len, const char *what)
{
    if (err && len) {
        std::strncpy(err, what, len - 1);
        err[len - 1] = '\0';
    }
}

int16_t to_i16(float v)
{
    const float s = std::clamp(v, -1.0f, 1.0f) * 32767.0f;
    return static_cast<int16_t>(std::lrint(s));
}

// The song header a table entry points at, or SIZE_MAX when the pointer leaves the ROM.
size_t song_header(const Handle &h, uint32_t song)
{
    const size_t entry = h.scan.songTableInfo.pos + size_t(song) * 8;
    if (!h.rom->ValidRange(entry, 8) || !h.rom->ValidPointer(h.rom->ReadU32(entry)))
        return SIZE_MAX;
    return h.rom->ReadAgbPtrToPos(entry);
}

}    // namespace

RAGB_API void *ragb_open(const uint8_t *data, size_t len, uint32_t rate, char *err, size_t err_len)
{
    try {
        auto h = std::make_unique<Handle>();
        h->bytes.assign(data, data + len);
        h->rate = rate;
        h->rom = std::make_unique<Rom>(Rom::LoadFromBufferRef(h->bytes));
        MP2KScanner scanner(*h->rom);
        auto results = scanner.Scan(nullptr);
        if (results.empty()) {
            copy_error(err, err_len, "no song table of the standard sound engine");
            return nullptr;
        }
        // Some games link the engine twice (a compilation, a bonus game); the largest table is
        // the game's own music.
        h->scan = *std::max_element(results.begin(), results.end(), [](const auto &a, const auto &b) {
            return a.songTableInfo.count < b.songTableInfo.count;
        });
        return h.release();
    } catch (const std::exception &e) {
        copy_error(err, err_len, e.what());
        return nullptr;
    }
}

/// Like ragb_open, for a song table found some other way (by its shape) that agbplay's own
/// scanner turned down, because it could not also find the engine's player table and mixing
/// settings. Those take the engine's usual values: four players of up to 16 tracks, full volume,
/// 13,379 Hz mixing, no reverb.
RAGB_API void *ragb_open_at(const uint8_t *data, size_t len, uint32_t rate, uint64_t table, uint32_t count, char *err, size_t err_len)
{
    try {
        auto h = std::make_unique<Handle>();
        h->bytes.assign(data, data + len);
        h->rate = rate;
        h->rom = std::make_unique<Rom>(Rom::LoadFromBufferRef(h->bytes));
        if (table + uint64_t(count) * 8 > len || count == 0) {
            copy_error(err, err_len, "the song table lies outside the ROM");
            return nullptr;
        }
        h->scan.songTableInfo.pos = static_cast<size_t>(table);
        h->scan.songTableInfo.count = static_cast<uint16_t>(std::min<uint32_t>(count, 0xFFFE));
        h->scan.playerTableInfo = PlayerTableInfo(4, PlayerInfo{.maxTracks = 16, .usePriority = 0});
        h->scan.mp2kSoundMode = MP2KSoundMode{.vol = 15, .rev = 0, .freq = 4, .maxChannels = 12, .dacConfig = 9};
        return h.release();
    } catch (const std::exception &e) {
        copy_error(err, err_len, e.what());
        return nullptr;
    }
}

RAGB_API void ragb_close(void *handle)
{
    delete static_cast<Handle *>(handle);
}

RAGB_API uint32_t ragb_song_count(void *handle)
{
    return static_cast<Handle *>(handle)->scan.songTableInfo.count;
}

/// Where the song table is in the ROM (an offset), for keeping with the index.
RAGB_API uint64_t ragb_song_table(void *handle)
{
    return static_cast<Handle *>(handle)->scan.songTableInfo.pos;
}

/// How many tracks song `song` has: 0 for the empty slots a table keeps for unused numbers.
RAGB_API uint32_t ragb_song_tracks(void *handle, uint32_t song)
{
    auto &h = *static_cast<Handle *>(handle);
    try {
        const size_t header = song_header(h, song);
        return header == SIZE_MAX ? 0 : h.rom->ReadU8(header);
    } catch (const std::exception &) {
        return 0;
    }
}

/// Which music player song `song` plays on (BGM and sound effects usually use different ones).
RAGB_API uint32_t ragb_song_player(void *handle, uint32_t song)
{
    auto &h = *static_cast<Handle *>(handle);
    try {
        return h.rom->ReadU8(h.scan.songTableInfo.pos + size_t(song) * 8 + 4);
    } catch (const std::exception &) {
        return 0;
    }
}

RAGB_API int ragb_start(void *handle, uint32_t song, int loops, char *err, size_t err_len)
{
    auto &h = *static_cast<Handle *>(handle);
    try {
        h.ctx = std::make_unique<MP2KContext>(
            h.rate, static_cast<int8_t>(std::clamp(loops, 0, 100)), *h.rom, h.scan.mp2kSoundMode, AgbplaySoundMode{},
            h.scan.songTableInfo, h.scan.playerTableInfo
        );
        h.ctx->m4aSongNumStart(static_cast<uint16_t>(song));
        h.pending.clear();
        h.pendingAt = 0;
        h.ended = false;
        return 1;
    } catch (const std::exception &e) {
        copy_error(err, err_len, e.what());
        h.ctx.reset();
        h.ended = true;
        return 0;
    }
}

/// Up to `frames` stereo frames into `out`; fewer only when the song has ended.
RAGB_API size_t ragb_render(void *handle, int16_t *out, size_t frames)
{
    auto &h = *static_cast<Handle *>(handle);
    size_t done = 0;
    try {
        while (done < frames) {
            if (h.pendingAt < h.pending.size()) {
                const size_t take = std::min((h.pending.size() - h.pendingAt) / 2, frames - done);
                std::memcpy(out + done * 2, h.pending.data() + h.pendingAt, take * 2 * sizeof(int16_t));
                h.pendingAt += take * 2;
                done += take;
                continue;
            }
            if (h.ended || !h.ctx)
                break;
            h.ctx->m4aSoundMain();
            if (h.ctx->SongEnded()) {
                h.ended = true;
                break;
            }
            const size_t n = h.ctx->mixer.GetSamplesPerBuffer();
            h.pending.resize(n * 2);
            for (size_t i = 0; i < n; i++) {
                h.pending[i * 2] = to_i16(h.ctx->masterAudioBuffer[i].left);
                h.pending[i * 2 + 1] = to_i16(h.ctx->masterAudioBuffer[i].right);
            }
            h.pendingAt = 0;
        }
    } catch (const std::exception &) {
        h.ended = true;
    }
    return done;
}

RAGB_API int ragb_ended(void *handle)
{
    auto &h = *static_cast<Handle *>(handle);
    return h.ended && h.pendingAt >= h.pending.size();
}
