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
#include <stdio.h>
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
#include "VGMSampColl.h"
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
  int file = 0; /* which of the scanned files the sequence is in */
  bool looping = false;
  uint32_t length_ms = 0;
  std::vector<uint8_t> midi;
  int bank = -1; /* index into Scan::banks, or -1 when its instruments were not asked for */
  /* The runs of the scanned files the song is made of: its sequence first, then its
     instrument sets and sample collections. file -1: data VGMTrans made itself (unpacked). */
  struct Part {
    int file;
    uint32_t offset, length;
  };
  std::vector<Part> parts;
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

/* What a song plays, without the names and markers VGMTrans writes into its MIDI (which
 * carry where the sequence sat): two copies of one sequence give the same key. */
std::vector<uint8_t> events_key(const std::vector<uint8_t> &midi) {
  std::vector<uint8_t> key;
  tml_message *first = tml_load_memory(midi.data(), (int)midi.size());
  for (tml_message *m = first; m; m = m->next) {
    if (m->type == TML_SET_TEMPO) continue;
    uint32_t t = m->time;
    uint8_t rec[8] = {(uint8_t)t, (uint8_t)(t >> 8), (uint8_t)(t >> 16), (uint8_t)(t >> 24), m->type, m->channel,
                      (uint8_t)m->key, (uint8_t)m->velocity};
    key.insert(key.end(), rec, rec + 8);
  }
  tml_free(first);
  return key;
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

/* Sequences their format's matcher left without instruments, paired with the Sony VABs in
   the same file: Konami's KDT1 sequences play on plain VABs, which VGMTrans files under
   another format, so its per-format matchers never put the two together. Each such sequence
   gets the VAB nearest after it (else the nearest before); with RVT_ORPHAN_BANKS=all set,
   one song per VAB in its file, for working out which bank a game uses. */
static std::vector<std::unique_ptr<VGMColl>> pair_orphans(Root &r, const std::vector<VGMColl *> &colls) {
  std::vector<std::unique_ptr<VGMColl>> out;
  std::set<VGMSeq *> placed;
  for (VGMColl *c : colls)
    if (c->seq()) placed.insert(c->seq());
  std::vector<VGMSeq *> orphans;
  std::vector<VGMInstrSet *> vabs;
  for (const auto &v : r.vgmFiles()) {
    if (auto seq = std::get_if<VGMSeq *>(&v)) {
      if (*seq && !placed.count(*seq)) orphans.push_back(*seq);
    } else if (auto set = std::get_if<VGMInstrSet *>(&v)) {
      if (*set && (*set)->format() && (*set)->format()->getName() == "PS1" && (*set)->sampColl()) vabs.push_back(*set);
    }
  }
  if (orphans.empty() || vabs.empty()) return out;
  const char *mode = getenv("RVT_ORPHAN_BANKS");
  bool all = mode && strcmp(mode, "all") == 0;
  for (VGMSeq *seq : orphans) {
    std::vector<VGMInstrSet *> same;
    for (VGMInstrSet *v : vabs)
      if (v->rawFile() == seq->rawFile()) same.push_back(v);
    if (same.empty()) continue;
    std::vector<VGMInstrSet *> picks;
    if (all) {
      picks = same;
    } else {
      VGMInstrSet *after = nullptr, *before = nullptr;
      for (VGMInstrSet *v : same) {
        if (v->offset() >= seq->offset()) {
          if (!after || v->offset() < after->offset()) after = v;
        } else if (!before || v->offset() > before->offset()) {
          before = v;
        }
      }
      picks.push_back(after ? after : before);
    }
    for (VGMInstrSet *v : picks) {
      std::string name = seq->name();
      if (all) {
        char at[32];
        snprintf(at, sizeof at, " +bank@%x", (unsigned)v->offset());
        name += at;
      }
      auto coll = std::make_unique<VGMColl>(name);
      coll->attachSeq(seq);
      coll->attachInstrSet(v);
      coll->attachSampColl(v->sampColl());
      out.push_back(std::move(coll));
    }
  }
  return out;
}

/* Scans `n` files (`datas[i]`, `lens[i]` bytes, named `names[i]`) for sequenced music in the
 * named formats (comma-separated VGMTrans format names, e.g. "PS1"; null or "" for every
 * format) and converts each sequence that has instruments to a MIDI file. The files are
 * scanned as separate files of one disc, so a format that pairs a sequence with
 * instruments from another file (by id or by name) finds them; Sony's SEQ/VAB pairs only
 * within one file. `loops` is how many times a looping song plays its loop again after the
 * first time through. `bank_for` picks whose instruments are converted to a SoundFont,
 * which is the slow and large part: -1 every song's, -2 none (names and lengths only), or
 * one song's index. Null when nothing was found. */
RVT_API void *rvt_scan_files(const uint8_t *const *datas, const size_t *lens, const char *const *names, size_t n,
                             const char *formats, int loops, int bank_for) {
  if (!datas || !lens || n == 0) return nullptr;
  for (size_t i = 0; i < n; i++) {
    if (!datas[i] || lens[i] == 0 || lens[i] > 0xFFFFFFFFu) return nullptr;
  }
  std::lock_guard<std::mutex> guard(g_lock);
  Root &r = root();
  auto wanted = format_set(formats);
  auto scan = std::make_unique<Scan>();
  /* Outlive everything VGMTrans finds in them, which is let go of below. */
  std::vector<std::unique_ptr<VirtFile>> files;
  std::map<RawFile *, int> file_index;
  try {
    ConversionOptions::the().setNumSequenceLoops(loops < 0 ? 0 : loops);
    auto scanners = ScannerManager::get().scanners();
    r.pushLoadRawFile();
    for (size_t i = 0; i < n; i++) {
      const char *name = names && names[i] ? names[i] : "data.bin";
      files.push_back(std::make_unique<VirtFile>(datas[i], (uint32_t)lens[i], name));
      VirtFile *file = files.back().get();
      file_index[file] = (int)i;
      for (auto &scanner : scanners) {
        Format *format = scanner->format();
        if (!format) continue;
        if (!wanted.empty() && !wanted.count(format->getName())) continue;
        try {
          scanner->scan(file);
        } catch (...) {
        }
        try {
          if (auto matcher = format->matcher.get()) matcher->onFinishedScan(file);
        } catch (...) {
        }
      }
    }
    r.popLoadRawFile();

    const auto context = ConversionContext::fromOptions(ConversionOptions::the(), SynthTarget::SoundFont);
    std::map<std::vector<VGMInstrSet *>, int> bank_of;
    std::set<std::vector<uint8_t>> seen_midi;
    int index = 0;
    std::vector<VGMColl *> colls(r.vgmColls().begin(), r.vgmColls().end());
    std::vector<std::unique_ptr<VGMColl>> paired = pair_orphans(r, colls);
    for (auto &extra : paired) colls.push_back(extra.get());
    for (VGMColl *coll : colls) {
      VGMSeq *seq = coll->seq();
      if (!seq || coll->instrSets().empty()) continue;
      Song song;
      song.name = coll->name();
      song.format = seq->format() ? seq->format()->getName() : std::string();
      song.offset = seq->offset();
      {
        auto known = file_index.find(seq->rawFile());
        song.file = known == file_index.end() ? 0 : known->second;
      }
      {
        auto part = [&](VGMFile *f) {
          if (!f) return;
          auto known = file_index.find(f->rawFile());
          int idx = known == file_index.end() ? -1 : known->second;
          for (const auto &p : song.parts)
            if (p.file == idx && p.offset == f->offset() && p.length == f->length()) return;
          song.parts.push_back({idx, f->offset(), f->length()});
        };
        part(seq);
        for (VGMInstrSet *set : coll->instrSets()) {
          part(set);
          part(set->sampColl());
        }
        for (VGMSampColl *samp : coll->sampColls()) part(samp);
      }
      /* A false hit can read past its file; reads throw then, and the song is left out. */
      try {
        auto midi = seq->convertToMidi(coll, context);
        if (!midi) continue;
        midi->writeMidiToBuffer(song.midi);
      } catch (...) {
        continue;
      }
      /* A disc can hold one sequence many times over (in each pack that uses it). */
      if (!seen_midi.insert(events_key(song.midi)).second) continue;
      for (SeqTrack *track : seq->tracks()) {
        if (track->infiniteLoops > 0) song.looping = true;
      }
      song.length_ms = midi_length_ms(song.midi);
      if (bank_for == -1 || bank_for == index) {
        std::vector<VGMInstrSet *> key(coll->instrSets().begin(), coll->instrSets().end());
        auto known = bank_of.find(key);
        if (known != bank_of.end()) {
          song.bank = known->second;
        } else {
          try {
            if (auto sf2 = conversion::createSF2File(*coll, context)) {
              scan->banks.push_back(sf2->saveToMem());
              song.bank = (int)scan->banks.size() - 1;
              bank_of.emplace(key, song.bank);
            }
          } catch (...) {
          }
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

/* One file: rvt_scan_files with n = 1. */
RVT_API void *rvt_scan(const uint8_t *data, size_t len, const char *name, const char *formats, int loops,
                       int bank_for) {
  return rvt_scan_files(&data, &len, &name, 1, formats, loops, bank_for);
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

/* Which of the scanned files the sequence is in. */
RVT_API int rvt_file(void *s, int i);

/* Where the sequence starts in its file. */
RVT_API uint32_t rvt_offset(void *s, int i) {
  const Song *song = song_at(s, i);
  return song ? song->offset : 0;
}

RVT_API int rvt_file(void *s, int i) {
  const Song *song = song_at(s, i);
  return song ? song->file : 0;
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

/* How many runs song `i` is made of (Song::parts), and run `k`'s file, offset and length. */
RVT_API int rvt_part_count(void *s, int i) {
  Scan *scan = (Scan *)s;
  if (!scan || i < 0 || i >= (int)scan->songs.size()) return 0;
  return (int)scan->songs[i].parts.size();
}

RVT_API int rvt_part(void *s, int i, int k, uint32_t *offset, uint32_t *length) {
  Scan *scan = (Scan *)s;
  if (!scan || i < 0 || i >= (int)scan->songs.size() || k < 0 || k >= (int)scan->songs[i].parts.size()) return -1;
  const auto &p = scan->songs[i].parts[k];
  *offset = p.offset;
  *length = p.length;
  return p.file;
}

RVT_API void rvt_free(void *s) { delete (Scan *)s; }

namespace {

/* TinySoundFont plays VGMTrans's SoundFonts louder than the consoles did: measured against
 * the same songs rendered from original audio format files by the console's own sound
 * chip, Sony's SEQ/VAB came out about 13 dB louder in RMS (and clipped), Square's AKAO and
 * FF Tactics engines 2 to 8 dB. */
float format_gain_db(const std::string &format) {
  if (format == "PS1") return -12.0f;
  return -6.0f;
}

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
  tsf_set_output(p->font, TSF_STEREO_INTERLEAVED, rate, gain_db + format_gain_db(song->format));
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
