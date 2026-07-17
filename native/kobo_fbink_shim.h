#ifndef RIDDLE_KOBO_FBINK_SHIM_H
#define RIDDLE_KOBO_FBINK_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct riddle_kobo_fb riddle_kobo_fb;

typedef struct {
    uint8_t *buffer;
    size_t buffer_size;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t bpp;
    uint32_t dpi;
    char device_name[32];
    char device_codename[32];
} riddle_kobo_fb_info;

enum {
    RIDDLE_KOBO_REFRESH_FAST = 0,
    RIDDLE_KOBO_REFRESH_BALANCED = 1,
    RIDDLE_KOBO_REFRESH_FULL = 2,
};

riddle_kobo_fb *riddle_kobo_fb_open(riddle_kobo_fb_info *info, char *error, size_t error_len);
int riddle_kobo_fb_refresh(riddle_kobo_fb *fb, uint32_t x, uint32_t y,
                           uint32_t width, uint32_t height, int mode);
void riddle_kobo_fb_close(riddle_kobo_fb *fb);

#ifdef __cplusplus
}
#endif

#endif
