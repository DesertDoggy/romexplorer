/* rvgm: see rvgm.h. */
#include "rvgm.h"

#include <stdlib.h>
#include <string.h>

#include "libvgmstream.h"
#include "libvgmstream_streamfile.h"

struct rvgm {
    libvgmstream_t *lib;
    /* A copy: every streamfile vgmstream reopens points at this, so it must live as long
     * as the handle, not as long as the caller's struct. */
    rvgm_io io;
    int finished;
};

/* One open file as vgmstream sees it. */
typedef struct {
    libstreamfile_t base; /* first, so a libstreamfile_t* is an rsf* */
    const rvgm_io *io;
    void *user;
    int owns_user;
    int64_t size;
    char *name;
} rsf;

static int rsf_read(void *user_data, uint8_t *dst, int64_t offset, int length) {
    rsf *sf = (rsf *)user_data;
    if (offset < 0 || length <= 0 || offset >= sf->size)
        return 0;
    if (offset + length > sf->size)
        length = (int)(sf->size - offset);
    int32_t got = sf->io->read(sf->user, dst, offset, (int32_t)length);
    return got < 0 ? 0 : got;
}

static int64_t rsf_get_size(void *user_data) {
    return ((rsf *)user_data)->size;
}

static const char *rsf_get_name(void *user_data) {
    return ((rsf *)user_data)->name;
}

static void rsf_close(libstreamfile_t *libsf) {
    rsf *sf = (rsf *)libsf;
    if (!sf)
        return;
    if (sf->owns_user && sf->io->close)
        sf->io->close(sf->user);
    free(sf->name);
    free(sf);
}

static libstreamfile_t *rsf_new(const rvgm_io *io, void *user, int owns_user, int64_t size, const char *name);

static libstreamfile_t *rsf_open(void *user_data, const char *filename) {
    rsf *sf = (rsf *)user_data;
    if (!sf->io->open || !filename)
        return NULL;
    int64_t size = 0;
    void *user = sf->io->open(sf->user, filename, &size);
    if (!user)
        return NULL;
    libstreamfile_t *out = rsf_new(sf->io, user, 1, size, filename);
    if (!out && sf->io->close)
        sf->io->close(user);
    return out;
}

static libstreamfile_t *rsf_new(const rvgm_io *io, void *user, int owns_user, int64_t size, const char *name) {
    rsf *sf = (rsf *)calloc(1, sizeof(rsf));
    if (!sf)
        return NULL;
    sf->name = strdup(name ? name : "stream");
    if (!sf->name) {
        free(sf);
        return NULL;
    }
    sf->io = io;
    sf->user = user;
    sf->owns_user = owns_user;
    sf->size = size;
    sf->base.user_data = sf;
    sf->base.read = rsf_read;
    sf->base.get_size = rsf_get_size;
    sf->base.get_name = rsf_get_name;
    sf->base.open = rsf_open;
    sf->base.close = rsf_close;
    return &sf->base;
}

int32_t rvgm_abi_version(void) {
    return RVGM_ABI_VERSION;
}

rvgm *rvgm_open(const rvgm_io *io, void *user, int64_t size, const char *name, int32_t subsong, double loops, double fade_seconds, int32_t ignore_loop) {
    if (!io || !io->read || size <= 0)
        return NULL;
    rvgm *h = (rvgm *)calloc(1, sizeof(rvgm));
    if (!h)
        return NULL;
    h->io = *io;

    h->lib = libvgmstream_init();
    if (!h->lib) {
        free(h);
        return NULL;
    }
    libvgmstream_set_log(LIBVGMSTREAM_LOG_LEVEL_NONE, NULL);

    libvgmstream_config_t cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.force_sfmt = LIBVGMSTREAM_SFMT_PCM16;
    cfg.ignore_loop = ignore_loop != 0;
    cfg.loop_count = loops > 0 ? loops : 1.0;
    cfg.fade_time = fade_seconds >= 0 ? fade_seconds : 0.0;
    libvgmstream_setup(h->lib, &cfg);

    libstreamfile_t *base = rsf_new(&h->io, user, 0, size, name);
    if (!base) {
        libvgmstream_free(h->lib);
        free(h);
        return NULL;
    }
    /* vgmstream seeks and rereads a lot in small pieces; the buffered wrapper turns that into
     * fewer, larger reads of the caller's source. It owns `base` from here on. */
    libstreamfile_t *buffered = libstreamfile_open_buffered(base);
    libstreamfile_t *sf = buffered ? buffered : base;
    int err = libvgmstream_open_stream(h->lib, sf, subsong < 0 ? 0 : subsong);
    libstreamfile_close(sf);
    if (err < 0 || !h->lib->format) {
        libvgmstream_free(h->lib);
        free(h);
        return NULL;
    }
    return h;
}

static void copy_str(char *dst, size_t cap, const char *src) {
    if (!src) {
        dst[0] = 0;
        return;
    }
    strncpy(dst, src, cap - 1);
    dst[cap - 1] = 0;
}

void rvgm_info_get(const rvgm *h, rvgm_info *out) {
    if (!out)
        return;
    memset(out, 0, sizeof(*out));
    if (!h || !h->lib || !h->lib->format)
        return;
    const libvgmstream_format_t *f = h->lib->format;
    out->channels = f->channels;
    out->sample_rate = f->sample_rate;
    out->subsong_index = f->subsong_index;
    out->subsong_count = f->subsong_count;
    out->stream_samples = f->stream_samples;
    out->loop_start = f->loop_start;
    out->loop_end = f->loop_end;
    out->loop_flag = f->loop_flag ? 1 : 0;
    out->play_samples = f->play_samples;
    out->bitrate = f->stream_bitrate;
    copy_str(out->codec, sizeof(out->codec), f->codec_name);
    copy_str(out->meta, sizeof(out->meta), f->meta_name);
    copy_str(out->stream_name, sizeof(out->stream_name), f->stream_name);
}

int32_t rvgm_render(rvgm *h, const int16_t **out) {
    if (!h || !h->lib || !out)
        return -1;
    *out = NULL;
    if (h->finished)
        return 0;
    /* A render may legitimately produce nothing (a layout switching blocks); a few tries
     * before calling it an end. */
    for (int tries = 0; tries < 16; tries++) {
        int err = libvgmstream_render(h->lib);
        if (err < 0)
            return err;
        libvgmstream_decoder_t *d = h->lib->decoder;
        if (d->done)
            h->finished = 1;
        if (d->buf_samples > 0) {
            *out = (const int16_t *)d->buf;
            return d->buf_samples;
        }
        if (h->finished)
            return 0;
    }
    h->finished = 1;
    return 0;
}

void rvgm_seek(rvgm *h, int64_t sample) {
    if (!h || !h->lib)
        return;
    libvgmstream_seek(h->lib, sample);
    h->finished = 0;
}

void rvgm_close(rvgm *h) {
    if (!h)
        return;
    if (h->lib)
        libvgmstream_free(h->lib);
    free(h);
}

int32_t rvgm_known_extension(const char *name) {
    if (!name)
        return 0;
    libvgmstream_valid_t cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.accept_common = true;
    return libvgmstream_is_valid(name, &cfg) ? 1 : 0;
}
