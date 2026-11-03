/*
 * psxpbp -- PlayStation discs out of a PSP "PS1 classic" EBOOT.PBP, through Beetle PSX's own
 * PBP reader (external/beetle-psx-libretro, mednafen/cdrom/CDAccess_PBP.c, GPL-2.0-or-later),
 * compiled into a library rather than the RetroArch core.
 *
 * A PBP holds one or more discs, each a run of raw 2352-byte sectors (deflated 16 at a time)
 * and the disc's TOC. Official PSN PBPs are decrypted on the way (Beetle's KIRK/PGD code); their
 * CD audio, which Sony stored as ATRAC3, is not decoded and reads back as silence.
 *
 * Sectors are read by LBA, 0 being track 1's INDEX 01 (00:02:00), as in a Redump track 1 file.
 * Beetle's reader keeps its selected disc in globals, so every call on any handle is
 * serialised by one lock: safe from several threads, never in parallel.
 */
#ifndef PSXPBP_H
#define PSXPBP_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  define PSXPBP_API __declspec(dllexport)
#else
#  define PSXPBP_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any incompatible change to this header; src/rom/pbp.rs checks it. */
#define PSXPBP_ABI_VERSION 1

#define PSXPBP_SECTOR 2352

enum psxpbp_status {
    PSXPBP_OK = 0,
    PSXPBP_ERR_ARGS = -1,  /* a NULL argument, or a disc/track/LBA out of range */
    PSXPBP_ERR_OPEN = -2,  /* not a readable PS1 PBP */
    PSXPBP_ERR_READ = -3,  /* a sector would not read or decompress (a damaged file) */
};

typedef struct psxpbp psxpbp;

typedef struct psxpbp_track {
    int number;        /* 1.. */
    int audio;         /* 1 for an audio track, 0 for data (MODE2/2352 on a PS1 disc) */
    int32_t lba;       /* its INDEX 01 */
    int32_t sectors;   /* from its INDEX 01 to the next track's (or the lead-out) */
} psxpbp_track;

PSXPBP_API int psxpbp_abi_version(void);

/* Opens `path`; NULL on failure, with `error` (when not NULL) set to a short English message. */
PSXPBP_API psxpbp *psxpbp_open(const char *path, char *error, size_t error_len);
PSXPBP_API void psxpbp_close(psxpbp *p);

/* How many discs the PBP holds (1 for most games). */
PSXPBP_API int psxpbp_disc_count(psxpbp *p);

/* Disc `disc`'s tracks (0-based disc index): fills up to `max` entries of `out` and returns
 * how many the disc has, or a negative psxpbp_status. `*leadout` gets the lead-out LBA. */
PSXPBP_API int psxpbp_tracks(psxpbp *p, int disc, psxpbp_track *out, int max, int32_t *leadout);

/* `count` raw 2352-byte sectors of disc `disc` from `lba` into `out`. Returns a psxpbp_status. */
PSXPBP_API int psxpbp_read(psxpbp *p, int disc, int32_t lba, int32_t count, uint8_t *out);

#ifdef __cplusplus
}
#endif

#endif /* PSXPBP_H */
