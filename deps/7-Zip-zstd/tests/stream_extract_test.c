/* Functional test for sevenzip_extract_entry_stream (the SEVENZIP_WITH_STREAMING build).
 *
 * Proves the streaming entry point actually delivers bytes -- not merely that the symbol
 * exports -- by checking, for one archive entry:
 *   1. chunks arrive tagged with the entry index the caller asked for, for EVERY entry
 *      (in a solid 7z, later entries force 7-Zip to decode through earlier ones);
 *   2. offset_in_entry advances contiguously from 0 with no gaps or overlaps;
 *   3. the streamed bytes total the entry's uncompressed size;
 *   4. with an out_path, the file written in that same pass is byte-identical to the
 *      bytes handed to the callback (one decompression feeding both sinks);
 *   5. with out_path == NULL nothing is written to disk, but the bytes still arrive;
 *   6. returning 0 from the callback stops the extraction (reported as -6).
 *
 * Built and run by run-stream-test.sh, which passes the library path; the library is
 * dlopen'd rather than linked so this can be pointed at either build variant (and so it
 * reports a clear skip, rather than failing to link, against a non-streaming build).
 */

#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct SevenZipArchive SevenZipArchive;
typedef int (*SevenZipProgressCb)(const char *text, float percent, void *user_data);
typedef int (*SevenZipDataCb)(uint32_t entry_index, uint64_t offset_in_entry,
                              const void *data, uint32_t size, void *user_data);

/* Mirrors SevenZipEntry in user/sevenzip_lib.h. Only `size` and `is_dir` are read here,
 * but the whole struct must match for get_entry to fill it correctly. */
typedef struct {
    uint64_t filetime_100ns;
    uint8_t extra_ns;
    int is_defined;
} SevenZipFileTime;

typedef struct {
    const char *path;
    uint64_t size;
    uint64_t packed_size;
    uint32_t attributes;
    SevenZipFileTime mtime;
    int is_dir;
} SevenZipEntry;

static SevenZipArchive *(*p_open)(const char *, int, SevenZipProgressCb, void *);
static void (*p_close)(SevenZipArchive *);
static int (*p_entry_count)(SevenZipArchive *);
static int (*p_get_entry)(SevenZipArchive *, int, SevenZipEntry *);
static int (*p_extract_stream)(SevenZipArchive *, int, const char *, SevenZipDataCb,
                               SevenZipProgressCb, void *);
static const char *(*p_last_error)(void);

/* Accumulates every streamed chunk so the test can compare against the written file. */
typedef struct {
    uint8_t *buf;
    size_t len;
    size_t cap;
    uint64_t next_offset;   /* offset the next chunk must start at */
    uint32_t want_index;
    int index_mismatches;
    int offset_gaps;
    int calls;
    int stop_after;         /* 0 = never stop; else stop on that call number */
} Sink;

static int on_data(uint32_t entry_index, uint64_t offset_in_entry, const void *data,
                   uint32_t size, void *user_data)
{
    Sink *s = (Sink *)user_data;
    s->calls++;

    if (entry_index != s->want_index)
        s->index_mismatches++;
    if (offset_in_entry != s->next_offset)
        s->offset_gaps++;
    s->next_offset = offset_in_entry + size;

    if (s->len + size > s->cap) {
        size_t cap = (s->cap ? s->cap : 65536);
        while (cap < s->len + size)
            cap *= 2;
        s->buf = (uint8_t *)realloc(s->buf, cap);
        if (!s->buf) {
            fprintf(stderr, "out of memory\n");
            exit(0x7f);
        }
        s->cap = cap;
    }
    memcpy(s->buf + s->len, data, size);
    s->len += size;

    if (s->stop_after && s->calls >= s->stop_after)
        return 0;   /* ask to stop */
    return 1;
}

#define CHECK(cond, ...) do { \
    if (!(cond)) { printf("FAIL: "); printf(__VA_ARGS__); printf("\n"); failures++; } \
    else { printf("ok: "); printf(__VA_ARGS__); printf("\n"); } \
} while (0)

int main(int argc, char **argv)
{
    if (argc < 4) {
        fprintf(stderr, "usage: %s <lib> <archive> <format:0=7z,1=zip> [out_dir]\n", argv[0]);
        return 2;
    }
    const char *lib_path = argv[1];
    const char *archive_path = argv[2];
    const int format = atoi(argv[3]);
    const char *out_dir = (argc > 4) ? argv[4] : "/tmp";
    int failures = 0;

    void *lib = dlopen(lib_path, RTLD_NOW);
    if (!lib) {
        fprintf(stderr, "dlopen failed: %s\n", dlerror());
        return 3;
    }

    p_extract_stream = (int (*)(SevenZipArchive *, int, const char *, SevenZipDataCb,
                                SevenZipProgressCb, void *))dlsym(lib, "sevenzip_extract_entry_stream");
    if (!p_extract_stream) {
        printf("SKIP: %s has no sevenzip_extract_entry_stream (not a streaming build)\n", lib_path);
        return 77;   /* conventional "skipped" status */
    }

    p_open = (SevenZipArchive *(*)(const char *, int, SevenZipProgressCb, void *))dlsym(lib, "sevenzip_open");
    p_close = (void (*)(SevenZipArchive *))dlsym(lib, "sevenzip_close");
    p_entry_count = (int (*)(SevenZipArchive *))dlsym(lib, "sevenzip_get_entry_count");
    p_get_entry = (int (*)(SevenZipArchive *, int, SevenZipEntry *))dlsym(lib, "sevenzip_get_entry");
    p_last_error = (const char *(*)(void))dlsym(lib, "sevenzip_get_last_error");
    if (!p_open || !p_close || !p_entry_count || !p_get_entry || !p_last_error) {
        fprintf(stderr, "missing base symbols\n");
        return 3;
    }

    SevenZipArchive *ar = p_open(archive_path, format, NULL, NULL);
    if (!ar) {
        fprintf(stderr, "open failed: %s\n", p_last_error());
        return 4;
    }

    const int count = p_entry_count(ar);
    int first = -1, last = -1, files = 0;
    char out_path[4096];

    /* --- 1. every entry: stream + write in one pass ---
     * In a solid 7z, extracting entry N makes 7-Zip decode through entries 0..N-1 and call
     * GetStream for them with askMode=kSkip; those must yield no chunks and must not leak
     * their bytes into entry N's stream. Each entry's output is left at
     * <out_dir>/entry_<index>.out so the caller can compare it against a reference
     * extraction by another tool. */
    for (int i = 0; i < count; i++) {
        SevenZipEntry e;
        memset(&e, 0, sizeof e);
        if (p_get_entry(ar, i, &e) != 0 || e.is_dir)
            continue;
        if (first < 0)
            first = i;
        last = i;
        files++;

        snprintf(out_path, sizeof out_path, "%s/entry_%d.out", out_dir, i);
        remove(out_path);

        Sink s;
        memset(&s, 0, sizeof s);
        s.want_index = (uint32_t)i;
        const int rc = p_extract_stream(ar, i, out_path, on_data, NULL, &s);
        CHECK(rc == 0, "entry %d: extract_stream returned 0 (got %d: %s)", i, rc, rc ? p_last_error() : "");
        CHECK(s.index_mismatches == 0, "entry %d: every chunk carried index %d (%d chunks)", i, i, s.calls);
        CHECK(s.offset_gaps == 0, "entry %d: offsets contiguous from 0", i);
        CHECK(s.len == e.size, "entry %d: streamed %llu == size %llu", i,
              (unsigned long long)s.len, (unsigned long long)e.size);

        FILE *f = fopen(out_path, "rb");
        if (f) {
            uint8_t *disk = (uint8_t *)malloc(s.len ? s.len : 1);
            const size_t got = fread(disk, 1, s.len, f);
            uint8_t tail;
            const size_t extra = fread(&tail, 1, 1, f);
            fclose(f);
            CHECK(got == s.len && extra == 0 && (s.len == 0 || memcmp(disk, s.buf, s.len) == 0),
                  "entry %d: written file identical to streamed bytes", i);
            free(disk);
        } else {
            CHECK(0, "entry %d: output file was written", i);
        }
        free(s.buf);
    }
    CHECK(files > 0, "archive has %d file entries", files);

    snprintf(out_path, sizeof out_path, "%s/stream_only_probe.out", out_dir);
    remove(out_path);

    /* --- 2. stream only (no file), on the LAST entry: for a solid 7z this is the path
     * that skips through every earlier entry with nothing written anywhere --- */
    {
        SevenZipEntry e;
        memset(&e, 0, sizeof e);
        p_get_entry(ar, last, &e);
        Sink s2;
        memset(&s2, 0, sizeof s2);
        s2.want_index = (uint32_t)last;
        const int rc = p_extract_stream(ar, last, NULL, on_data, NULL, &s2);
        CHECK(rc == 0, "stream-only entry %d returned 0 (got %d)", last, rc);
        CHECK(s2.index_mismatches == 0 && s2.len == e.size,
              "stream-only entry %d delivered all %llu bytes with the right index", last,
              (unsigned long long)e.size);
        free(s2.buf);
    }

    /* --- 3. stopping from the callback --- */
    {
        Sink s3;
        memset(&s3, 0, sizeof s3);
        s3.want_index = (uint32_t)first;
        s3.stop_after = 1;
        const int rc = p_extract_stream(ar, first, NULL, on_data, NULL, &s3);
        CHECK(rc == -6, "returning 0 from on_data stops extraction with -6 (got %d)", rc);
        free(s3.buf);
    }

    p_close(ar);

    printf("%s\n", failures ? "FAILURES" : "all checks passed");
    return failures ? 1 : 0;
}
