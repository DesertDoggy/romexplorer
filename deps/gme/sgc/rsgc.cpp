// rsgc: SGC (Game Gear, Master System and ColecoVision music rips) through kode54's
// Game_Music_Emu fork, under the names and the struct layout of upstream libgme's C API with
// an `rsgc_` prefix, so src/rom/gme.rs drives both libraries the same way. Only the rsgc_
// functions are exported; the fork's own gme_* stay inside this library.
#include "gme.h"
#include "Music_Emu.h"
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#define RSGC_EXPORT extern "C" __declspec(dllexport)
#else
#define RSGC_EXPORT extern "C" __attribute__((visibility("default")))
#endif

// upstream libgme's gme_info_t: fade_length sits where the fork has a reserved int.
struct rsgc_info_t {
	int length, intro_length, loop_length, play_length, fade_length;
	int i5, i6, i7, i8, i9, i10, i11, i12, i13, i14, i15;
	const char *system, *game, *song, *author, *copyright, *comment, *dumper;
	const char *s7, *s8, *s9, *s10, *s11, *s12, *s13, *s14, *s15;
	gme_info_t* inner;
};

// The fork's SGC player runs about 8 dB hotter than upstream libgme's other players (and
// clips): brought down to sit with them.
static double const sgc_gain = 0.4;

// gme_open_data with the gain set between creating the player and loading the file, which
// is when the SGC player reads it. (set_gain asserts no sample rate is set yet; the gain is
// only read at load, so setting it here is safe, and Release builds compile the assert out.)
RSGC_EXPORT gme_err_t rsgc_open_data(void const* data, long size, gme_t** out, int rate) {
	*out = 0;
	gme_type_t type = size >= 4 ? gme_identify_extension(gme_identify_header(data)) : 0;
	if (!type) return "Wrong file type for this emulator";
	Music_Emu* emu = gme_new_emu(type, rate);
	if (!emu) return "Out of memory";
	emu->set_gain(sgc_gain);
	gme_err_t err = gme_load_data(emu, data, size);
	if (err) { delete emu; return err; }
	*out = emu;
	return 0;
}
RSGC_EXPORT int rsgc_track_count(const gme_t* e) { return gme_track_count(e); }
RSGC_EXPORT gme_err_t rsgc_start_track(gme_t* e, int i) { return gme_start_track(e, i); }
RSGC_EXPORT gme_err_t rsgc_play(gme_t* e, int n, short* out) { return gme_play(e, n, out); }
RSGC_EXPORT void rsgc_delete(gme_t* e) { gme_delete(e); }
RSGC_EXPORT void rsgc_set_fade_msecs(gme_t* e, int start, int len) { gme_set_fade(e, start, len); }
// The fork never limits a track by itself.
RSGC_EXPORT void rsgc_set_autoload_playback_limit(gme_t*, int) {}
RSGC_EXPORT int rsgc_track_ended(const gme_t* e) { return gme_track_ended(e); }
RSGC_EXPORT int rsgc_tell(const gme_t* e) { return gme_tell(e); }
RSGC_EXPORT gme_err_t rsgc_seek(gme_t* e, int msec) { return gme_seek(e, msec); }
RSGC_EXPORT const char* rsgc_identify_header(void const* h) { return gme_identify_header(h); }
RSGC_EXPORT gme_err_t rsgc_load_m3u_data(gme_t* e, void const* data, long size) { return gme_load_m3u_data(e, data, size); }

RSGC_EXPORT gme_err_t rsgc_track_info(const gme_t* e, rsgc_info_t** out, int track) {
	*out = 0;
	gme_info_t* in = 0;
	gme_err_t err = gme_track_info(e, &in, track);
	if (err) return err;
	rsgc_info_t* o = (rsgc_info_t*) calloc(1, sizeof *o);
	if (!o) { gme_free_info(in); return "Out of memory"; }
	o->length = in->length;
	o->intro_length = in->intro_length;
	o->loop_length = in->loop_length;
	o->play_length = in->play_length;
	o->fade_length = -1;
	o->system = in->system; o->game = in->game; o->song = in->song; o->author = in->author;
	o->copyright = in->copyright; o->comment = in->comment; o->dumper = in->dumper;
	o->inner = in;
	*out = o;
	return 0;
}

RSGC_EXPORT void rsgc_free_info(rsgc_info_t* o) {
	if (!o) return;
	gme_free_info(o->inner);
	free(o);
}
