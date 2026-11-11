/* rvt_*: sequenced music read from a game's own files, through VGMTrans (external/vgmtrans,
 * zlib licence) and played with TinySoundFont (external/TinySoundFont, MIT).
 *
 * VGMTrans knows the sequence and instrument formats of the consoles' standard sound
 * drivers (Sony's SEQ/VAB and HD/BD/SQ, Square's, Sega's Saturn driver, ...). It finds them
 * in a file's bytes, pairs each sequence with its instruments, and turns them into a MIDI
 * file and a SoundFont. TinySoundFont then plays the two together. Nothing is emulated, so
 * this needs no BIOS and no set: the music comes from the disc's own files.
 *
 * Only VGMTrans's core library is built (no Qt UI, no shell). Its scanners and matchers keep
 * global state, so one scan runs at a time; a scan converts what it found and lets go of
 * everything VGMTrans holds before it returns. A player owns its own copies and plays
 * alongside further scans. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <map>
#include <memory>
#include <mutex>
#include <set>
#include <string>
#include <vector>

#include "Format.h"
#include "Matcher.h"
#include "Options.h"
#include "Root.h"
#include "RawFile.h"
#include "Scanner.h"
#include "ScannerManager.h"
#include "VGMColl.h"
#include "VGMInstrSet.h"
#include "VGMSeq.h"
#include "SeqTrack.h"
#include "conversion/MidiFile.h"
#include "conversion/SF2Conversion.h"
#include "conversion/SF2File.h"

#define TSF_IMPLEMENTATION
#define TSF_STATIC
#include "tsf.h"
#define TML_IMPLEMENTATION
#define TML_STATIC
#include "tml.h"

#if defined(_WIN32)
#define RVT_API extern "C" __declspec(dllexport)
#else
#define RVT_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

/* VGMTrans's UI hooks, all silent: nothing is saved to disk, and its log goes nowhere. */
class Root final : public VGMRoot {
 public:
  void UI_setRootPtr(VGMRoot **theRoot) override { *theRoot = this; }
  std::filesystem::path UI_getSaveFilePath(const std::string &, const std::string & = "") override {
    return {};
  }
  std::filesystem::path UI_getSaveDirPath(const std::filesystem::path & = {}) override { return {}; }
};

std::mutex g_lock;
Root *g_root = nullptr;

Root &root() {
  if (!g_root) {
    g_root = new Root();
    g_root->init();
  }
  return *g_root;
}

struct Song {
  std::string name;
  std::string format;
  uint32_t offset = 0;
  bool looping = false;
  uint32_t length_ms = 0;
  std::vector<uint8_t> midi;
  int bank = -1; /* index into Scan::banks, or -1 when its instruments were not asked for */
};

struct Scan {
  std::vector<Song> songs;
  std::vector<std::vector<uint8_t>> banks;
};

/* The formats a caller asked for, as a set of VGMTrans format names; empty = all. */
std::set<std::string> format_set(const char *formats) {
  std::set<std::string> out;
  if (!formats) return out;
  std::string all(formats);
  size_t start = 0;
  while (start <= all.size()) {
    size_t end = all.find(',', start);
    if (end == std::string::npos) end = all.size();
    if (end > start) out.insert(all.substr(start, end - start));
    start = end + 1;
  }
  return out;
}

/* The MIDI's length to its last event, as TinyMidiLoader reads it. */
uint32_t midi_length_ms(const std::vector<uint8_t> &midi) {
  tml_message *m = tml_load_memory(midi.data(), (int)midi.size());
  if (!m) return 0;
  unsigned int length = 0;
  tml_get_info(m, nullptr, nullptr, nullptr, nullptr, &length);
  tml_free(m);
  return length;
}

}  // namespace

/* Scans `len` bytes for sequenced music in the named formats (comma-separated VGMTrans
 * format names, e.g. "PS1"; null or "" for every format) and converts each sequence that
 * has instruments to a MIDI file. `loops` is how many times a looping song plays its loop
 * again after the first time through. `bank_for` picks whose instruments are converted to a
 * SoundFont, which is the slow and large part: -1 every song's, -2 none (names and lengths
 * only), or one song's index. Null when nothing was found. */
RVT_API void *rvt_scan(const uint8_t *data, size_t len, const char *name, const char *formats,
                       int loops, int bank_for) {
  if (!data || len == 0 || len > 0xFFFFFFFFu) return nullptr;
  std::lock_guard<std::mutex> guard(g_lock);
  Root &r = root();
  auto wanted = format_set(formats);
  auto scan = std::make_unique<Scan>();
  /* Outlives everything VGMTrans finds in it, which is let go of below. */
  std::unique_ptr<VirtFile> file;
  try {
    ConversionOptions::the().setNumSequenceLoops(loops < 0 ? 0 : loops);
    file = std::make_unique<VirtFile>(data, (uint32_t)len, name ? name : "data.bin");
    r.pushLoadRawFile();
    for (auto &scanner : ScannerManager::get().scanners()) {
      Format *format = scanner->format();
      if (!format) continue;
      if (!wanted.empty() && !wanted.count(format->getName())) continue;
      try {
        scanner->scan(file.get());
      } catch (...) {
      }
      if (auto matcher = format->matcher.get()) matcher->onFinishedScan(file.get());
    }
    r.popLoadRawFile();

    const auto context = ConversionContext::fromOptions(ConversionOptions::the(), SynthTarget::SoundFont);
    std::map<std::vector<VGMInstrSet *>, int> bank_of;
    int index = 0;
    for (VGMColl *coll : r.vgmColls()) {
      VGMSeq *seq = coll->seq();
      if (!seq || coll->instrSets().empty()) continue;
      Song song;
      song.name = coll->name();
      song.format = seq->format() ? seq->format()->getName() : std::string();
      song.offset = seq->offset();
      auto midi = seq->convertToMidi(coll, context);
      if (!midi) continue;
      midi->writeMidiToBuffer(song.midi);
      for (SeqTrack *track : seq->tracks()) {
        if (track->infiniteLoops > 0) song.looping = true;
      }
      song.length_ms = midi_length_ms(song.midi);
      if (bank_for == -1 || bank_for == index) {
        std::vector<VGMInstrSet *> key(coll->instrSets().begin(), coll->instrSets().end());
        auto known = bank_of.find(key);
        if (known != bank_of.end()) {
          song.bank = known->second;
        } else if (auto sf2 = conversion::createSF2File(*coll, context)) {
          scan->banks.push_back(sf2->saveToMem());
          song.bank = (int)scan->banks.size() - 1;
          bank_of.emplace(key, song.bank);
        }
      }
      scan->songs.push_back(std::move(song));
      index++;
    }
  } catch (...) {
    scan->songs.clear();
    scan->banks.clear();
  }
  r.removeAllFilesAndCollections();
  if (scan->songs.empty()) return nullptr;
  return scan.release();
}

RVT_API int rvt_count(void *s) { return s ? (int)((Scan *)s)->songs.size() : 0; }

static const Song *song_at(void *s, int i) {
  if (!s || i < 0 || i >= rvt_count(s)) return nullptr;
  return &((Scan *)s)->songs[i];
}

RVT_API const char *rvt_name(void *s, int i) {
  const Song *song = song_at(s, i);
  return song ? song->name.c_str() : "";
}

RVT_API const char *rvt_format(void *s, int i) {
  const Song *song = song_at(s, i);
  return song ? song->format.c_str() : "";
}

/* Where the sequence starts in the scanned bytes. */
RVT_API uint32_t rvt_offset(void *s, int i) {
  const Song *song = song_at(s, i);
  return song ? song->offset : 0;
}

RVT_API int rvt_looping(void *s, int i) {
  const Song *song = song_at(s, i);
  return song && song->looping ? 1 : 0;
}

/* To the last event, with the loops the scan was asked for; the notes' release comes after. */
RVT_API uint32_t rvt_length_ms(void *s, int i) {
  const Song *song = song_at(s, i);
  return song ? song->length_ms : 0;
}

RVT_API void rvt_free(void *s) { delete (Scan *)s; }

namespace {

struct Player {
  tsf *font = nullptr;
  tml_message *messages = nullptr;
  tml_message *next = nullptr;
  double rate = 44100.0;
  double msec = 0.0;
  ~Player() {
    if (font) tsf_close(font);
    if (messages) tml_free(messages);
  }
};

}  // namespace

/* A player for song `i`, which must have had its instruments converted by the scan.
 * `gain_db` is added to TinySoundFont's output. It owns copies of everything it plays, so
 * the scan can be freed straight away. */
RVT_API void *rvt_play(void *s, int i, int rate, float gain_db) {
  const Song *song = song_at(s, i);
  if (!song || song->bank < 0 || rate <= 0) return nullptr;
  const auto &bank = ((Scan *)s)->banks[song->bank];
  auto p = std::make_unique<Player>();
  p->font = tsf_load_memory(bank.data(), (int)bank.size());
  p->messages = tml_load_memory(song->midi.data(), (int)song->midi.size());
  if (!p->font || !p->messages) return nullptr;
  p->next = p->messages;
  p->rate = rate;
  tsf_set_output(p->font, TSF_STEREO_INTERLEAVED, rate, gain_db);
  /* VGMTrans leaves channel 10 alone (no GM drums), so every channel is melodic. */
  for (int c = 0; c < 16; c++) tsf_channel_set_presetindex(p->font, c, 0);
  return p.release();
}

/* Renders `frames` stereo frames into `out`. 1 once the song's last event has passed and
 * every note has died away (`out` is still filled, with silence after the end), else 0. */
RVT_API int rvt_render(void *player, int16_t *out, size_t frames) {
  Player *p = (Player *)player;
  if (!p) return 1;
  while (frames > 0) {
    int block = frames > TSF_RENDER_EFFECTSAMPLEBLOCK ? TSF_RENDER_EFFECTSAMPLEBLOCK : (int)frames;
    for (p->msec += block * (1000.0 / p->rate); p->next && p->msec >= p->next->time; p->next = p->next->next) {
      tml_message *m = p->next;
      switch (m->type) {
        case TML_PROGRAM_CHANGE: tsf_channel_set_presetnumber(p->font, m->channel, m->program, 0); break;
        case TML_NOTE_ON: tsf_channel_note_on(p->font, m->channel, m->key, m->velocity / 127.0f); break;
        case TML_NOTE_OFF: tsf_channel_note_off(p->font, m->channel, m->key); break;
        case TML_PITCH_BEND: tsf_channel_set_pitchwheel(p->font, m->channel, m->pitch_bend); break;
        case TML_CONTROL_CHANGE: tsf_channel_midi_control(p->font, m->channel, m->control, m->control_value); break;
        default: break;
      }
    }
    tsf_render_short(p->font, out, block, 0);
    out += block * 2;
    frames -= block;
  }
  return !p->next && tsf_active_voice_count(p->font) == 0 ? 1 : 0;
}

RVT_API void rvt_play_free(void *player) { delete (Player *)player; }
