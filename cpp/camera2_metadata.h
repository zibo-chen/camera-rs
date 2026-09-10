#pragma once
#include <stdint.h>
#include <stddef.h>
#include <limits.h>

// Pure helpers shared with the host regression test. Android NDK rectangles
// encode left, top, width, height (including SENSOR_INFO_ACTIVE_ARRAY_SIZE).
static inline bool camera2_zoom_crop(const int32_t sensor[4], int32_t zoom100, int32_t out[4]) {
    if (zoom100 < 100 || sensor[0] < 0 || sensor[1] < 0 || sensor[2] <= 0 || sensor[3] <= 0)
        return false;
    int64_t w = (int64_t)sensor[2] * 100 / zoom100;
    int64_t h = (int64_t)sensor[3] * 100 / zoom100;
    int64_t x = sensor[0] + (sensor[2] - w) / 2;
    int64_t y = sensor[1] + (sensor[3] - h) / 2;
    if (!w || !h || x > INT32_MAX || y > INT32_MAX) return false;
    out[0] = (int32_t)x; out[1] = (int32_t)y;
    out[2] = (int32_t)w; out[3] = (int32_t)h;
    return true;
}
static inline int32_t camera2_clamp_fps(int32_t fps, const int32_t range[2]) {
    return fps < range[0] ? range[0] : (fps > range[1] ? range[1] : fps);
}
static inline bool camera2_select_fps(const int32_t *ranges, size_t count, int32_t requested, int32_t out[2]) {
    if (!ranges || requested <= 0) return false;
    int64_t best_distance = INT64_MAX, best_width = INT64_MAX;
    bool found = false;
    for (size_t i = 0; i + 1 < count; i += 2) {
        if (ranges[i] <= 0 || ranges[i+1] < ranges[i]) continue;
        int64_t delta = (int64_t)camera2_clamp_fps(requested, ranges+i) - requested;
        if (delta < 0) delta = -delta;
        int64_t width = (int64_t)ranges[i+1] - ranges[i];
        if (delta < best_distance || (delta == best_distance && width < best_width)) {
            best_distance = delta; best_width = width; found = true;
            out[0] = ranges[i]; out[1] = ranges[i+1];
        }
    }
    return found;
}
