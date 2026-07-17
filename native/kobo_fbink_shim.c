#include "kobo_fbink_shim.h"

#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fbink.h"

enum {
    RIDDLE_GRAYSCALE_8BIT = 1,
};

struct riddle_kobo_fb {
    int fd;
    riddle_kobo_fb_state original;
    bool mode_changed;
    FBInkConfig base;
};

static void set_error(char *error, size_t error_len, const char *message) {
    if (error && error_len) {
        snprintf(error, error_len, "%s", message);
    }
}

static void read_state(riddle_kobo_fb_state *out) {
    struct fb_var_screeninfo var_info;
    struct fb_fix_screeninfo fix_info;
    memset(&var_info, 0, sizeof(var_info));
    memset(&fix_info, 0, sizeof(fix_info));
    fbink_get_fb_info(&var_info, &fix_info);
    out->bpp = (uint8_t)var_info.bits_per_pixel;
    out->grayscale = (uint8_t)var_info.grayscale;
    out->reserved = 0;
    out->rotation = var_info.rotate;
}

int riddle_kobo_fb_capture_state(riddle_kobo_fb_state *state, char *error, size_t error_len) {
    if (!state) {
        set_error(error, error_len, "missing framebuffer state output");
        return -EINVAL;
    }
    FBInkConfig cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.is_quiet = true;
    int fd = fbink_open();
    if (fd < 0 || fbink_init(fd, &cfg) < 0) {
        if (fd >= 0) {
            fbink_close(fd);
        }
        set_error(error, error_len, "cannot capture framebuffer state");
        return -EIO;
    }
    read_state(state);
    fbink_close(fd);
    return 0;
}

int riddle_kobo_fb_restore_state(const riddle_kobo_fb_state *state, char *error, size_t error_len) {
    if (!state || !state->bpp) {
        set_error(error, error_len, "invalid framebuffer restore state");
        return -EINVAL;
    }
    FBInkConfig cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.is_quiet = true;
    int fd = fbink_open();
    if (fd < 0 || fbink_init(fd, &cfg) < 0) {
        if (fd >= 0) {
            fbink_close(fd);
        }
        set_error(error, error_len, "cannot open framebuffer for restore");
        return -EIO;
    }
    int rv = fbink_set_fb_info(fd, state->rotation, state->bpp, state->grayscale, &cfg);
    fbink_close(fd);
    if (rv < 0) {
        set_error(error, error_len, "framebuffer restore failed");
    }
    return rv;
}

riddle_kobo_fb *riddle_kobo_fb_open(riddle_kobo_fb_info *info, char *error, size_t error_len) {
    if (!info) {
        set_error(error, error_len, "missing output info");
        return NULL;
    }
    memset(info, 0, sizeof(*info));

    riddle_kobo_fb *ctx = calloc(1, sizeof(*ctx));
    if (!ctx) {
        set_error(error, error_len, "out of memory");
        return NULL;
    }
    ctx->fd = fbink_open();
    if (ctx->fd < 0) {
        set_error(error, error_len, "fbink_open failed");
        free(ctx);
        return NULL;
    }

    memset(&ctx->base, 0, sizeof(ctx->base));
    ctx->base.is_quiet = true;
    ctx->base.no_viewport = true;
    ctx->base.dithering_mode = HWD_PASSTHROUGH;
    if (fbink_init(ctx->fd, &ctx->base) < 0) {
        set_error(error, error_len, "fbink_init failed");
        fbink_close(ctx->fd);
        free(ctx);
        return NULL;
    }

    FBInkState state;
    memset(&state, 0, sizeof(state));
    fbink_get_state(&ctx->base, &state);
    read_state(&ctx->original);
    const uint32_t canonical_ur = fbink_rota_canonical_to_native(0);

    // Keep Condor's native bit depth (normally RGB32) and only normalize the
    // device rotation. If firmware happens to expose inverted Gray8, normalize
    // its grayscale semantics as well. This avoids relying on an unsupported
    // MTK 32→8 bpp switch.
    const bool normalize_gray8 = state.bpp == 8 && state.inverted_grayscale;
    if (normalize_gray8 || state.current_rota != canonical_ur) {
        const uint8_t grayscale = normalize_gray8 ? RIDDLE_GRAYSCALE_8BIT : KEEP_CURRENT_GRAYSCALE;
        int rv = fbink_set_fb_info(ctx->fd, canonical_ur, KEEP_CURRENT_BITDEPTH, grayscale, &ctx->base);
        if (rv < 0) {
            set_error(error, error_len, "cannot switch framebuffer to canonical portrait");
            fbink_close(ctx->fd);
            free(ctx);
            return NULL;
        }
        ctx->mode_changed = true;
        memset(&state, 0, sizeof(state));
        fbink_get_state(&ctx->base, &state);
    }

    size_t buffer_size = 0;
    uint8_t *buffer = fbink_get_fb_pointer(ctx->fd, &buffer_size);
    const bool supported_bpp = state.bpp == 8 || state.bpp == 32;
    const bool bad_gray8 = state.bpp == 8 && state.inverted_grayscale;
    if (!buffer || !buffer_size || !supported_bpp || bad_gray8 || state.current_rota != canonical_ur) {
        char message[160];
        snprintf(message, sizeof(message),
                 "unsupported framebuffer after normalize (bpp=%u inverted=%u rota=%u expected=%u)",
                 state.bpp, state.inverted_grayscale ? 1U : 0U, state.current_rota, canonical_ur);
        set_error(error, error_len, message);
        riddle_kobo_fb_close(ctx);
        return NULL;
    }

    info->buffer = buffer;
    info->buffer_size = buffer_size;
    info->width = state.screen_width;
    info->height = state.screen_height;
    info->stride = state.scanline_stride;
    info->bpp = state.bpp;
    info->dpi = state.screen_dpi;
    snprintf(info->device_name, sizeof(info->device_name), "%s", state.device_name);
    snprintf(info->device_codename, sizeof(info->device_codename), "%s", state.device_codename);

    if (strcmp(info->device_codename, "Condor") != 0 || info->width != 1404 || info->height != 1872) {
        set_error(error, error_len, "unsupported Kobo (expected Elipsa 2E / Condor 1404x1872)");
        riddle_kobo_fb_close(ctx);
        return NULL;
    }
    if ((size_t)info->stride * info->height > info->buffer_size) {
        set_error(error, error_len, "framebuffer mapping is smaller than its geometry");
        riddle_kobo_fb_close(ctx);
        return NULL;
    }
    return ctx;
}

int riddle_kobo_fb_refresh(riddle_kobo_fb *ctx, uint32_t x, uint32_t y,
                           uint32_t width, uint32_t height, int mode) {
    if (!ctx) {
        return -EINVAL;
    }
    FBInkConfig cfg = ctx->base;
    cfg.is_flashing = false;
    switch (mode) {
        case RIDDLE_KOBO_REFRESH_FAST:
            cfg.wfm_mode = WFM_DU;
            break;
        case RIDDLE_KOBO_REFRESH_BALANCED:
            cfg.wfm_mode = WFM_GL16;
            break;
        case RIDDLE_KOBO_REFRESH_FULL:
            cfg.wfm_mode = WFM_GC16;
            cfg.is_flashing = true;
            x = y = width = height = 0;
            break;
        default:
            return -EINVAL;
    }
    int rv = fbink_refresh(ctx->fd, y, x, width, height, &cfg);
    if (rv == 0 && mode == RIDDLE_KOBO_REFRESH_FULL) {
        (void)fbink_wait_for_submission(ctx->fd, LAST_MARKER);
        (void)fbink_wait_for_complete(ctx->fd, LAST_MARKER);
    }
    return rv;
}

void riddle_kobo_fb_close(riddle_kobo_fb *ctx) {
    if (!ctx) {
        return;
    }
    if (ctx->fd >= 0) {
        if (ctx->mode_changed && ctx->original.bpp) {
            (void)fbink_set_fb_info(ctx->fd, ctx->original.rotation, ctx->original.bpp,
                                    ctx->original.grayscale, &ctx->base);
        }
        fbink_close(ctx->fd);
    }
    free(ctx);
}
