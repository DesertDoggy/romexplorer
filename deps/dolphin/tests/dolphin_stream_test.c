/* Functional test for the dolphinrvz streaming build (build_dolphin_rvz.sh's default).
 *
 * Usage: dolphin_stream_test <streaming lib> <default lib> <input disc image> <scratch dir>
 *                            [official dolphin-tool executable]
 *
 * The reference is a plain ISO conversion by the DEFAULT library (and, if given, the
 * official dolphin-tool's, which must match it).
 *
 *   1. extract_stream + write: streamed bytes == reference, contiguous offsets from 0,
 *      file_index 0 / file_name = output path, and the file written in the same pass ==
 *      reference.
 *   2. extract_stream, stream only: same bytes, no file left.
 *   3. extract_stream stop: -6, no file left.
 *   4. convert_stream to RVZ, from the reference ISO and from the input itself: the input
 *      stream == reference (every byte once, in order), and the RVZ written == what the
 *      default library's plain dolphinrvz_convert writes with the same settings.
 *   5. convert_stream stop: -6.
 *   6. reader: random reads vs the reference, then from 4 threads at once (one reader each).
 *
 * Both libraries are loaded into this one process on purpose: that also checks the two
 * variants can coexist. dlopen'd; SKIP (77) against a non-streaming build.
 */

#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

typedef int (*ProgressCb)(const char *, float, void *);
typedef int (*DataCb)(uint32_t, const char *, uint64_t, const void *, uint32_t, void *);

typedef struct {
    const char *input_path, *output_path;
    int format; /* 0 ISO, 1 GCZ, 2 WIA, 3 RVZ */
    const char *user_dir;
    int scrub, block_size, compression, compression_level;
    ProgressCb on_progress;
    void *user_data;
} ConvertOptions;

typedef struct DolphinRvzReader DolphinRvzReader;
typedef struct {
    uint64_t data_size, raw_size, block_size;
    int32_t blob_type, data_size_accurate;
} ReaderInfo;

static int (*p_convert)(const ConvertOptions *);
static int (*p_convert_default)(const ConvertOptions *);
static const char *(*p_err)(void);
static int (*p_extract)(const char *, const char *, const char *, DataCb, ProgressCb, void *);
static int (*p_convert_stream)(const ConvertOptions *, DataCb);
static DolphinRvzReader *(*p_open)(const char *, const char *);
static void (*p_close)(DolphinRvzReader *);
static int (*p_info)(DolphinRvzReader *, ReaderInfo *);
static int (*p_read)(DolphinRvzReader *, uint64_t, void *, uint64_t);

static int failures;
#define CHECK(cond, ...) do { \
    if (!(cond)) { printf("FAIL: "); printf(__VA_ARGS__); printf("\n"); failures++; } \
    else { printf("ok: "); printf(__VA_ARGS__); printf("\n"); } \
    fflush(stdout); \
} while (0)

/* Streamable 64-bit hash (8 bytes per step), for equality checks only. Chunk boundaries
 * don't matter: bytes are carried over between calls. */
typedef struct { uint64_t h, len; uint8_t carry[8]; int ncarry; } Hash;
static void hash_init(Hash *s) { s->h = 0x9E3779B97F4A7C15ULL; s->len = 0; s->ncarry = 0; }
static void hash_word(Hash *s, uint64_t w) { s->h ^= w; s->h *= 0x100000001B3ULL; s->h ^= s->h >> 29; }
static void hash_update(Hash *s, const void *data, size_t n)
{
    const uint8_t *p = data;
    s->len += n;
    while (n && s->ncarry) { s->carry[s->ncarry++] = *p++; n--; if (s->ncarry == 8) { uint64_t w; memcpy(&w, s->carry, 8); hash_word(s, w); s->ncarry = 0; } }
    for (; n >= 8; n -= 8, p += 8) { uint64_t w; memcpy(&w, p, 8); hash_word(s, w); }
    while (n--) s->carry[s->ncarry++] = *p++;
}
static uint64_t hash_final(Hash *s)
{
    uint64_t h = s->h;
    for (int i = 0; i < s->ncarry; i++) { h ^= s->carry[i]; h *= 0x100000001B3ULL; }
    return h ^ s->len;
}

static int hash_file(const char *path, uint64_t *out, uint64_t *size)
{
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    static uint8_t buf[1 << 22];
    Hash s; hash_init(&s);
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) hash_update(&s, buf, n);
    fclose(f);
    *out = hash_final(&s); *size = s.len;
    return 0;
}

static int file_exists(const char *p) { struct stat st; return stat(p, &st) == 0; }

typedef struct {
    Hash hash;
    uint64_t next;
    int gaps, bad_index, bad_name, stop_now;
    const char *want_name;
} Sink;

static int on_data(uint32_t idx, const char *name, uint64_t off, const void *data, uint32_t size, void *ud)
{
    Sink *s = ud;
    if (s->stop_now) return 0;
    if (idx != 0) s->bad_index++;
    if (s->want_name && strcmp(name, s->want_name) != 0) s->bad_name++;
    if (off != s->next) s->gaps++;
    s->next = off + size;
    hash_update(&s->hash, data, size);
    return 1;
}

static void sink_init(Sink *s, const char *want_name) { memset(s, 0, sizeof *s); hash_init(&s->hash); s->want_name = want_name; }

static const char *g_user_dir;

static int convert_plain(int (*fn)(const ConvertOptions *), const char *in, const char *out, int format)
{
    ConvertOptions o;
    memset(&o, 0, sizeof o);
    o.input_path = in; o.output_path = out; o.format = format; o.user_dir = g_user_dir;
    o.block_size = 131072; o.compression = 5 /* zstd */; o.compression_level = 5;
    unlink(out);
    return fn(&o);
}

/* ---- reader ---- */
typedef struct { const char *image, *ref; uint64_t size; unsigned seed; int iterations, bad; } ReadJob;

static void *random_reads(void *arg)
{
    ReadJob *j = arg;
    DolphinRvzReader *r = p_open(j->image, g_user_dir);
    FILE *ref = fopen(j->ref, "rb");
    if (!r || !ref) { j->bad++; if (r) p_close(r); if (ref) fclose(ref); return NULL; }
    uint8_t *a = malloc(1 << 20), *b = malloc(1 << 20);
    for (int i = 0; i < j->iterations; i++) {
        uint64_t off = (((uint64_t)rand_r(&j->seed) << 31) ^ (uint64_t)rand_r(&j->seed)) % j->size;
        uint64_t len = 1 + (uint64_t)rand_r(&j->seed) % ((1 << 20) - 1);
        if (off + len > j->size) len = j->size - off;
        if (p_read(r, off, a, len) || fseeko(ref, (off_t)off, SEEK_SET) || fread(b, 1, len, ref) != len || memcmp(a, b, len))
            j->bad++;
    }
    free(a); free(b);
    fclose(ref);
    p_close(r);
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc < 5) {
        fprintf(stderr, "usage: %s <streaming lib> <default lib> <input disc image> <scratch dir> [dolphin-tool]\n", argv[0]);
        return 2;
    }
    void *lib = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL), *def = dlopen(argv[2], RTLD_NOW | RTLD_LOCAL);
    if (!lib || !def) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 3; }
    *(void **)&p_extract = dlsym(lib, "dolphinrvz_extract_stream");
    *(void **)&p_convert_stream = dlsym(lib, "dolphinrvz_convert_stream");
    *(void **)&p_open = dlsym(lib, "dolphinrvz_reader_open");
    if (!p_extract || !p_convert_stream || !p_open) { printf("SKIP: %s is not a streaming build\n", argv[1]); return 77; }
    *(void **)&p_convert = dlsym(lib, "dolphinrvz_convert");
    *(void **)&p_convert_default = dlsym(def, "dolphinrvz_convert");
    *(void **)&p_err = dlsym(lib, "dolphinrvz_get_last_error");
    *(void **)&p_close = dlsym(lib, "dolphinrvz_reader_close");
    *(void **)&p_info = dlsym(lib, "dolphinrvz_reader_get_info");
    *(void **)&p_read = dlsym(lib, "dolphinrvz_reader_read");

    const char *input = argv[3], *scratch = argv[4];
    char ref[2048], out[2048], rvz_a[2048], rvz_b[2048], user_dir[2048];
    mkdir(scratch, 0755);
    snprintf(user_dir, sizeof user_dir, "%s/dolphin_user", scratch);
    g_user_dir = user_dir;
    snprintf(ref, sizeof ref, "%s/ref.iso", scratch);
    snprintf(out, sizeof out, "%s/stream.iso", scratch);

    /* reference */
    CHECK(convert_plain(p_convert_default, input, ref, 0) == 0, "reference ISO with the default build");
    uint64_t ref_hash, ref_size;
    hash_file(ref, &ref_hash, &ref_size);
    printf("reference: %llu bytes\n", (unsigned long long)ref_size);
    if (argc > 5) {
        char off_iso[2048], cmd[8192];
        uint64_t h, n;
        snprintf(off_iso, sizeof off_iso, "%s/official.iso", scratch);
        unlink(off_iso);
        snprintf(cmd, sizeof cmd, "'%s' convert -i '%s' -o '%s' -f iso > /dev/null 2>&1", argv[5], input, off_iso);
        CHECK(system(cmd) == 0 && hash_file(off_iso, &h, &n) == 0 && h == ref_hash && n == ref_size,
              "default build's ISO identical to official dolphin-tool's");
        unlink(off_iso);
    }

    /* 1. extract_stream + write */
    Sink s; sink_init(&s, out);
    unlink(out);
    CHECK(p_extract(input, out, user_dir, on_data, NULL, &s) == 0, "extract_stream+write returned 0 (%s)", p_err());
    CHECK(s.gaps == 0 && s.bad_index == 0 && s.bad_name == 0, "offsets contiguous, file_index 0, file_name = output path");
    CHECK(hash_final(&s.hash) == ref_hash && s.next == ref_size, "streamed bytes == reference");
    uint64_t h, n;
    CHECK(hash_file(out, &h, &n) == 0 && h == ref_hash && n == ref_size, "written file == reference");
    unlink(out);

    /* 2. stream only */
    sink_init(&s, "");
    CHECK(p_extract(input, NULL, user_dir, on_data, NULL, &s) == 0 && hash_final(&s.hash) == ref_hash,
          "stream-only bytes == reference");
    CHECK(!file_exists(out), "stream-only wrote no file");

    /* 3. stop */
    sink_init(&s, NULL); s.stop_now = 1;
    CHECK(p_extract(input, out, user_dir, on_data, NULL, &s) == -6, "stop from on_data -> -6");
    CHECK(!file_exists(out), "stopped extraction left no file");

    /* 4. convert_stream to RVZ: from the reference ISO, then from the input itself */
    const char *sources[2] = { ref, input };
    const char *labels[2] = { "ISO -> RVZ", "input -> RVZ" };
    snprintf(rvz_a, sizeof rvz_a, "%s/stream.rvz", scratch);
    snprintf(rvz_b, sizeof rvz_b, "%s/plain.rvz", scratch);
    for (int k = 0; k < 2; k++) {
        ConvertOptions o;
        memset(&o, 0, sizeof o);
        o.input_path = sources[k]; o.output_path = rvz_a; o.format = 3; o.user_dir = user_dir;
        o.block_size = 131072; o.compression = 5; o.compression_level = 5;
        sink_init(&s, sources[k]);
        o.user_data = &s;
        unlink(rvz_a);
        CHECK(p_convert_stream(&o, on_data) == 0, "%s: convert_stream returned 0 (%s)", labels[k], p_err());
        CHECK(s.gaps == 0 && s.bad_index == 0 && s.bad_name == 0, "%s: input stream contiguous, file_name = input", labels[k]);
        CHECK(hash_final(&s.hash) == ref_hash && s.next == ref_size, "%s: input stream == reference ISO", labels[k]);
        uint64_t ha, na, hb, nb;
        CHECK(convert_plain(p_convert_default, sources[k], rvz_b, 3) == 0 && hash_file(rvz_a, &ha, &na) == 0 &&
              hash_file(rvz_b, &hb, &nb) == 0 && ha == hb && na == nb,
              "%s: RVZ identical to the default build's plain convert (%llu bytes)", labels[k], (unsigned long long)na);
    }

    /* 5. convert_stream stop */
    {
        ConvertOptions o;
        memset(&o, 0, sizeof o);
        o.input_path = input; o.output_path = rvz_a; o.format = 3; o.user_dir = user_dir;
        o.block_size = 131072; o.compression = 5; o.compression_level = 5;
        sink_init(&s, NULL); s.stop_now = 1;
        o.user_data = &s;
        unlink(rvz_a);
        CHECK(p_convert_stream(&o, on_data) == -6, "convert_stream: stop from on_input_data -> -6");
    }
    unlink(rvz_a); unlink(rvz_b);

    /* 6. reader */
    DolphinRvzReader *r = p_open(input, user_dir);
    CHECK(r != NULL, "reader opens");
    if (r) {
        ReaderInfo info;
        p_info(r, &info);
        printf("info: data=%llu raw=%llu block=%llu type=%d accurate=%d\n", (unsigned long long)info.data_size,
               (unsigned long long)info.raw_size, (unsigned long long)info.block_size, info.blob_type, info.data_size_accurate);
        CHECK(info.data_size == ref_size, "reader data_size == reference size");
        p_close(r);
        ReadJob one = { input, ref, ref_size, 1, 200, 0 };
        random_reads(&one);
        CHECK(one.bad == 0, "200 random reads, 1 thread: %d bad", one.bad);
        ReadJob jobs[4]; pthread_t th[4];
        for (int t = 0; t < 4; t++) { jobs[t] = one; jobs[t].seed = 77u + (unsigned)t; jobs[t].bad = 0; }
        for (int t = 0; t < 4; t++) pthread_create(&th[t], NULL, random_reads, &jobs[t]);
        int bad = 0;
        for (int t = 0; t < 4; t++) { pthread_join(th[t], NULL); bad += jobs[t].bad; }
        CHECK(bad == 0, "4 threads x 200 random reads, one reader each: %d bad", bad);
    }

    unlink(ref);
    printf("%s\n", failures ? "FAILURES" : "all checks passed");
    return failures ? 1 : 0;
}
