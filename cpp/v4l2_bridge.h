#ifndef CAMERA_V4L2_BRIDGE_H
#define CAMERA_V4L2_BRIDGE_H
#include <stdint.h>
#include <stddef.h>
/* Fixed-width boundary: Linux UAPI layouts remain in C, including on 32-bit. */
typedef struct { char card[32], driver[16], bus[32]; } camera_v4l2_info;
typedef struct {
    uint32_t fourcc, width, height, fps, fps_denominator, strides[2], planes, ycbcr, full_range;
} camera_v4l2_format;
typedef struct { uint32_t kind, min_w, max_w, step_w, min_h, max_h, step_h; } camera_v4l2_size;
typedef struct { uint32_t kind, min_n, min_d, max_n, max_d, step_n, step_d; } camera_v4l2_interval;
typedef struct { int32_t min, max, step, def; uint32_t read_only; } camera_v4l2_control;
typedef struct {
    const uint8_t *data[2];
    size_t lengths[2];
    int64_t timestamp_ns;
    uint32_t index, planes, damaged, sequence, monotonic;
} camera_v4l2_frame;
typedef struct camera_v4l2_stream camera_v4l2_stream;
int camera_v4l2_probe(int fd, camera_v4l2_info *info);
int camera_v4l2_enum_format(int fd, uint32_t index, uint32_t *fourcc);
int camera_v4l2_enum_size(int fd, uint32_t fourcc, uint32_t index, camera_v4l2_size *size);
int camera_v4l2_enum_interval(int fd, uint32_t fourcc, uint32_t w, uint32_t h, uint32_t index, camera_v4l2_interval *interval);
int camera_v4l2_format_set(int fd, camera_v4l2_format *format, int apply);
int camera_v4l2_start(int fd, uint32_t count, camera_v4l2_stream **out);
int camera_v4l2_next(camera_v4l2_stream *stream, int wake_fd, camera_v4l2_frame *frame);
int camera_v4l2_release(camera_v4l2_stream *stream, uint32_t index);
int camera_v4l2_stop(camera_v4l2_stream *stream);
int camera_v4l2_query_control(int fd, uint32_t id, camera_v4l2_control *control);
int camera_v4l2_get_control(int fd, uint32_t id, int32_t *value);
int camera_v4l2_set_control(int fd, uint32_t id, int32_t value);
int camera_v4l2_auto_exposure(int fd, int enabled);
#endif
