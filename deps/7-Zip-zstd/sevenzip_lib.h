#pragma once

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  ifdef SEVENZIP_BUILDING_DLL
#    define SEVENZIP_API __declspec(dllexport)
#  else
#    define SEVENZIP_API __declspec(dllimport)
#  endif
#else
#  define SEVENZIP_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Container format. This build only registers Zip and 7z handlers (see
 * README.sevenzip-lib.md) -- no Rar/Tar/Iso/Wim/Cab/etc, and no password/encryption
 * support in either direction (an encrypted archive fails to open/extract with a
 * clear error instead of prompting for a password). */
typedef enum SevenZipFormat
{
    SEVENZIP_FORMAT_7Z = 0,
    SEVENZIP_FORMAT_ZIP = 1,
} SevenZipFormat;

/* Compression method for archive creation. Reading supports whatever method(s) an
 * existing archive already uses (LZMA/LZMA2/PPMd/Deflate/Deflate64/BZip2/Zstd/Copy,
 * plus BCJ/BCJ2/Delta filter chains inside 7z) -- this enum only constrains what
 * sevenzip_create_archive can produce.
 *
 * SEVENZIP_METHOD_LZMA2 is valid only with SEVENZIP_FORMAT_7Z: plain ZIP has no
 * standard LZMA2 method id (only plain LZMA, method 14), so sevenzip_create_archive
 * rejects format=ZIP + method=LZMA2 itself, up front, with a clear error --
 * it does not depend on however native 7-Zip happens to fail that combination. */
typedef enum SevenZipMethod
{
    SEVENZIP_METHOD_COPY = 0,
    SEVENZIP_METHOD_DEFLATE = 1,
    SEVENZIP_METHOD_DEFLATE64 = 2,
    SEVENZIP_METHOD_BZIP2 = 3,
    SEVENZIP_METHOD_LZMA = 4,
    SEVENZIP_METHOD_LZMA2 = 5,
    SEVENZIP_METHOD_ZSTD = 6,
} SevenZipMethod;

/* Last-modified time at full precision, mirroring 7-Zip's own internal CArcTime
 * representation rather than collapsing it to Unix seconds (which would silently
 * discard everything below 1-second resolution). */
typedef struct SevenZipFileTime
{
    /* Windows FILETIME: 100ns ticks since 1601-01-01 UTC. This is the value's primary
     * precision; 0 when is_defined is 0. */
    uint64_t filetime_100ns;

    /* Additional nanoseconds (0-99) within that last 100ns tick, when the archive
     * and source filesystem recorded finer-than-100ns precision (e.g. ext4/btrfs
     * nanosecond mtimes) -- mirrors 7-Zip's own CArcTime::Ns100 field 1:1. 0 when no
     * finer precision than the 100ns tick itself is available. */
    uint8_t extra_ns;

    /* Nonzero if this entry actually has a modification time recorded at all. When 0,
     * the other two fields are both 0 and should not be treated as meaningful. */
    int is_defined;
} SevenZipFileTime;

/* One entry's metadata, as returned by sevenzip_get_entry. Paths use '/' separators
 * and are UTF-8, relative to the archive root. */
typedef struct SevenZipEntry
{
    const char* path;         /* Owned by the archive handle; valid until sevenzip_close. */
    uint64_t size;             /* Uncompressed size in bytes. */
    uint64_t packed_size;      /* Compressed size in bytes, 0 if the archive doesn't report it. */
    uint32_t attributes;       /* Raw Windows-style file attributes (FILE_ATTRIBUTE_*) as stored
                                 * in the archive; POSIX-only archives may report 0. */
    SevenZipFileTime mtime;    /* Last-modified time, full precision -- see SevenZipFileTime. */
    int is_dir;                /* Nonzero if this entry is a directory. */
} SevenZipEntry;

/* Called periodically during an open/extract/create call with a status message
 * (e.g. the entry currently being processed) and progress in [0,100]. Return 0 to
 * cancel the operation, nonzero to continue. May be NULL. */
typedef int (*SevenZipProgressCb)(const char* text, float percent, void* user_data);

typedef struct SevenZipArchive SevenZipArchive;

/* Opens an existing archive for reading. format is the container format to try --
 * callers that don't already know it (e.g. from the file extension) should try
 * SEVENZIP_FORMAT_7Z then SEVENZIP_FORMAT_ZIP, or vice versa.
 *
 * Returns NULL on failure -- see sevenzip_get_last_error(). Solid 7z archives are
 * read transparently like any other (only archive *creation* is always non-solid --
 * see sevenzip_create_archive). Encrypted archives fail to open.
 */
SEVENZIP_API SevenZipArchive* sevenzip_open(const char* path, SevenZipFormat format,
                                              SevenZipProgressCb on_progress, void* user_data);

/* Closes an archive handle opened by sevenzip_open. Safe to call with NULL. */
SEVENZIP_API void sevenzip_close(SevenZipArchive* archive);

/* Number of entries in an open archive (files and directories). */
SEVENZIP_API int sevenzip_get_entry_count(SevenZipArchive* archive);

/* Fills *out_entry with entry `index`'s metadata (0 <= index < sevenzip_get_entry_count).
 * Returns 0 on success, negative on failure. */
SEVENZIP_API int sevenzip_get_entry(SevenZipArchive* archive, int index, SevenZipEntry* out_entry);

/* Extracts entry `index` to a new file at out_path (overwritten if it already exists;
 * parent directories are created as needed). Returns 0 on success, negative on
 * failure -- see sevenzip_get_last_error(). */
SEVENZIP_API int sevenzip_extract_entry_to_file(SevenZipArchive* archive, int index,
                                                  const char* out_path,
                                                  SevenZipProgressCb on_progress, void* user_data);

/* Extracts entry `index` into memory. On success (return 0), *out_data is a newly
 * allocated buffer of *out_len bytes that the caller must release with
 * sevenzip_free_buffer(); on failure neither is modified. Intended for the "pull one
 * small metadata file out of an archive" case -- extracting a very large entry this
 * way holds the whole decompressed entry in memory at once. */
SEVENZIP_API int sevenzip_extract_entry_to_buffer(SevenZipArchive* archive, int index,
                                                    uint8_t** out_data, size_t* out_len,
                                                    SevenZipProgressCb on_progress, void* user_data);

/* Releases a buffer returned by sevenzip_extract_entry_to_buffer. Safe to call with NULL. */
SEVENZIP_API void sevenzip_free_buffer(uint8_t* data);

#ifdef SEVENZIP_WITH_STREAMING
/* ---------------------------------------------------------------------------
 * Streaming extraction (only in a library built with SEVENZIP_WITH_STREAMING --
 * user/scripts/build.sh builds this by default, to
 * user/release/with_streaming/<platform>/<arch>/<version>/). The --nostreaming build
 * exports none of the symbols below, so a caller resolving them dynamically can
 * use their presence to detect a streaming-capable library.
 * --------------------------------------------------------------------------- */

/* Called with each chunk of an entry's decompressed bytes as 7-Zip produces it,
 * before the data is discarded. `entry_index` is the archive entry the chunk
 * belongs to (the same index sevenzip_get_entry takes), so a caller extracting
 * several entries can route chunks to the right per-file hasher/writer.
 * `offset_in_entry` is the running byte offset within that entry, starting at 0.
 *
 * Return 0 to stop the extraction (reported as the "Cancelled" code, -6), nonzero
 * to continue -- matching SevenZipProgressCb's convention. May be NULL, in which
 * case sevenzip_extract_entry_stream behaves like the non-streaming call. */
typedef int (*SevenZipDataCb)(uint32_t entry_index, uint64_t offset_in_entry,
                              const void* data, uint32_t size, void* user_data);

/* Extracts entry `index`, handing every decompressed chunk to on_data as it is
 * produced rather than only after the whole entry has been written.
 *
 * If out_path is non-NULL the entry is ALSO written there, exactly as
 * sevenzip_extract_entry_to_file would -- one decompression pass feeds both the
 * file and the callback, so hashing while extracting costs no second read.
 * If out_path is NULL nothing is written to disk: the bytes reach on_data and are
 * then discarded, for hashing an entry without materializing it.
 *
 * Returns 0 on success, negative on failure (-6 if on_data or on_progress asked to
 * stop) -- see sevenzip_get_last_error(). */
SEVENZIP_API int sevenzip_extract_entry_stream(SevenZipArchive* archive, int index,
                                                 const char* out_path,
                                                 SevenZipDataCb on_data,
                                                 SevenZipProgressCb on_progress,
                                                 void* user_data);
#endif /* SEVENZIP_WITH_STREAMING */

typedef struct SevenZipCreateOptions
{
    /* Required. Container format to write. */
    SevenZipFormat format;

    /* Required. Compression method -- see SevenZipMethod's doc comment for the
     * per-format compatibility caveat. */
    SevenZipMethod method;

    /* Compression level, 0 (store/fastest) to 9 (ultra/slowest). */
    int level;

    /* input_paths[i] is a real filesystem path to add; archive_names[i] is the path
     * it should have inside the archive ('/'-separated, relative). Both arrays must
     * have `count` entries. Directories in input_paths are added recursively is NOT
     * performed here -- callers must expand directories into individual file entries
     * themselves and list each file explicitly, since 7-Zip's update callback drives
     * off exactly this flat list. */
    const char* const* input_paths;
    const char* const* archive_names;
    int count;

    /* Optional. May be NULL. */
    SevenZipProgressCb on_progress;
    void* user_data;
} SevenZipCreateOptions;

/* Creates a new archive at out_path from the files described by *options, synchronously
 * on the calling thread. Always non-solid (each file its own compressed block) --
 * there is no option to change this, so that any single entry can later be extracted
 * without touching the others.
 *
 * Returns 0 on success, negative on failure -- see sevenzip_get_last_error().
 */
SEVENZIP_API int sevenzip_create_archive(const char* out_path, const SevenZipCreateOptions* options);

#ifdef SEVENZIP_WITH_STREAMING
/* ---------------------------------------------------------------------------
 * Streaming build only: archive creation from caller-supplied sources, and pull-style
 * (sequential + random access) reading of one entry.
 * --------------------------------------------------------------------------- */

/*
 * One archive member whose bytes come from the caller instead of a file. Give `read`
 * (sequential), `read_at` (random access), or both; with both, `read` is used.
 *
 *   read:    copy up to `size` next bytes into buf; return the count (0 = end), <0 = error.
 *   read_at: copy exactly `size` bytes at `offset` into buf; return 0, nonzero = error.
 *
 * Either way exactly `size` bytes (the field) are consumed, from offset 0 upward --
 * 7-Zip reads its input strictly forward. Calls happen on the thread that called
 * sevenzip_create_archive_from_sources.
 */
typedef struct SevenZipSource
{
    const char* archive_name;  /* path inside the archive, '/'-separated */
    uint64_t size;             /* exact byte count */
    uint64_t mtime_filetime;   /* Windows FILETIME (100 ns since 1601); 0 = now */
    int64_t (*read)(void* user_data, void* buf, uint64_t size);
    int (*read_at)(void* user_data, uint64_t offset, void* buf, uint64_t size);
    void* user_data;
} SevenZipSource;

/*
 * sevenzip_create_archive, with members from `sources` instead of files. format, method,
 * level, on_progress and user_data come from *options; its input_paths / archive_names /
 * count are ignored. Non-solid, like sevenzip_create_archive. Returns 0, -6 if
 * on_progress cancelled, other negatives on failure (a source error included).
 */
SEVENZIP_API int sevenzip_create_archive_from_sources(const char* out_path,
                                                      const SevenZipCreateOptions* options,
                                                      const SevenZipSource* sources, int count);

/*
 * Pull-style reader for one entry, opening its own handle on the archive (so readers are
 * independent of each other and of any SevenZipArchive, and may run on separate threads).
 * Decoding runs on a background thread into a bounded buffer.
 *
 *   read:    sequential; returns bytes copied (0 = end of entry), <0 = error.
 *   read_at: random access. zip/7z data is a forward-only compressed stream, so this is
 *            emulated: a forward seek decodes and discards up to `offset`, a backward seek
 *            restarts decoding from the entry's start. Cost is O(offset), not O(1).
 *
 * A single reader is not thread-safe.
 */
typedef struct SevenZipEntryReader SevenZipEntryReader;

SEVENZIP_API SevenZipEntryReader* sevenzip_entry_reader_open(const char* archive_path,
                                                            SevenZipFormat format, int index);
SEVENZIP_API uint64_t sevenzip_entry_reader_size(SevenZipEntryReader* reader);
SEVENZIP_API int64_t sevenzip_entry_reader_read(SevenZipEntryReader* reader, void* buf,
                                                uint64_t size);
SEVENZIP_API int sevenzip_entry_reader_read_at(SevenZipEntryReader* reader, uint64_t offset,
                                               void* buf, uint64_t size);
SEVENZIP_API void sevenzip_entry_reader_close(SevenZipEntryReader* reader);
#endif /* SEVENZIP_WITH_STREAMING */

/* Message for the most recent failed call on this thread (sevenzip_open,
 * sevenzip_extract_entry_to_file/buffer, or sevenzip_create_archive). */
SEVENZIP_API const char* sevenzip_get_last_error(void);

#ifdef __cplusplus
}
#endif
