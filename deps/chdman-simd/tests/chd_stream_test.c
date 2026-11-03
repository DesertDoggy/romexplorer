/* Functional test for the chdman streaming build (build_chdman_lib.sh's default).
 *
 * Usage: chd_stream_test <streaming lib> <default lib> <input.chd> <cd|dvd|raw> <scratch dir>
 *                        [official chdman executable]
 *
 * With the optional official chdman (e.g. /usr/bin/chdman from mame-tools), the reference
 * output is additionally checked against that tool's own extraction, file by file.
 *
 * The reference is a plain extract by the DEFAULT (unpatched) library, so every check below
 * is against what the unpatched chdman writes, not against the streaming build itself.
 *
 *   1. stream + write: every output file's streamed bytes hash-match the reference file, the
 *      file written in that same pass hash-matches too, and offsets per file are contiguous
 *      from 0. For extractcd the cue sheet comes last as its own file.
 *   2. stream only (write_files=0): same hashes, and nothing is left in the output dir.
 *   3. stop: returning 0 from on_data yields -6 and leaves no files.
 *   4. reader: random-offset read_bytes (dvd/raw) or random physical sector reads (cd, data
 *      tracks) compared against the reference output, single-threaded with and without the
 *      LRU cache, then from 4 threads at once, each with its own reader on the same CHD.
 *
 * dlopen'd so it can run against either build: SKIP (77) when the streaming symbols are
 * missing.
 */

#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

typedef void (*ChdmanProgressCb)(const char *, float, void *);
typedef int (*ChdmanDataCb)(uint32_t, const char *, uint64_t, const void *, uint32_t, void *);
typedef struct ChdmanReader ChdmanReader;
typedef struct {
    uint64_t logical_bytes;
    uint32_t hunk_bytes, hunk_count, unit_bytes, version;
    int32_t is_cd, is_gdrom;
    uint32_t num_tracks;
} ChdmanReaderInfo;
typedef struct {
    uint32_t track_type, sub_type, data_size, sub_size, frames, pregap, postgap, session;
    uint32_t logical_start, physical_start, chd_frame_start;
} ChdmanTrackInfo;

static int (*p_run)(int, const char *const *, char **, ChdmanProgressCb, void *);
static int (*p_run_default)(int, const char *const *, char **, ChdmanProgressCb, void *);
static void (*p_free_log)(char *);
static int (*p_stream)(int, const char *const *, int, ChdmanDataCb, char **, ChdmanProgressCb, void *);
static ChdmanReader *(*p_open)(const char *, const char *, uint32_t);
static void (*p_close)(ChdmanReader *);
static int (*p_info)(ChdmanReader *, ChdmanReaderInfo *);
static int (*p_read_bytes)(ChdmanReader *, uint64_t, void *, uint32_t);
static int (*p_track)(ChdmanReader *, uint32_t, ChdmanTrackInfo *);
static int (*p_sector)(ChdmanReader *, uint32_t, void *, uint32_t, int);
static const char *(*p_err)(void);

static int failures;
#define CHECK(cond, ...) do { \
    if (!(cond)) { printf("FAIL: "); printf(__VA_ARGS__); printf("\n"); failures++; } \
    else { printf("ok: "); printf(__VA_ARGS__); printf("\n"); } \
} while (0)

/* ---- FNV-1a 64, streamable ---- */
static uint64_t fnv(uint64_t h, const void *data, size_t n)
{
    const uint8_t *p = (const uint8_t *)data;
    for (size_t i = 0; i < n; i++) { h ^= p[i]; h *= 0x100000001b3ULL; }
    return h;
}
#define FNV_INIT 0xcbf29ce484222325ULL

static int hash_file(const char *path, uint64_t *out, uint64_t *size)
{
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    static uint8_t buf[1 << 20];
    uint64_t h = FNV_INIT, total = 0;
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) { h = fnv(h, buf, n); total += n; }
    fclose(f);
    *out = h; *size = total;
    return 0;
}

/* ---- stream sink: per-file running hash + contiguity ---- */
#define MAX_FILES 128
typedef struct {
    char name[MAX_FILES][1024];
    uint64_t hash[MAX_FILES], next[MAX_FILES];
    int nfiles, gaps, bad_index, stop_now;
} Sink;

static int on_data(uint32_t idx, const char *name, uint64_t off, const void *data, uint32_t size, void *ud)
{
    Sink *s = (Sink *)ud;
    if (s->stop_now) return 0;
    if (idx == (uint32_t)s->nfiles && idx < MAX_FILES) {       /* first chunk of a new file */
        snprintf(s->name[idx], sizeof s->name[idx], "%s", name);
        s->hash[idx] = FNV_INIT; s->next[idx] = 0; s->nfiles++;
    }
    if (idx >= (uint32_t)s->nfiles || strcmp(s->name[idx], name) != 0) { s->bad_index++; return 1; }
    if (off != s->next[idx]) s->gaps++;
    s->next[idx] = off + size;
    s->hash[idx] = fnv(s->hash[idx], data, size);
    return 1;
}

static const char *base_name(const char *p) { const char *s = strrchr(p, '/'); return s ? s + 1 : p; }

static int dir_file_count(const char *dir)
{
    DIR *d = opendir(dir);
    if (!d) return -1;
    int n = 0; struct dirent *e;
    while ((e = readdir(d))) if (e->d_name[0] != '.') n++;
    closedir(d);
    return n;
}

static void rm_dir(const char *dir)
{
    DIR *d = opendir(dir);
    if (!d) return;
    struct dirent *e; char p[2048];
    while ((e = readdir(d))) if (e->d_name[0] != '.') { snprintf(p, sizeof p, "%s/%s", dir, e->d_name); unlink(p); }
    closedir(d);
    rmdir(dir);
}

static const char *g_cmd, *g_ext, *g_chd;

static int run_extract(int (*fn)(int, const char *const *, char **, ChdmanProgressCb, void *), const char *out)
{
    const char *argv[] = { g_cmd, "-i", g_chd, "-o", out, "-f" };
    char *log = NULL;
    int rc = fn(6, argv, &log, NULL, NULL);
    if (rc) printf("extract log:\n%s\n", log ? log : "");
    p_free_log(log);
    return rc;
}

static int run_stream(const char *out, int write_files, Sink *s)
{
    const char *argv[] = { g_cmd, "-i", g_chd, "-o", out, "-f" };
    char *log = NULL;
    int rc = p_stream(6, argv, write_files, on_data, &log, NULL, s);
    if (rc && rc != -6) printf("stream log:\n%s\n", log ? log : "");
    p_free_log(log);
    return rc;
}

/* ---- reader: random reads vs reference ---- */
typedef struct {
    int is_cd;
    char ref_path[2048];           /* dvd/raw: the .iso/.raw; cd: track-1 bin (cue mode) */
    ChdmanTrackInfo t0;            /* cd only */
    uint64_t ref_size;
    unsigned seed;
    int iterations, mismatches, errors;
    uint32_t cache;
} ReadJob;

static void *random_reads(void *arg)
{
    ReadJob *j = (ReadJob *)arg;
    ChdmanReader *r = p_open(g_chd, NULL, j->cache);
    if (!r) { j->errors++; return NULL; }
    FILE *ref = fopen(j->ref_path, "rb");
    if (!ref) { j->errors++; p_close(r); return NULL; }
    static __thread uint8_t a[1 << 18], b[1 << 18];
    for (int i = 0; i < j->iterations; i++) {
        uint64_t off; uint32_t len;
        if (j->is_cd) {
            /* bound by what the bin actually holds: a GD-ROM track's frames include
             * padding frames that chdman does not write out */
            uint32_t in_bin = (uint32_t)(j->ref_size / j->t0.data_size);
            uint32_t sector = (uint32_t)(rand_r(&j->seed) % (in_bin < j->t0.frames ? in_bin : j->t0.frames));
            len = j->t0.data_size;
            off = (uint64_t)sector * len;
            if (p_sector(r, j->t0.physical_start + sector, a, 8 /* RAW_DONTCARE */, 1)) { j->errors++; continue; }
        } else {
            len = 1 + (uint32_t)(rand_r(&j->seed) % (sizeof a - 1));
            off = ((uint64_t)rand_r(&j->seed) << 20 | (uint64_t)rand_r(&j->seed)) % j->ref_size;
            if (off + len > j->ref_size) len = (uint32_t)(j->ref_size - off);
            if (p_read_bytes(r, off, a, len)) { j->errors++; continue; }
        }
        if (fseeko(ref, (off_t)off, SEEK_SET) || fread(b, 1, len, ref) != len) { j->errors++; continue; }
        if (memcmp(a, b, len)) j->mismatches++;
    }
    fclose(ref);
    p_close(r);
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc < 6) {
        fprintf(stderr, "usage: %s <streaming lib> <default lib> <input.chd> <cd|dvd|raw> <scratch dir>\n", argv[0]);
        return 2;
    }
    void *lib = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    void *def = dlopen(argv[2], RTLD_NOW | RTLD_LOCAL);
    if (!lib || !def) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 3; }
    *(void **)&p_stream = dlsym(lib, "chdman_extract_stream");
    *(void **)&p_open = dlsym(lib, "chdman_reader_open");
    if (!p_stream || !p_open) { printf("SKIP: %s is not a streaming build\n", argv[1]); return 77; }
    *(void **)&p_run = dlsym(lib, "chdman_run");
    *(void **)&p_run_default = dlsym(def, "chdman_run");
    *(void **)&p_free_log = dlsym(lib, "chdman_free_log");
    *(void **)&p_close = dlsym(lib, "chdman_reader_close");
    *(void **)&p_info = dlsym(lib, "chdman_reader_get_info");
    *(void **)&p_read_bytes = dlsym(lib, "chdman_reader_read_bytes");
    *(void **)&p_track = dlsym(lib, "chdman_reader_get_track");
    *(void **)&p_sector = dlsym(lib, "chdman_reader_read_sector");
    *(void **)&p_err = dlsym(lib, "chdman_reader_last_error");

    g_chd = argv[3];
    const char *mode = argv[4], *scratch = argv[5];
    if (!strcmp(mode, "cd")) { g_cmd = "extractcd"; g_ext = "cue"; }
    else if (!strcmp(mode, "dvd")) { g_cmd = "extractdvd"; g_ext = "iso"; }
    else { g_cmd = "extractraw"; g_ext = "raw"; }

    char ref_dir[1024], s_dir[1024], n_dir[1024], x_dir[1024], out[2048];
    snprintf(ref_dir, sizeof ref_dir, "%s/ref", scratch);
    snprintf(s_dir, sizeof s_dir, "%s/stream", scratch);
    snprintf(n_dir, sizeof n_dir, "%s/nowrite", scratch);
    snprintf(x_dir, sizeof x_dir, "%s/stop", scratch);
    mkdir(scratch, 0755);
    rm_dir(ref_dir); rm_dir(s_dir); rm_dir(n_dir); rm_dir(x_dir);
    mkdir(ref_dir, 0755); mkdir(s_dir, 0755); mkdir(n_dir, 0755); mkdir(x_dir, 0755);

    /* reference: DEFAULT lib, plain extract */
    snprintf(out, sizeof out, "%s/disc.%s", ref_dir, g_ext);
    CHECK(run_extract(p_run_default, out) == 0, "reference extract with the default build");

    /* optional: the official chdman must produce the same files */
    char off_dir[1024];
    snprintf(off_dir, sizeof off_dir, "%s/official", scratch);
    if (argc > 6) {
        rm_dir(off_dir);
        mkdir(off_dir, 0755);
        char cmd[8192];
        snprintf(cmd, sizeof cmd, "'%s' %s -i '%s' -o '%s/disc.%s' -f > /dev/null 2>&1", argv[6], g_cmd, g_chd, off_dir, g_ext);
        CHECK(system(cmd) == 0, "official chdman extract (%s)", argv[6]);
        int nref = dir_file_count(ref_dir), noff = dir_file_count(off_dir), all_same = nref == noff;
        DIR *d = opendir(ref_dir); struct dirent *e;
        while (d && (e = readdir(d))) {
            if (e->d_name[0] == '.') continue;
            char a[2048], b[2048]; uint64_t ha = 0, hb = 1, sa, sb;
            snprintf(a, sizeof a, "%s/%s", ref_dir, e->d_name);
            snprintf(b, sizeof b, "%s/%s", off_dir, e->d_name);
            if (hash_file(a, &ha, &sa) || hash_file(b, &hb, &sb) || ha != hb) { all_same = 0; printf("  differs: %s\n", e->d_name); }
        }
        if (d) closedir(d);
        CHECK(all_same, "default build's %d files identical to official chdman's %d", nref, noff);
        rm_dir(off_dir);
    }

    /* 1. stream + write */
    Sink *s = calloc(1, sizeof *s);
    snprintf(out, sizeof out, "%s/disc.%s", s_dir, g_ext);
    CHECK(run_stream(out, 1, s) == 0, "stream+write returned 0");
    CHECK(s->gaps == 0 && s->bad_index == 0, "%d files, offsets contiguous, indices consistent", s->nfiles);
    CHECK(s->nfiles == dir_file_count(ref_dir), "streamed %d files == reference's %d", s->nfiles, dir_file_count(ref_dir));
    if (!strcmp(mode, "cd"))
        CHECK(s->nfiles > 0 && !strcmp(base_name(s->name[s->nfiles - 1]), "disc.cue"), "cue sheet delivered last");
    for (int i = 0; i < s->nfiles; i++) {
        char ref_path[2048]; uint64_t rh = 0, wh = 0, rs = 0, ws = 0;
        snprintf(ref_path, sizeof ref_path, "%s/%s", ref_dir, base_name(s->name[i]));
        int ok_r = hash_file(ref_path, &rh, &rs) == 0, ok_w = hash_file(s->name[i], &wh, &ws) == 0;
        /* the cue sheet names its bins by basename only, so it matches across dirs */
        CHECK(ok_r && s->hash[i] == rh && s->next[i] == rs, "file %d %s: streamed == reference (%llu bytes)", i,
              base_name(s->name[i]), (unsigned long long)rs);
        CHECK(ok_w && wh == rh, "file %d %s: written == reference", i, base_name(s->name[i]));
    }

    /* 2. stream only */
    Sink *n = calloc(1, sizeof *n);
    snprintf(out, sizeof out, "%s/disc.%s", n_dir, g_ext);
    CHECK(run_stream(out, 0, n) == 0, "stream-only returned 0");
    CHECK(n->nfiles == s->nfiles, "stream-only delivered %d files", n->nfiles);
    int same = n->nfiles == s->nfiles;
    for (int i = 0; same && i < n->nfiles; i++) same = n->hash[i] == s->hash[i] && n->next[i] == s->next[i];
    CHECK(same, "stream-only bytes identical to stream+write");
    CHECK(dir_file_count(n_dir) == 0, "stream-only left nothing on disk (%d files)", dir_file_count(n_dir));

    /* 3. stop */
    Sink *x = calloc(1, sizeof *x);
    x->stop_now = 1;
    snprintf(out, sizeof out, "%s/disc.%s", x_dir, g_ext);
    CHECK(run_stream(out, 1, x) == -6, "returning 0 from on_data stops with -6");
    CHECK(dir_file_count(x_dir) == 0, "stopped extraction left nothing on disk (%d files)", dir_file_count(x_dir));

    /* 4. reader */
    ChdmanReader *r = p_open(g_chd, NULL, 8);
    CHECK(r != NULL, "reader opens (%s)", r ? "" : p_err());
    if (r) {
        ChdmanReaderInfo info;
        p_info(r, &info);
        printf("info: v%u logical=%llu hunk=%u x %u unit=%u cd=%d gdrom=%d tracks=%u\n", info.version,
               (unsigned long long)info.logical_bytes, info.hunk_bytes, info.hunk_count, info.unit_bytes,
               info.is_cd, info.is_gdrom, info.num_tracks);
        CHECK(info.is_cd == !strcmp(mode, "cd"), "is_cd matches the mode");

        ReadJob base;
        memset(&base, 0, sizeof base);
        base.is_cd = info.is_cd;
        base.iterations = 400;
        int can_read = 1;
        if (info.is_cd) {
            p_track(r, 0, &base.t0);
            /* cue output byte-swaps audio, so only compare a data track 1 */
            can_read = base.t0.track_type != 7;
            snprintf(base.ref_path, sizeof base.ref_path, "%s/%s", ref_dir, base_name(s->name[0]));
        } else {
            snprintf(base.ref_path, sizeof base.ref_path, "%s/disc.%s", ref_dir, g_ext);
        }
        uint64_t dummy;
        hash_file(base.ref_path, &dummy, &base.ref_size);
        p_close(r);

        if (can_read) {
            for (int cache = 0; cache <= 8; cache += 8) {
                ReadJob j = base; j.cache = (uint32_t)cache; j.seed = 1234u + (unsigned)cache;
                random_reads(&j);
                CHECK(j.errors == 0 && j.mismatches == 0, "%d random reads, cache=%d: %d mismatches, %d errors",
                      j.iterations, cache, j.mismatches, j.errors);
            }
            ReadJob jobs[4]; pthread_t th[4];
            for (int t = 0; t < 4; t++) { jobs[t] = base; jobs[t].cache = 4; jobs[t].seed = 99u + (unsigned)t; }
            for (int t = 0; t < 4; t++) pthread_create(&th[t], NULL, random_reads, &jobs[t]);
            int bad = 0;
            for (int t = 0; t < 4; t++) { pthread_join(th[t], NULL); bad += jobs[t].errors + jobs[t].mismatches; }
            CHECK(bad == 0, "4 threads x %d random reads, one reader each: %d bad", base.iterations, bad);
        } else {
            printf("note: track 1 is audio, skipping sector compare\n");
        }
    }

    rm_dir(ref_dir); rm_dir(s_dir); rm_dir(n_dir); rm_dir(x_dir);
    free(s); free(n); free(x);
    printf("%s\n", failures ? "FAILURES" : "all checks passed");
    return failures ? 1 : 0;
}
