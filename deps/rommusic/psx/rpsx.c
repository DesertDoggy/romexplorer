/* rpsx_*: PS1 and PS2 music from PSF/PSF2 rips through Highly Experimental
 * (external/highly_experimental, GPL-3.0).
 *
 * The emulator runs the console's sound CPU on a kernel built from a real PS2 BIOS
 * (mkhebios). No BIOS ships with romexplorer: the caller hands one over from the user's own
 * files with rpsx_set_bios, once, before any player is made.
 *
 * PSF (PS1): each file's program area is a PS-X EXE; the Rust side reads the _lib chain and
 * uploads them in psf_load's order. PSF2 (PS2): each file's reserved area is a small file
 * system; the Rust side unpacks it and adds every file here by path, and the emulated
 * kernel reads them (psf2.irx first) through the readfile callback. */
#include <ctype.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "psx.h"
#include "iop.h"
#include "r3000.h"
#include "bios.h"
#include "mkhebios.h"

#if defined(_WIN32)
#define RPSX_API __declspec(dllexport)
#else
#define RPSX_API __attribute__((visibility("default")))
#endif

struct rpsx_file {
    char *path; /* lower case, '/' separated, no leading separator */
    uint8_t *data;
    size_t size;
};

struct rpsx {
    void *state;
    int version;
    int set_pc; /* the first PS-X EXE sets PC and SP */
    struct rpsx_file *files;
    size_t file_count;
};

static void *hebios;

/* Builds the kernel from a 4 MB PS2 BIOS image. 0 on success (or when one is already set),
 * -1 when the image is not one mkhebios can use. */
RPSX_API int rpsx_set_bios(const void *ps2_bios, size_t size) {
    if (hebios) {
        return 0;
    }
    int out_size = (int)size;
    void *built;
    if (size != 0x400000) {
        return -1;
    }
    built = mkhebios_create((void *)ps2_bios, &out_size);
    if (!built) {
        return -1;
    }
    bios_set_image((uint8 *)built, (uint32)out_size);
    if (psx_init() != 0) {
        mkhebios_delete(built);
        return -1;
    }
    hebios = built;
    return 0;
}

RPSX_API int rpsx_bios_ready(void) {
    return hebios != NULL;
}

/* A fresh console: version 1 (PS1) or 2 (PS2). NULL without a BIOS. */
RPSX_API void *rpsx_new(int version) {
    struct rpsx *p;
    if (!hebios || (version != 1 && version != 2)) {
        return NULL;
    }
    p = (struct rpsx *)calloc(1, sizeof *p);
    if (!p) {
        return NULL;
    }
    p->state = malloc(psx_get_state_size((uint8)version));
    if (!p->state) {
        free(p);
        return NULL;
    }
    psx_clear_state(p->state, (uint8)version);
    p->version = version;
    p->set_pc = 1;
    if (version == 1) {
        /* As the PSF players default to: lets games that poke the hardware oddly play. */
        iop_set_compat(psx_get_iop_state(p->state), IOP_COMPAT_FRIENDLY);
    }
    return p;
}

static uint32_t le32(const uint8_t *b) {
    return (uint32_t)b[0] | ((uint32_t)b[1] << 8) | ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24);
}

/* One PS-X EXE (a PSF's program area), deepest _lib first. Returns the refresh rate its
 * header names (50 or 60), 0 when it names none, or -1 when it is not a usable EXE. */
RPSX_API int rpsx_upload_exe(void *handle, const uint8_t *exe, size_t size) {
    struct rpsx *p = (struct rpsx *)handle;
    void *iop;
    uint32_t addr, len;
    int refresh = 0;
    if (p->version != 1 || size < 0x800) {
        return -1;
    }
    addr = le32(exe + 0x18) & 0x1fffff;
    len = (uint32_t)(size - 0x800);
    if (addr < 0x10000 || len > 0x1f0000 || addr + len > 0x200000) {
        return -1;
    }
    iop = psx_get_iop_state(p->state);
    iop_upload_to_ram(iop, addr, exe + 0x800, len);
    if (size > 113 + 13) {
        if (!strncmp((const char *)exe + 113, "Japan", 5) || !strncmp((const char *)exe + 113, "North America", 13)) {
            refresh = 60;
        } else if (!strncmp((const char *)exe + 113, "Europe", 6)) {
            refresh = 50;
        }
    }
    if (p->set_pc) {
        void *cpu = iop_get_r3000_state(iop);
        r3000_setreg(cpu, R3000_REG_PC, le32(exe + 0x10));
        r3000_setreg(cpu, R3000_REG_GEN + 29, le32(exe + 0x30));
        p->set_pc = 0;
    }
    return refresh;
}

static int is_sep(char c) {
    return c == '/' || c == '\\' || c == '|' || c == ':';
}

/* `path` in the form the files are kept in: lower case, '/' between parts. */
static char *normalise(const char *path) {
    size_t n = strlen(path), o = 0, i;
    char *out = (char *)malloc(n + 1);
    if (!out) {
        return NULL;
    }
    for (i = 0; i < n; i++) {
        if (is_sep(path[i])) {
            if (o > 0 && out[o - 1] != '/') {
                out[o++] = '/';
            }
        } else {
            out[o++] = (char)tolower((unsigned char)path[i]);
        }
    }
    while (o > 0 && out[o - 1] == '/') {
        o--;
    }
    out[o] = 0;
    return out;
}

static struct rpsx_file *find(struct rpsx *p, const char *path) {
    size_t i;
    for (i = 0; i < p->file_count; i++) {
        if (!strcmp(p->files[i].path, path)) {
            return &p->files[i];
        }
    }
    return NULL;
}

/* One file of a PSF2's file system. A later file at the same path replaces the earlier
 * one, as libraries are overlaid. 0 on success, -1 out of memory. */
RPSX_API int rpsx_add_file(void *handle, const char *path, const uint8_t *data, size_t size) {
    struct rpsx *p = (struct rpsx *)handle;
    char *key = normalise(path);
    uint8_t *copy;
    struct rpsx_file *f;
    if (!key) {
        return -1;
    }
    copy = (uint8_t *)malloc(size ? size : 1);
    if (!copy) {
        free(key);
        return -1;
    }
    memcpy(copy, data, size);
    f = find(p, key);
    if (f) {
        free(key);
        free(f->data);
    } else {
        struct rpsx_file *grown = (struct rpsx_file *)realloc(p->files, (p->file_count + 1) * sizeof *grown);
        if (!grown) {
            free(key);
            free(copy);
            return -1;
        }
        p->files = grown;
        f = &p->files[p->file_count++];
        f->path = key;
    }
    f->data = copy;
    f->size = size;
    return 0;
}

static sint32 EMU_CALL readfile(void *context, const char *path, sint32 offset, char *buffer, sint32 length) {
    struct rpsx *p = (struct rpsx *)context;
    char *key = normalise(path);
    struct rpsx_file *f;
    if (!key) {
        return -1;
    }
    f = find(p, key);
    free(key);
    if (!f || offset < 0 || length < 0) {
        return -1;
    }
    if (length == 0) {
        return (sint32)f->size; /* the file's length, as psf2fs answers */
    }
    if ((size_t)offset >= f->size) {
        return 0;
    }
    if ((size_t)length > f->size - (size_t)offset) {
        length = (sint32)(f->size - (size_t)offset);
    }
    memcpy(buffer, f->data + offset, (size_t)length);
    return length;
}

/* After the uploads: the refresh rate (50 or 60; anything else keeps the default) and, for
 * PS2, the file system. */
RPSX_API void rpsx_start(void *handle, int refresh) {
    struct rpsx *p = (struct rpsx *)handle;
    if (refresh == 50 || refresh == 60) {
        psx_set_refresh(p->state, (uint32)refresh);
    }
    if (p->version == 2) {
        psx_set_readfile(p->state, readfile, p);
    }
}

/* Fills `frames` stereo frames (44100 Hz for PS1, 48000 for PS2). 0 on success, 1 when the
 * program halted (the rest is silence), -1 on an emulation error. */
RPSX_API int rpsx_render(void *handle, int16_t *out, size_t frames) {
    struct rpsx *p = (struct rpsx *)handle;
    int stalled = 0;
    while (frames > 0) {
        uint32 got = frames > 0x10000 ? 0x10000 : (uint32)frames;
        sint32 r = psx_execute(p->state, 0x7fffffff, out, &got, 0);
        if (r == -1) {
            memset(out, 0, frames * 2 * sizeof *out);
            return 1;
        }
        if (r < -1) {
            return -1;
        }
        /* A program that runs without making sound for this long has hung. */
        if (got == 0 && ++stalled > 1000) {
            return -1;
        }
        out += (size_t)got * 2;
        frames -= got;
    }
    return 0;
}

RPSX_API void rpsx_free(void *handle) {
    struct rpsx *p = (struct rpsx *)handle;
    size_t i;
    if (!p) {
        return;
    }
    for (i = 0; i < p->file_count; i++) {
        free(p->files[i].path);
        free(p->files[i].data);
    }
    free(p->files);
    free(p->state);
    free(p);
}
