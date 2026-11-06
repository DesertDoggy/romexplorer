/*
 * cemuwua -- Wii U disc images (.wud / .wux) and title folders into Cemu's .wua archive,
 * and the same ZArchive format for any files the caller supplies (Xenia's .zar).
 *
 * A thin C API over Cemu's own code (external/cemu): its WUD/WUX reader, its disc partition
 * and FST decryption, its ticket/TMD parser, and the ZArchive writer Cemu uses for .wua.
 * The conversion is the one Cemu's "Convert to compressed Wii U archive (.wua)" performs:
 * every title is stored decrypted under "<title id, 16 hex digits>_v<version>/", holding its
 * code/, content/ and meta/ folders.
 *
 * Each archive cemuwua writes also carries "romexplorer.json" at its root: the SHA-1 and size
 * of every file it stored, plus whatever the caller says about the source (its DAT hashes).
 * Cemu lists only root folders named like a title, so it never sees the file. A .wua holds
 * no hash of the dump it came from, so that manifest is what lets a later scan tell that a
 * .wua still holds exactly what a verified disc held (cemuwua_wua_manifest, cemuwua_wua_stream).
 *
 * It only goes one way. A .wua keeps the decrypted files and nothing of the disc they came
 * from (its encryption, signed tickets and layout), so no .wud/.wux can be rebuilt from one.
 *
 * Strings are UTF-8. All functions are safe to call from several threads at once on
 * different files.
 */
#ifndef CEMUWUA_H
#define CEMUWUA_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  define CEMUWUA_API __declspec(dllexport)
#else
#  define CEMUWUA_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any incompatible change to this header; src/rom/cemu_wrapper.rs checks it. */
#define CEMUWUA_ABI_VERSION 2

enum cemuwua_status {
    CEMUWUA_OK = 0,
    CEMUWUA_CANCELLED = 1,        /* a callback asked to stop */
    CEMUWUA_NO_MANIFEST = 2,      /* a readable .wua without romexplorer.json (made elsewhere) */
    CEMUWUA_NOT_FOUND = 3,        /* cemuwua_read_file: the archive has no file by that path */
    CEMUWUA_ERR_ARGS = -1,        /* a NULL or empty argument */
    CEMUWUA_ERR_OPEN = -2,        /* an input could not be opened or is not a Wii U title */
    CEMUWUA_ERR_KEY = -3,         /* a disc image whose key is missing or wrong */
    CEMUWUA_ERR_READ = -4,        /* reading or decrypting failed part way (a damaged image) */
    CEMUWUA_ERR_WRITE = -5,       /* the output could not be written */
    CEMUWUA_ERR_VERIFY = -6,      /* the written archive did not read back */
};

/*
 * One title to put into the archive. `path` is one of:
 *   - a disc image, .wud or .wux. Its disc key is `key` when given (16 bytes), otherwise the
 *     first matching key from `keys_txt` (see cemuwua_create), otherwise a "<image>.key"
 *     file beside it (16 raw bytes, as dumpers write it) -- the same lookup Cemu does;
 *   - a NUS/CDN title folder (title.tmd, title.tik and the .app files); `key` is unused;
 *   - an extracted title folder holding code/, content/ and meta/ ("loadiine" layout);
 *     `key` is unused.
 * A disc holds the base game only. Its update and DLC are separate titles: pass their folders
 * as further sources and they go into the same archive, as Cemu does.
 */
typedef struct cemuwua_source {
    const char *path;
    const uint8_t *key; /* 16 bytes, or NULL */
} cemuwua_source;

/* Called as data is written: bytes of title data read so far, out of the total. Return
 * non-zero to cancel; the function then returns CEMUWUA_CANCELLED and removes the output. */
typedef int (*cemuwua_progress_fn)(uint64_t done, uint64_t total, void *user);

CEMUWUA_API int cemuwua_abi_version(void);

/* The manifest's name at the archive root. */
#define CEMUWUA_MANIFEST_NAME "romexplorer.json"

/*
 * Writes `sources` into one .wua at `out_path` (created or replaced), then reads it back to
 * check it. `keys_txt` is the path of a Cemu-style keys.txt (one hex key per line, '#' starts
 * a comment), or NULL. `source_json` is a JSON value (normally an object) stored verbatim as
 * the manifest's "source" member, or NULL for null. `error`, when not NULL, receives a short
 * English message on failure. Returns a cemuwua_status.
 *
 * The manifest:
 *   {"format":1,"tool":"cemuwua","source":<source_json>,
 *    "titles":[{"folder":"0005000010101c00_v0","title_id":"0005000010101c00","version":0,
 *               "files":[{"path":"code/app.xml","size":1234,"sha1":"<40 hex>"},...]}]}
 * "path" is relative to the title folder; files are listed in the order they are stored.
 */
CEMUWUA_API int cemuwua_create(const cemuwua_source *sources, size_t source_count,
                               const char *keys_txt, const char *source_json,
                               const char *out_path,
                               cemuwua_progress_fn progress, void *user,
                               char *error, size_t error_len);

/*
 * The title one source holds (see cemuwua_source for what `path` may be), read from its
 * code/app.xml: its 64-bit title id and version. For a disc image this also proves the key
 * works. Returns a cemuwua_status.
 */
CEMUWUA_API int cemuwua_title_info(const char *path, const uint8_t *key, const char *keys_txt,
                                   uint64_t *title_id, uint32_t *title_version,
                                   char *error, size_t error_len);

/*
 * The titles inside a .wua: calls `each` once per top-level "<title id>_v<version>" folder.
 * Returns a cemuwua_status (CEMUWUA_ERR_OPEN when it is not a readable .wua).
 */
typedef void (*cemuwua_title_fn)(uint64_t title_id, uint32_t title_version, void *user);
CEMUWUA_API int cemuwua_wua_titles(const char *wua_path, cemuwua_title_fn each, void *user,
                                   char *error, size_t error_len);

/*
 * Copies the .wua's romexplorer.json into `out` (NUL-terminated when it fits). `*needed`, when
 * not NULL, is set to its length plus one either way, so a caller can retry with a bigger
 * buffer. Returns CEMUWUA_OK, CEMUWUA_NO_MANIFEST, CEMUWUA_ERR_ARGS when `out` is too small,
 * or CEMUWUA_ERR_OPEN.
 */
CEMUWUA_API int cemuwua_wua_manifest(const char *wua_path, char *out, size_t out_len,
                                     size_t *needed, char *error, size_t error_len);

/*
 * Streams every file of every title in the .wua, each folder in name order (ZArchive keeps
 * folders sorted, so this is not necessarily the order they were written or the manifest's):
 * `begin` with its path as "<title folder>/<path in title>" and size, then `data` in chunks
 * as it is decompressed, never the whole file at once. Either callback returning non-zero
 * stops the walk with CEMUWUA_CANCELLED. Root entries that are not title folders (the
 * manifest) are skipped. Returns a cemuwua_status.
 */
typedef int (*cemuwua_file_fn)(const char *path, uint64_t size, void *user);
typedef int (*cemuwua_data_fn)(const uint8_t *data, size_t len, void *user);
CEMUWUA_API int cemuwua_wua_stream(const char *wua_path, cemuwua_file_fn begin,
                                   cemuwua_data_fn data, void *user,
                                   char *error, size_t error_len);

/*
 * Any ZArchive (.zar, as Xenia Canary reads for Xbox 360 games): an archive of files the
 * caller supplies, in the same format and with the same manifest convention as a .wua.
 *
 *   cemuwua_zar *z = cemuwua_zar_create(path, err, n);
 *   cemuwua_zar_dir(z, "media");            -- parents are made as needed
 *   cemuwua_zar_file(z, "default.xex");     -- starts a file; its parent must exist
 *   cemuwua_zar_write(z, data, len); ...    -- appends to it
 *   cemuwua_zar_finish(z, manifest, err, n) -- adds romexplorer.json (when not NULL), writes
 *                                              the index, reads the archive back, frees z
 *   cemuwua_zar_abort(z)                    -- or: stops, removes the output, frees z
 *
 * The manifest is the caller's JSON text, stored as is; cemuwua_wua_manifest reads it back
 * from a .zar too. One handle is used from one thread at a time.
 */
typedef struct cemuwua_zar cemuwua_zar;
CEMUWUA_API cemuwua_zar *cemuwua_zar_create(const char *out_path, char *error, size_t error_len);
CEMUWUA_API int cemuwua_zar_dir(cemuwua_zar *z, const char *path);
CEMUWUA_API int cemuwua_zar_file(cemuwua_zar *z, const char *path);
CEMUWUA_API int cemuwua_zar_write(cemuwua_zar *z, const uint8_t *data, size_t len);
CEMUWUA_API int cemuwua_zar_finish(cemuwua_zar *z, const char *manifest_json, char *error, size_t error_len);
CEMUWUA_API void cemuwua_zar_abort(cemuwua_zar *z);

/*
 * Streams every file of a ZArchive except the root romexplorer.json, each folder in name
 * order, with paths relative to the root ("media/x.bin"); otherwise as cemuwua_wua_stream.
 */
CEMUWUA_API int cemuwua_zar_stream(const char *zar_path, cemuwua_file_fn begin,
                                   cemuwua_data_fn data, void *user,
                                   char *error, size_t error_len);

/*
 * Reads part of one file of any ZArchive (.wua or .zar) without walking the rest: up to
 * `out_len` bytes from `offset` of the file at `inner_path` ("default.xex",
 * "0005000010101a00_v0/meta/meta.xml"), for a header that sits at a known place. `*got` is set
 * to the bytes copied and `*file_size`, when not NULL, to the file's whole size. Returns
 * CEMUWUA_OK (`*got` may be less than `out_len` at the end of the file), CEMUWUA_NOT_FOUND,
 * CEMUWUA_ERR_OPEN or CEMUWUA_ERR_READ.
 *
 * Added after ABI version 2 without bumping it: nothing earlier changed, so a caller looks the
 * symbol up and does without it when an older build lacks it.
 */
CEMUWUA_API int cemuwua_read_file(const char *archive_path, const char *inner_path,
                                  uint64_t offset, uint8_t *out, size_t out_len,
                                  size_t *got, uint64_t *file_size,
                                  char *error, size_t error_len);

/*
 * Reads a title's files in place, for playing what is inside it: `path` is a .wua (or .zar),
 * a .wud/.wux disc image (key as for cemuwua_create: `key`, then `keys_txt`, then a
 * "<image>.key" beside it), or a NUS/CDN title folder. cemuwua_reader_list calls `each` once
 * per file (path '/'-separated from the title's root, size), and cemuwua_reader_read reads
 * like cemuwua_read_file but without opening the archive again. One reader may be used from
 * several threads; calls are taken one at a time. cemuwua_reader_open returns NULL with
 * `error` filled in when the input cannot be opened.
 *
 * Added after ABI version 2 without bumping it, like cemuwua_read_file.
 */
typedef struct cemuwua_reader cemuwua_reader;
CEMUWUA_API cemuwua_reader *cemuwua_reader_open(const char *path, const uint8_t *key, const char *keys_txt,
                                                char *error, size_t error_len);
CEMUWUA_API int cemuwua_reader_list(cemuwua_reader *r, cemuwua_file_fn each, void *user);
CEMUWUA_API int cemuwua_reader_read(cemuwua_reader *r, const char *inner_path, uint64_t offset,
                                    uint8_t *out, size_t out_len, size_t *got, uint64_t *file_size);
CEMUWUA_API void cemuwua_reader_close(cemuwua_reader *r);

#ifdef __cplusplus
}
#endif

#endif /* CEMUWUA_H */
