#pragma once

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  ifdef DOLPHINRVZ_BUILDING_DLL
#    define DOLPHINRVZ_API __declspec(dllexport)
#  else
#    define DOLPHINRVZ_API __declspec(dllimport)
#  endif
#else
#  define DOLPHINRVZ_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Output container format -- matches DiscIO::BlobType's PLAIN/GCZ/WIA/RVZ. */
typedef enum DolphinRvzFormat
{
    DOLPHINRVZ_FORMAT_ISO = 0,
    DOLPHINRVZ_FORMAT_GCZ = 1,
    DOLPHINRVZ_FORMAT_WIA = 2,
    DOLPHINRVZ_FORMAT_RVZ = 3,
} DolphinRvzFormat;

/* Compression method for WIA/RVZ output -- matches DiscIO::WIARVZCompressionType.
 * WIA does not support Zstd; RVZ does not support Purge (same restriction as the
 * Dolphin GUI/CLI). */
typedef enum DolphinRvzCompression
{
    DOLPHINRVZ_COMPRESSION_NONE = 0,
    DOLPHINRVZ_COMPRESSION_PURGE = 1,
    DOLPHINRVZ_COMPRESSION_BZIP2 = 2,
    DOLPHINRVZ_COMPRESSION_LZMA = 3,
    DOLPHINRVZ_COMPRESSION_LZMA2 = 4,
    DOLPHINRVZ_COMPRESSION_ZSTD = 5,
} DolphinRvzCompression;

/* Called periodically during conversion with a status message and progress in [0,100].
 * Return 0 to cancel the conversion, nonzero to continue. May be NULL. */
typedef int (*DolphinRvzProgressCb)(const char* text, float percent, void* user_data);

typedef struct DolphinRvzConvertOptions
{
    /* Required. Path to the source disc image (ISO/GCZ/WIA/RVZ/CISO/NFS/etc -- anything
     * DiscIO::CreateBlobReader recognizes). */
    const char* input_path;

    /* Required. Path to write the converted file to. */
    const char* output_path;

    /* Required. Output container format. */
    DolphinRvzFormat format;

    /* Optional. Dolphin user directory for temporary processing files, created if it
     * doesn't exist. Pass NULL or "" to use Dolphin's normal per-platform default user
     * directory. */
    const char* user_dir;

    /* Optional (default: false/0). Scrub junk data during conversion. Only valid for
     * GameCube/Wii disc images; ignored (with a warning in the log) otherwise. */
    int scrub;

    /* Required for GCZ/WIA/RVZ, ignored for ISO. Block size in bytes -- see
     * DOLPHINRVZ_PREFERRED_MIN_BLOCK_SIZE / DOLPHINRVZ_PREFERRED_MAX_BLOCK_SIZE below.
     * Suggested value for RVZ: 131072 (128 KiB). */
    int block_size;

    /* Required for WIA/RVZ, ignored otherwise. */
    DolphinRvzCompression compression;

    /* Required for WIA/RVZ when compression is not NONE, ignored otherwise. Valid range
     * depends on the compression method -- see dolphinrvz_get_allowed_compression_levels. */
    int compression_level;

    /* Optional. May be NULL. */
    DolphinRvzProgressCb on_progress;
    void* user_data;
} DolphinRvzConvertOptions;

enum
{
    DOLPHINRVZ_PREFERRED_MIN_BLOCK_SIZE = 0x8000,
    DOLPHINRVZ_PREFERRED_MAX_BLOCK_SIZE = 0x200000,
};

/* Runs the conversion described by *options, synchronously on the calling thread.
 * Returns 0 on success, negative on failure -- see dolphinrvz_get_last_error() for a
 * human-readable message (including validation failures: missing required fields,
 * invalid block size/compression level for the chosen format, etc -- the same checks
 * Dolphin's own convert CLI performs). A return of exactly -6 specifically means
 * on_progress returned 0 (the caller cancelled) -- check for that value if you need to
 * distinguish cancellation from every other failure.
 *
 * Not safe to call concurrently with itself on multiple threads at once (shares
 * process-wide DiscIO/Common state). Serialize calls, e.g. one thread per conversion.
 */
DOLPHINRVZ_API int dolphinrvz_convert(const DolphinRvzConvertOptions* options);

/* Message for the most recent failed dolphinrvz_convert call on this thread. */
DOLPHINRVZ_API const char* dolphinrvz_get_last_error(void);

/* Returns the [min, max] compression_level range accepted by dolphinrvz_convert for the
 * given compression method. */
DOLPHINRVZ_API void dolphinrvz_get_allowed_compression_levels(DolphinRvzCompression compression,
                                                                int* out_min, int* out_max);

#ifdef DOLPHINRVZ_WITH_STREAMING
/*
 * ---------------------------------------------------------------------------------------
 * Streaming build only (user/scripts/build_dolphin_rvz.sh's default). A library exports
 * these iff it was built that way, so callers detect support by looking up
 * dolphinrvz_extract_stream / dolphinrvz_reader_open. Failures set
 * dolphinrvz_get_last_error() like dolphinrvz_convert does.
 * ---------------------------------------------------------------------------------------
 */

/*
 * One block of a file, in order: blocks of one file arrive with contiguous offsets from 0.
 * A disc image is a single file, so file_index is always 0 here (the parameter mirrors
 * the other libraries' callbacks, which can report several output files). Return 0 to
 * stop, nonzero to continue.
 */
typedef int (*DolphinRvzDataCb)(uint32_t file_index, const char* file_name, uint64_t offset,
                                const void* data, uint32_t size, void* user_data);

/*
 * Decodes any disc image DiscIO reads (RVZ/WIA/GCZ/ISO/CISO/WBFS/...) to its plain ISO
 * bytes in one pass, handing every block to on_data and, if output_path is non-NULL, also
 * writing it there. The bytes (and the file) are identical to dolphinrvz_convert with
 * DOLPHINRVZ_FORMAT_ISO: same read loop and chunking as DiscIO::ConvertToPlain.
 * file_name is output_path, or "" when streaming only.
 *
 * user_dir: as in DolphinRvzConvertOptions. Returns 0, negative on failure, -6 if on_data
 * or on_progress returned 0 (a partial output file is deleted). Same threading rule as
 * dolphinrvz_convert.
 */
DOLPHINRVZ_API int dolphinrvz_extract_stream(const char* input_path, const char* output_path,
                                             const char* user_dir, DolphinRvzDataCb on_data,
                                             DolphinRvzProgressCb on_progress, void* user_data);

/*
 * dolphinrvz_convert, additionally handing the INPUT disc image's plain bytes to
 * on_input_data -- e.g. to hash an ISO while it is being compressed to RVZ, without a
 * second read pass. Every byte of the input's data is delivered exactly once, in order
 * (file_index 0, file_name = input_path), even though the converter itself reads out of
 * order and partly through a separate reader: re-reads are dropped and anything it skips is
 * read and delivered in between, so the stream is always complete. (The converted OUTPUT
 * can't be streamed: GCZ/WIA/RVZ writers go back and patch their headers at the end.)
 *
 * Returning 0 from on_input_data cancels the conversion at its next progress point (-6).
 */
DOLPHINRVZ_API int dolphinrvz_convert_stream(const DolphinRvzConvertOptions* options,
                                             DolphinRvzDataCb on_input_data);

/*
 * A disc image (plain ISO bytes) supplied by the caller instead of a file. Give `read`
 * (sequential), `read_at` (random access), or both; with both, `read_at` is used.
 *
 *   read:    copy up to `size` next bytes into buf; return the count (0 = end), <0 = error.
 *   read_at: copy exactly `size` bytes at `offset` into buf; return 0, nonzero = error.
 *            May be called from Dolphin's worker threads, but never concurrently.
 */
typedef struct {
    uint64_t size;  /* exact plain size */
    int64_t (*read)(void* user_data, void* buf, uint64_t size);
    int (*read_at)(void* user_data, uint64_t offset, void* buf, uint64_t size);
    void* user_data;
} DolphinRvzSource;

/*
 * dolphinrvz_convert with the input from `source` (options->input_path is then only a name
 * for on_input_data and may be NULL; options->scrub is not supported). on_input_data, if
 * non-NULL, receives every input byte once, in order, as in dolphinrvz_convert_stream.
 *
 * GCZ/WIA/RVZ creation reads its input at arbitrary offsets, so a sequential-only source
 * is first spooled to a temporary file (in options->user_dir, else the system temp
 * directory), which is deleted afterwards; a read_at source is used directly.
 * Returns as dolphinrvz_convert.
 */
DOLPHINRVZ_API int dolphinrvz_convert_from_source(const DolphinRvzConvertOptions* options,
                                                  const DolphinRvzSource* source,
                                                  DolphinRvzDataCb on_input_data);

/*
 * Number of compression threads GCZ/WIA/RVZ creation uses from now on (process-wide, this
 * library only); 0 = one per logical CPU (the default). Applies on Linux and macOS; ignored on
 * Windows, where Dolphin's thread count can't be changed from outside.
 */
DOLPHINRVZ_API void dolphinrvz_set_compress_threads(uint32_t threads);

/*
 * Random access to a disc image's plain (ISO) contents without converting it -- e.g.
 * reading a GC/Wii filesystem out of an RVZ. Each reader owns its own file handle and
 * decoder state; for parallel reads open one reader per thread. A single reader is not
 * thread-safe.
 */
typedef struct DolphinRvzReader DolphinRvzReader;

typedef struct {
    uint64_t data_size;         /* plain (ISO) size */
    uint64_t raw_size;          /* size of the file on disk */
    uint64_t block_size;        /* container block size, 0 if the format has none */
    int32_t blob_type;          /* DiscIO::BlobType: 0 PLAIN, 3 GCZ, 7 WIA, 8 RVZ, ... */
    int32_t data_size_accurate; /* 0 if data_size is only a bound (e.g. some WBFS) */
} DolphinRvzReaderInfo;

/* user_dir: as in DolphinRvzConvertOptions. NULL on failure. */
DOLPHINRVZ_API DolphinRvzReader* dolphinrvz_reader_open(const char* path, const char* user_dir);
DOLPHINRVZ_API void dolphinrvz_reader_close(DolphinRvzReader* reader);
DOLPHINRVZ_API int dolphinrvz_reader_get_info(DolphinRvzReader* reader, DolphinRvzReaderInfo* out);
/* Reads `size` plain bytes at `offset`. 0 on success. */
DOLPHINRVZ_API int dolphinrvz_reader_read(DolphinRvzReader* reader, uint64_t offset, void* buffer,
                                          uint64_t size);
#endif /* DOLPHINRVZ_WITH_STREAMING */

#ifdef __cplusplus
}
#endif
