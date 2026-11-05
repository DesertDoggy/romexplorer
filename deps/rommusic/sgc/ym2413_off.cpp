// The YM2413 (the Master System's Japanese FM unit) left silent. The fork's ym2413.c is old
// MAME code with no clear licence, and Game Gear, what this library is for, has no FM chip.
#include "Ym2413_Emu.h"
#include <string.h>

Ym2413_Emu::Ym2413_Emu() : opll(0) {}
Ym2413_Emu::~Ym2413_Emu() {}
int Ym2413_Emu::set_rate(int, int) { return 0; }
void Ym2413_Emu::reset() {}
void Ym2413_Emu::mute_voices(int) {}
void Ym2413_Emu::write(int, int) {}
void Ym2413_Emu::run(int pair_count, sample_t* out) { memset(out, 0, sizeof(sample_t) * 2 * pair_count); }
