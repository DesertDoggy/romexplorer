/* Random-read scaling benchmark for the chdman_reader_* API (streaming build).
 *
 * Usage: chd_reader_bench <streaming lib> <input.chd> [seconds per step]
 *
 * For each thread count, every thread opens its own reader on the same CHD and decompresses
 * uniformly random hunks until the time is up. Reports decompressed MiB/s and speedup over
 * one thread, and the resident memory one open reader costs.
 *
 * The point is not the absolute numbers (they belong to the machine it runs on) but the
 * shape: readers share no locks, caches or file handles, so throughput should keep rising
 * until the machine runs out of cores (or the disk out of bandwidth, on a cold cache), with
 * no internal ceiling. Counts go well past this machine's core count to show that
 * oversubscription is safe, just not faster.
 */
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef struct ChdmanReader ChdmanReader;
typedef struct {
    uint64_t logical_bytes;
    uint32_t hunk_bytes, hunk_count, unit_bytes, version;
    int32_t is_cd, is_gdrom;
    uint32_t num_tracks;
} Info;
static ChdmanReader *(*p_open)(const char *, const char *, uint32_t);
static void (*p_close)(ChdmanReader *);
static int (*p_info)(ChdmanReader *, Info *);
static int (*p_hunk)(ChdmanReader *, uint32_t, void *);

static const char *g_chd;
static double g_secs;
static Info g_info;

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}

static long rss_kb(void)
{
    FILE *f = fopen("/proc/self/statm", "r");
    long pages = 0, res = 0;
    if (f) {
        if (fscanf(f, "%ld %ld", &pages, &res) != 2) res = 0;
        fclose(f);
    }
    return res * (sysconf(_SC_PAGESIZE) / 1024);
}

typedef struct { unsigned seed; uint64_t hunks, errors; } Job;

static void *worker(void *arg)
{
    Job *j = arg;
    ChdmanReader *r = p_open(g_chd, NULL, 0);
    if (!r) { j->errors++; return NULL; }
    uint8_t *buf = malloc(g_info.hunk_bytes);
    double end = now() + g_secs;
    while (now() < end)
        for (int k = 0; k < 16; k++) {
            if (p_hunk(r, (uint32_t)(rand_r(&j->seed) % g_info.hunk_count), buf)) j->errors++;
            else j->hunks++;
        }
    free(buf);
    p_close(r);
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc < 3) { fprintf(stderr, "usage: %s <streaming lib> <input.chd> [seconds]\n", argv[0]); return 2; }
    void *lib = dlopen(argv[1], RTLD_NOW);
    if (!lib) { fprintf(stderr, "%s\n", dlerror()); return 3; }
    *(void **)&p_open = dlsym(lib, "chdman_reader_open");
    if (!p_open) { printf("SKIP: not a streaming build\n"); return 77; }
    *(void **)&p_close = dlsym(lib, "chdman_reader_close");
    *(void **)&p_info = dlsym(lib, "chdman_reader_get_info");
    *(void **)&p_hunk = dlsym(lib, "chdman_reader_read_hunk");
    g_chd = argv[2];
    g_secs = argc > 3 ? atof(argv[3]) : 3.0;

    ChdmanReader *r = p_open(g_chd, NULL, 0);
    if (!r) { fprintf(stderr, "open failed\n"); return 4; }
    p_info(r, &g_info);
    p_close(r);
    printf("%s: %u hunks x %u bytes, %.0f MiB logical, %ld logical CPUs\n", g_chd, g_info.hunk_count,
           g_info.hunk_bytes, g_info.logical_bytes / 1048576.0, sysconf(_SC_NPROCESSORS_ONLN));

    /* memory per open reader: open 32 at once, measure the RSS delta */
    enum { M = 32 };
    ChdmanReader *rs[M];
    uint8_t *tmp = malloc(g_info.hunk_bytes);
    long before = rss_kb();
    for (int i = 0; i < M; i++) { rs[i] = p_open(g_chd, NULL, 0); p_hunk(rs[i], (uint32_t)i, tmp); }
    long after = rss_kb();
    for (int i = 0; i < M; i++) p_close(rs[i]);
    free(tmp);
    printf("memory per open reader (after one read): %.0f KiB\n", (after - before) / (double)M);

    int counts[] = { 1, 2, 4, 6, 8, 12, 16, 20, 24, 32, 48, 64 };
    double base = 0;
    printf("%8s %12s %10s %9s\n", "threads", "MiB/s", "hunks/s", "speedup");
    for (size_t c = 0; c < sizeof counts / sizeof *counts; c++) {
        int n = counts[c];
        Job *jobs = calloc((size_t)n, sizeof *jobs);
        pthread_t *th = calloc((size_t)n, sizeof *th);
        double t0 = now();
        for (int i = 0; i < n; i++) {
            jobs[i].seed = 7u + (unsigned)i * 7919u;
            pthread_create(&th[i], NULL, worker, &jobs[i]);
        }
        uint64_t hunks = 0, errors = 0;
        for (int i = 0; i < n; i++) { pthread_join(th[i], NULL); hunks += jobs[i].hunks; errors += jobs[i].errors; }
        double dt = now() - t0;
        double mib = hunks * (double)g_info.hunk_bytes / 1048576.0 / dt;
        if (n == 1) base = mib;
        printf("%8d %12.1f %10.0f %8.2fx%s\n", n, mib, hunks / dt, mib / base, errors ? "  ERRORS" : "");
        free(jobs);
        free(th);
    }
    return 0;
}
