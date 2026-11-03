/* exiso: see exiso.h. extract-xiso.c is compiled here as it is, with its main() renamed, so
 * its file-static functions and globals are reachable from the wrapper below. */

/* Before any system header: extract-xiso.c sets these itself, but exiso.h comes first. */
#if defined(__LINUX__)
#define _LARGEFILE64_SOURCE
#define _GNU_SOURCE
#endif

#include "exiso.h"

/* Every write extract-xiso makes goes through exiso_write (below), which reports progress and
 * is where a cancel stops it: a failed write is an error extract-xiso already handles, by
 * unwinding and leaving the output to be removed. The system headers come first, so their own
 * declaration of write() is not renamed. */
#include <errno.h>
#if defined(_WIN32)
#include <io.h>
#else
#include <unistd.h>
#endif
static int exiso_write(int fd, const void *buf, unsigned int len);
#define write(fd, buf, len) exiso_write((fd), (buf), (unsigned int)(len))
/* Paths come in as UTF-8. Windows' open() reads them in the ANSI code page, which fails on any
 * non-ASCII name (a Japanese folder), so there every open goes through _wopen. */
#if defined(_WIN32)
#include <fcntl.h>
static int exiso_open(const char *path, int flags, int mode);
#define open(path, flags, mode) exiso_open((path), (flags), (mode))
#endif

#define main exiso_cli_main
#include "extract-xiso.c"
#undef main
#undef write
#if defined(_WIN32)
#undef open
#endif

#if defined(_WIN32)
#include <windows.h>
static SRWLOCK g_lock = SRWLOCK_INIT;
#define LOCK() AcquireSRWLockExclusive(&g_lock)
#define UNLOCK() ReleaseSRWLockExclusive(&g_lock)
#else
#include <pthread.h>
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
#define LOCK() pthread_mutex_lock(&g_lock)
#define UNLOCK() pthread_mutex_unlock(&g_lock)
#endif

static exiso_progress_fn g_progress;
static void *g_user;
/* Bytes written so far and the file data's total, which extract-xiso reports up front. */
static uint64_t g_done, g_total;
static int g_cancelled;

static void on_progress(xoff_t done, xoff_t total)
{
    (void)done;
    g_total = (uint64_t)total;
}

static int exiso_write(int fd, const void *buf, unsigned int len)
{
    if (g_cancelled) {
        errno = ECANCELED;
        return -1;
    }
    int n = (int)write(fd, buf, len);
    if (n > 0)
        g_done += (uint64_t)n;
    if (g_progress && g_total) {
        uint64_t done = g_done < g_total ? g_done : g_total;
        if (!g_progress(done, g_total, g_user)) {
            g_cancelled = 1;
            errno = ECANCELED;
            return -1;
        }
    }
    return n;
}

#if defined(_WIN32)
static int exiso_open(const char *path, int flags, int mode)
{
    int n = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, NULL, 0);
    if (n <= 0)
        return open(path, flags, mode);
    wchar_t *wide = (wchar_t *)malloc((size_t)n * sizeof(wchar_t));
    if (!wide) {
        errno = ENOMEM;
        return -1;
    }
    MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, n);
    int fd = _wopen(wide, flags, mode);
    free(wide);
    return fd;
}
#endif

static void set_error(char *error, size_t error_len, const char *message)
{
    if (!error || !error_len)
        return;
    snprintf(error, error_len, "%s", message);
}

EXISO_API int exiso_abi_version(void)
{
    return EXISO_ABI_VERSION;
}

EXISO_API int exiso_rewrite(const char *in_path, const char *out_path,
                            exiso_progress_fn progress, void *user,
                            char *error, size_t error_len)
{
    if (!in_path || !*in_path || !out_path || !*out_path) {
        set_error(error, error_len, "no input or output path");
        return EXISO_ERR_ARGS;
    }

    /* create_xiso wants the output as a directory and a file name, both writable. */
    char *dir = strdup(out_path);
    if (!dir) {
        set_error(error, error_len, "out of memory");
        return EXISO_ERR_FAILED;
    }
    char *slash = strrchr(dir, '/');
#if defined(_WIN32)
    char *back = strrchr(dir, '\\');
    if (!slash || (back && back > slash))
        slash = back;
#endif
    char *file;
    char here[] = ".";
    /* create_xiso strips a trailing separator in place, so the root is given as "/." (writable,
     * and still the root once joined). */
    char root_dir[] = PATH_CHAR_STR ".";
    char *out_dir;
    if (slash) {
        *slash = 0;
        file = slash + 1;
        out_dir = *dir ? dir : root_dir;
    } else {
        file = dir;
        out_dir = here;
    }
    if (!*file) {
        free(dir);
        set_error(error, error_len, "the output path names a directory");
        return EXISO_ERR_ARGS;
    }
    char *in_name = strdup(in_path);
    char path_prefix[] = PATH_CHAR_STR;

    LOCK();
    /* What extract-xiso's main() would set for `-r -m -Q`: rewrite, no media patching, silent. */
    s_quiet = s_real_quiet = true;
    s_media_enable = false;
    s_remove_systemupdate = false;
    s_total_bytes = 0;
    s_total_files = 0;
    s_xbox_disc_lseek = 0;
    g_progress = progress;
    g_user = user;
    g_done = g_total = 0;
    g_cancelled = 0;

    int status = EXISO_OK;
    int err = boyer_moore_init(XISO_MEDIA_ENABLE, XISO_MEDIA_ENABLE_LENGTH, k_default_alphabet_size);
    int xiso = err ? -1 : open(in_path, READFLAGS, 0);
    int32_t root_sector = 0, root_size = 0;
    dir_node_avl *root = nil;
    /* Given back by create_xiso rather than freed there: on an error it unlinks the output by
     * this path after the point it would have freed it. */
    char *written = nil;
    if (err || !in_name) {
        status = EXISO_ERR_FAILED;
        set_error(error, error_len, "out of memory");
    } else if (xiso == -1) {
        status = EXISO_ERR_OPEN;
        set_error(error, error_len, "the image could not be opened");
    } else if ((err = verify_xiso(xiso, &root_sector, &root_size, in_name)) != 0) {
        status = err == err_iso_no_files ? EXISO_ERR_EMPTY : EXISO_ERR_OPEN;
        set_error(error, error_len, err == err_iso_no_files ? "the image holds no files" : "not an Xbox or Xbox 360 disc image");
    } else {
        xoff_t start = (xoff_t)root_sector * XISO_SECTOR_SIZE + s_xbox_disc_lseek;
        if (lseek(xiso, start, SEEK_SET) == -1)
            err = 1;
        if (!err)
            err = traverse_xiso(xiso, nil, start, path_prefix, k_generate_avl, &root, true);
        if (!err)
            /* A rewrite names its output after the root it is given; in_name only stops ".iso" being
             * appended. */
            err = create_xiso(file, out_dir, root, xiso, &written, file, on_progress);
        if (err == err_iso_rewritten)
            err = 0;
        if (err && g_cancelled) {
            status = EXISO_CANCELLED;
            set_error(error, error_len, "cancelled");
            remove(out_path);
        } else if (err) {
            status = EXISO_ERR_FAILED;
            set_error(error, error_len, "reading the image or writing the XISO failed part way");
            remove(out_path);
        }
    }
    if (xiso != -1)
        close(xiso);
    boyer_moore_done();
    g_progress = NULL;
    g_user = NULL;
    UNLOCK();

    free(written);
    free(in_name);
    free(dir);
    return status;
}
