/*
 * exiso -- an Xbox or Xbox 360 disc image rewritten as an XISO, through extract-xiso's own
 * code (external/extract-xiso, XboxDev), compiled into a library rather than a CLI.
 *
 * The rewrite is extract-xiso's `-r`: the game partition's XDVDFS filesystem, re-laid out
 * with only the files in it. What a Redump image holds besides that (the video partition,
 * the unused and security sectors) is dropped, so the full image cannot be rebuilt from an
 * XISO: it is for playing (xemu, Xenia), not for archiving. Files are copied byte for byte;
 * extract-xiso's default patching of .xbe media flags is off.
 *
 * extract-xiso keeps its state in globals, so calls are serialised: one rewrite at a time per
 * process, and others wait. Paths are passed to the C runtime's open() as they are (UTF-8 on
 * Linux and macOS; the ANSI code page on Windows).
 */
#ifndef EXISO_H
#define EXISO_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  define EXISO_API __declspec(dllexport)
#else
#  define EXISO_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any incompatible change to this header; src/rom/xbox.rs checks it. */
#define EXISO_ABI_VERSION 2

enum exiso_status {
    EXISO_OK = 0,
    EXISO_ERR_ARGS = -1,    /* a NULL or empty argument */
    EXISO_ERR_OPEN = -2,    /* the input could not be opened, or holds no XDVDFS filesystem */
    EXISO_ERR_EMPTY = -3,   /* the filesystem has no files */
    EXISO_ERR_FAILED = -4,  /* reading or writing failed part way; the output is removed */
    EXISO_CANCELLED = -5,   /* the progress callback returned 0; the output is removed */
};

/* Bytes written so far, out of the file data's total, called on every write (2 MiB or less).
 * Return 0 to cancel: the next write fails, and extract-xiso stops as it does on any write
 * error. */
typedef int (*exiso_progress_fn)(uint64_t done, uint64_t total, void *user);

EXISO_API int exiso_abi_version(void);

/* Writes `in_path` (a Redump Xbox / Xbox 360 image, or an XISO) as an XISO at `out_path`,
 * created or replaced. `error`, when not NULL, receives a short English message on failure.
 * Returns an exiso_status. */
EXISO_API int exiso_rewrite(const char *in_path, const char *out_path,
                            exiso_progress_fn progress, void *user,
                            char *error, size_t error_len);

#ifdef __cplusplus
}
#endif

#endif /* EXISO_H */
