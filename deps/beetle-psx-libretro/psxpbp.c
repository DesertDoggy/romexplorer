/* psxpbp: see psxpbp.h. Everything that reads a PBP is Beetle PSX's own code; this file opens
 * one CDAccess per handle and switches the reader's global disc selection under one lock. */

#include "psxpbp.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libretro.h>

#include "mednafen/cdrom/CDAccess.h"
#include "mednafen/cdrom/CDAccess_PBP.h"
#include "mednafen/cdrom/CDUtility.h"

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

/* What the core normally provides: the reader's disc selection, and its log sink (silent). */
int CD_SelectedDisc = 0;
extern int PBP_DiscCount;

static void quiet_log(enum retro_log_level level, const char *fmt, ...)
{
    /* PSXPBP_DEBUG=1 shows the reader's own messages, for a PBP that will not open. */
    static int debug = -1;
    if (debug < 0)
        debug = getenv("PSXPBP_DEBUG") != NULL;
    if (!debug)
        return;
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "[psxpbp %d] ", (int)level);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
}
retro_log_printf_t log_cb = quiet_log;

struct psxpbp
{
    CDAccess *cd;
    int discs;
    int selected;
};

static void set_error(char *error, size_t error_len, const char *message)
{
    if (error && error_len)
        snprintf(error, error_len, "%s", message);
}

/* With the lock held: make `disc` the one the reader reads. */
static int select_disc(psxpbp *p, int disc)
{
    if (disc < 0 || disc >= p->discs)
        return 0;
    if (p->selected != disc) {
        CD_SelectedDisc = disc;
        PBP_DiscCount = p->discs;
        p->cd->Eject(p->cd, false);
        p->selected = disc;
        /* Reading the TOC is what loads a disc's block index. */
        TOC toc;
        if (!p->cd->Read_TOC(p->cd, &toc))
            return 0;
    }
    return 1;
}

PSXPBP_API int psxpbp_abi_version(void)
{
    return PSXPBP_ABI_VERSION;
}

PSXPBP_API psxpbp *psxpbp_open(const char *path, char *error, size_t error_len)
{
    if (!path || !*path) {
        set_error(error, error_len, "no input path");
        return NULL;
    }
    LOCK();
    CDUtility_Init();
    CD_SelectedDisc = 0;
    PBP_DiscCount = 0;
    /* The reader only ever clears this. */
    bool ok = true;
    CDAccess *cd = CDAccess_PBP_New(&ok, path, false);
    psxpbp *p = NULL;
    if (!ok || !cd) {
        if (cd)
            cd->destroy(cd);
        set_error(error, error_len, "not a readable PlayStation PBP");
    } else {
        p = (psxpbp *)calloc(1, sizeof(*p));
        TOC toc;
        if (p && !cd->Read_TOC(cd, &toc)) {
            free(p);
            p = NULL;
            cd->destroy(cd);
            set_error(error, error_len, "the PBP's disc table is not readable");
        } else if (p) {
            p->cd = cd;
            p->discs = PBP_DiscCount > 0 ? PBP_DiscCount : 1;
            p->selected = 0;
        } else {
            cd->destroy(cd);
            set_error(error, error_len, "out of memory");
        }
    }
    UNLOCK();
    return p;
}

PSXPBP_API void psxpbp_close(psxpbp *p)
{
    if (!p)
        return;
    LOCK();
    p->cd->destroy(p->cd);
    UNLOCK();
    free(p);
}

PSXPBP_API int psxpbp_disc_count(psxpbp *p)
{
    return p ? p->discs : PSXPBP_ERR_ARGS;
}

PSXPBP_API int psxpbp_tracks(psxpbp *p, int disc, psxpbp_track *out, int max, int32_t *leadout)
{
    if (!p)
        return PSXPBP_ERR_ARGS;
    LOCK();
    int result = PSXPBP_ERR_ARGS;
    TOC toc;
    memset(&toc, 0, sizeof(toc));
    if (select_disc(p, disc)) {
        if (!p->cd->Read_TOC(p->cd, &toc)) {
            result = PSXPBP_ERR_READ;
        } else {
            int n = 0;
            for (int t = toc.first_track; t >= 1 && t <= toc.last_track && t <= 99; t++) {
                int32_t next = t < toc.last_track ? (int32_t)toc.tracks[t + 1].lba : (int32_t)toc.tracks[100].lba;
                if (out && n < max) {
                    out[n].number = t;
                    out[n].audio = (toc.tracks[t].control & 0x4) ? 0 : 1;
                    out[n].lba = (int32_t)toc.tracks[t].lba;
                    out[n].sectors = next - (int32_t)toc.tracks[t].lba;
                }
                n++;
            }
            if (leadout)
                *leadout = (int32_t)toc.tracks[100].lba;
            result = n;
        }
    }
    UNLOCK();
    return result;
}

PSXPBP_API int psxpbp_read(psxpbp *p, int disc, int32_t lba, int32_t count, uint8_t *out)
{
    if (!p || !out || lba < 0 || count < 0)
        return PSXPBP_ERR_ARGS;
    uint8_t sector[PSXPBP_SECTOR + 96];
    LOCK();
    int result = PSXPBP_OK;
    if (!select_disc(p, disc)) {
        result = PSXPBP_ERR_ARGS;
    } else {
        for (int32_t i = 0; i < count; i++) {
            if (!p->cd->Read_Raw_Sector(p->cd, sector, lba + i)) {
                result = PSXPBP_ERR_READ;
                break;
            }
            memcpy(out + (size_t)i * PSXPBP_SECTOR, sector, PSXPBP_SECTOR);
        }
    }
    UNLOCK();
    return result;
}
