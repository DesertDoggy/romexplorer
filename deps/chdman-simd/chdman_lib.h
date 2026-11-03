#pragma once

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  ifdef CHDMAN_BUILDING_DLL
#    define CHDMAN_API __declspec(dllexport)
#  else
#    define CHDMAN_API __declspec(dllimport)
#  endif
#else
#  define CHDMAN_API __attribute__((visibility("default")))
#endif

/*
 * Called for every line chdman writes to stdout/stderr during chdman_run, as it's
 * written (not batched at the end) -- this is how a caller sees live progress.
 *
 * text is that one line, WITHOUT its trailing '\r'/'\n' (chdman uses '\r' for in-place
 * progress updates like "Compressing, 45.3% complete... (ratio=61.2%)", and '\n' for
 * ordinary messages). percent is the parsed value from a "<float>% complete" segment
 * when the line contains one (chdman's consistent progress format across every
 * long-running command), or -1.0f when the line doesn't have one (general/final
 * messages) -- callers should treat -1.0f as "no change to the last known percent".
 *
 * Called on the same thread that called chdman_run, synchronously between chdman's own
 * writes -- keep this fast; do not call back into chdman_run from within it.
 */
typedef void (*ChdmanProgressCb)(const char* text, float percent, void* user_data);

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Runs chdman with the same command-line arguments the chdman CLI takes -- e.g.
 * createcd / extractcd / createdvd / createhd / extracthd / createraw / extractraw /
 * createld / info / verify -- exactly as documented by `chdman help <command>`.
 *
 * argv must NOT include a program name: argv[0] is the command itself (e.g. "createcd"),
 * followed by its options and values (e.g. "-i", "input.cue", "-o", "output.chd").
 * argc is the number of entries in argv. This mirrors invoking:
 *   chdman <argv[0]> <argv[1]> <argv[2]> ...
 * from the command line -- input/output paths and every other chdman option are passed
 * exactly as the CLI would take them, just as function arguments instead of a shell
 * command line.
 *
 * Returns the same exit code the chdman CLI would return (0 = success).
 *
 * on_progress, if non-NULL, is called for every line of chdman's output as it happens
 * -- see ChdmanProgressCb above. Pass NULL to skip this (e.g. for quick commands like
 * info/verify where progress isn't useful).
 *
 * All text chdman would normally print to stdout/stderr during the call is also
 * accumulated (not just delivered to on_progress) instead of being written to the real
 * streams. If out_log is non-NULL, *out_log is set to a newly allocated, null-terminated
 * buffer containing that full accumulated text (never NULL, even on success -- may be
 * an empty string). The caller owns this buffer and must release it with
 * chdman_free_log(). Pass NULL for out_log to discard the text.
 *
 * Not safe to call concurrently with itself on multiple threads at once: chdman's
 * command handlers use process-wide state (e.g. the shared CHD compressor thread
 * pool). Serialize calls, e.g. one thread per conversion.
 */
CHDMAN_API int chdman_run(int argc, const char* const* argv, char** out_log,
                           ChdmanProgressCb on_progress, void* user_data);

/* Releases a buffer returned via chdman_run's out_log. Safe to call with NULL. */
CHDMAN_API void chdman_free_log(char* log);

#ifdef CHDMAN_WITH_STREAMING
/*
 * ---------------------------------------------------------------------------------------
 * Streaming build only (user/scripts/build_chdman_lib.sh's default). A library exports
 * these iff it was built that way, so callers detect support by looking up
 * chdman_extract_stream / chdman_reader_open. Nothing here changes the CHD file format.
 * ---------------------------------------------------------------------------------------
 */

/*
 * One block of an output file, in write order. file_index numbers the output files in the
 * order they are first written (0, 1, ...); file_name is the path chdman writes that file
 * to. offset_in_file is where the block goes in that file -- blocks of one file arrive in
 * increasing, contiguous order. Return 0 to stop the extraction, nonzero to continue.
 */
typedef int (*ChdmanDataCb)(uint32_t file_index, const char* file_name, uint64_t offset_in_file,
                            const void* data, uint32_t size, void* user_data);

/*
 * chdman_run for the extract commands, additionally handing every block chdman writes to
 * on_data -- so one decompression pass can feed a hasher / re-archiver as well as (or
 * instead of) the disk. argv is exactly as for chdman_run (e.g. "extractcd", "-i", in,
 * "-o", out.cue). Supported: extractraw, extracthd, extractdvd, extractcd.
 *
 * The bytes are exactly the bytes chdman writes (the hook sits at chdman's own write
 * calls), so cue/gdi/toc track layout is identical to a plain extract. For extractcd,
 * after the track files, the finished cue/gdi/toc sheet itself is delivered as one more
 * file (next file_index, file_name = the -o path).
 *
 * write_files = 0: stream only. Nothing is left on disk -- chdman still opens its output
 * paths, so they must be writable and not already exist (or pass -f), but every file it
 * created is deleted before returning.
 *
 * Returns chdman's exit code (0 = success), or -6 if on_data returned 0. Same threading
 * rule as chdman_run: serialize calls.
 */
CHDMAN_API int chdman_extract_stream(int argc, const char* const* argv, int write_files,
                                     ChdmanDataCb on_data, char** out_log,
                                     ChdmanProgressCb on_progress, void* user_data);

/*
 * Random access to a CHD's decompressed contents, without extracting it.
 *
 * Every CHD hunk is compressed independently and the hunk map is fully decoded at open,
 * so reading any hunk costs one seek + one hunk decompression, in any order. Each reader
 * owns its own file handle, decompressors and cache and shares nothing with other readers
 * or with chdman_run, so for parallel reads open one reader per thread (the same CHD may
 * be opened any number of times). A single reader is not thread-safe.
 *
 * The file is opened through a plain stdio handle rather than chdman's OSD file layer, so
 * it does not get the sequential-scan hint chdman-simd's 02 patch applies to read-only
 * opens on Windows.
 */
typedef struct ChdmanReader ChdmanReader;

typedef struct {
    uint64_t logical_bytes; /* decompressed size (for a DVD CHD, the ISO size) */
    uint32_t hunk_bytes;
    uint32_t hunk_count;
    uint32_t unit_bytes;    /* e.g. 2448 (one CD frame: 2352 data + 96 subcode), 2048 for DVD */
    uint32_t version;       /* CHD format version (3, 4, 5) */
    int32_t is_cd;          /* CD or GD-ROM: track/sector calls are valid */
    int32_t is_gdrom;
    uint32_t num_tracks;    /* 0 when !is_cd */
} ChdmanReaderInfo;

typedef struct {
    uint32_t track_type;    /* CHDMAN_CD_TRACK_* */
    uint32_t sub_type;      /* 0 = cooked, 1 = raw, 2 = none */
    uint32_t data_size;     /* bytes of data per sector in this track */
    uint32_t sub_size;      /* bytes of subcode per sector */
    uint32_t frames;        /* frames in the track, including pregap stored in the CHD */
    uint32_t pregap;
    uint32_t postgap;
    uint32_t session;
    uint32_t logical_start; /* first logical LBA of the track data */
    uint32_t physical_start;/* first physical frame of the track data (phys=1 LBAs) */
    uint32_t chd_frame_start; /* frame index of the track's start within the CHD */
} ChdmanTrackInfo;

/* Values for ChdmanTrackInfo.track_type and chdman_reader_read_sector's datatype. */
enum {
    CHDMAN_CD_TRACK_MODE1 = 0,        /* 2048 */
    CHDMAN_CD_TRACK_MODE1_RAW = 1,    /* 2352 */
    CHDMAN_CD_TRACK_MODE2 = 2,        /* 2336 */
    CHDMAN_CD_TRACK_MODE2_FORM1 = 3,  /* 2048 */
    CHDMAN_CD_TRACK_MODE2_FORM2 = 4,  /* 2324 */
    CHDMAN_CD_TRACK_MODE2_FORM_MIX = 5, /* 2336 */
    CHDMAN_CD_TRACK_MODE2_RAW = 6,    /* 2352 */
    CHDMAN_CD_TRACK_AUDIO = 7,        /* 2352 */
    CHDMAN_CD_TRACK_RAW_DONTCARE = 8  /* whatever the track stores */
};

/*
 * Opens path (and parent_path, for a delta CHD; NULL otherwise). cache_hunks is how many
 * decompressed hunks read_bytes keeps (LRU) for partial-hunk reads; 0 = chdman's own
 * single-hunk cache only. Returns NULL on failure (see chdman_reader_last_error).
 */
CHDMAN_API ChdmanReader* chdman_reader_open(const char* path, const char* parent_path, uint32_t cache_hunks);
CHDMAN_API void chdman_reader_close(ChdmanReader* reader);
CHDMAN_API int chdman_reader_get_info(ChdmanReader* reader, ChdmanReaderInfo* out);

/* Decompresses hunk `hunk` into buffer (>= hunk_bytes). 0 on success. */
CHDMAN_API int chdman_reader_read_hunk(ChdmanReader* reader, uint32_t hunk, void* buffer);

/* Reads `size` decompressed bytes at `offset` (any alignment). 0 on success. */
CHDMAN_API int chdman_reader_read_bytes(ChdmanReader* reader, uint64_t offset, void* buffer, uint32_t size);

/* CD/GD-ROM only. track is 0-based. 0 on success. */
CHDMAN_API int chdman_reader_get_track(ChdmanReader* reader, uint32_t track, ChdmanTrackInfo* out);

/*
 * CD/GD-ROM only: reads one sector as `datatype` (e.g. CHDMAN_CD_TRACK_MODE1 for the
 * 2048-byte user data of a MODE1 or MODE1_RAW track -- what an ISO9660 walk wants).
 * lba is logical, or physical when phys != 0. buffer must hold 2352 bytes. 0 on success.
 */
CHDMAN_API int chdman_reader_read_sector(ChdmanReader* reader, uint32_t lba, void* buffer,
                                         uint32_t datatype, int phys);

/* Last error text from a chdman_reader_* call on this thread. Never NULL. */
CHDMAN_API const char* chdman_reader_last_error(void);

/*
 * One input file whose bytes come from the caller instead of the filesystem. Give `read`
 * (sequential), `read_at` (random access), or both; with both, `read_at` is used.
 *
 *   read:    copy up to `size` next bytes into buf; return the count (0 = end), <0 = error.
 *   read_at: copy exactly `size` bytes at `offset` into buf; return 0, nonzero = error.
 *
 * chdman consumes inputs front to back. A sequential source is served through a history
 * window, so chdman's small re-reads behind the current position still work; a read before
 * that window fails. Callbacks may run on a chdman worker thread, never two at once for one
 * source.
 */
typedef struct {
    const char* name;  /* the name the command refers to (see below) */
    uint64_t size;     /* exact byte count */
    int64_t (*read)(void* user_data, void* buf, uint64_t size);
    int (*read_at)(void* user_data, uint64_t offset, void* buf, uint64_t size);
    void* user_data;
} ChdmanSource;

/*
 * chdman_run for createraw / createhd / createdvd / createcd with inputs from `sources`.
 * argv is as for chdman_run; its -i value must be the `name` of one source:
 *   - createraw/createhd/createdvd: that source is the input image.
 *   - createcd: that source is the .cue/.gdi/.toc sheet; the track files it references
 *     (by the file names written in the sheet) are the other sources, matched by name.
 *     (.iso/.nrg inputs to createcd are read with plain stdio by chdman and can't come
 *     from sources; use createdvd for disc images.)
 * The output (-o) is a normal file. Sheets are written to a private temp directory for the
 * duration of the call (chdman parses them with stdio); nothing else touches the disk.
 * Returns chdman's exit code (0 = success). Same threading rule as chdman_run.
 */
CHDMAN_API int chdman_create_from_sources(int argc, const char* const* argv,
                                          const ChdmanSource* sources, int count,
                                          char** out_log, ChdmanProgressCb on_progress,
                                          void* user_data);
#endif /* CHDMAN_WITH_STREAMING */

#ifdef __cplusplus
}
#endif
