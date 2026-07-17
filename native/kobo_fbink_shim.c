#include "kobo_fbink_shim.h"

#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fbink.h"

enum {
    RIDDLE_GRAYSCALE_8BIT = 1,
    RIDDLE_GRAYSCALE_8BIT_INVERTED = 2,
};

struct riddle_kobo_fb {
    int fd;
    uint8_t original_bpp;
    uint8_t original_grayscale;
    FBInkConfig base;
};

static void set_error(char *error, size_t error_len, const char *message) {
    if (error && error_len) {
        snprintf(error, error_len, "%s", message);
    }
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
    ctx->original_bpp = (uint8_t)state.bpp;
    ctx->original_grayscale = state.inverted_grayscale ? RIDDLE_GRAYSCALE_8BIT_INVERTED : RIDDLE_GRAYSCALE_8BIT;

    // Condor supports switching bit depth. Gray8 is FBInk's fastest path and
    // gives riddle a simple one-byte-per-pixel drawing surface.
    if (state.bpp != 8) {
        int rv = fbink_set_fb_info(ctx->fd, KEEP_CURRENT_ROTATE, 8, RIDDLE_GRAYSCALE_8BIT, &ctx->base);
        if (rv < 0) {
            set_error(error, error_len, "cannot switch framebuffer to Gray8");
            fbink_close(ctx->fd);
            free(ctx);
            return NULL;
        }
        memset(&state, 0, sizeof(state));
        fbink_get_state(&ctx->base, &state);
    }

    size_t buffer_size = 0;
    uint8_t *buffer = fbink_get_fb_pointer(ctx->fd, &buffer_size);
    if (!buffer || !buffer_size || state.bpp != 8) {
        set_error(error, error_len, "FBInk returned no Gray8 framebuffer");
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
            // DU accepts arbitrary previous pixels and is intended for tracing
            // pen input. It is safer than staying in A2 across gray transitions.
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
        // Condor/MTK supports submission waits; a full flash is the one place
        // where fencing avoids the following fast update colliding with it.
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
        if (ctx->original_bpp && ctx->original_bpp != 8) {
            (void)fbink_set_fb_info(ctx->fd, KEEP_CURRENT_ROTATE, ctx->original_bpp,
                                    ctx->original_grayscale, &ctx->base);
        }
        fbink_close(ctx->fd);
    }
    free(ctx);
}
