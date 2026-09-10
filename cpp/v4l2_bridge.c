#include "v4l2_bridge.h"
#include <errno.h>
#include <linux/videodev2.h>
#include <poll.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

/* All functions return zero on success or a negative errno. */
static int xioctl(int fd, unsigned long request, void *arg) {
    int rc;
    do { rc = ioctl(fd, request, arg); } while (rc < 0 && errno == EINTR);
    return rc < 0 ? -errno : 0;
}

static int capture_type(int fd, enum v4l2_buf_type *type, camera_v4l2_info *info) {
    struct v4l2_capability cap = {0};
    int rc = xioctl(fd, VIDIOC_QUERYCAP, &cap);
    if (rc) return rc;
    uint32_t flags = (cap.capabilities & V4L2_CAP_DEVICE_CAPS) ? cap.device_caps : cap.capabilities;
    if (!(flags & V4L2_CAP_STREAMING)) return -ENOTSUP;
    if (flags & V4L2_CAP_VIDEO_CAPTURE) *type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    else if (flags & V4L2_CAP_VIDEO_CAPTURE_MPLANE) *type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    else return -ENOTSUP;
    if (info) {
        memcpy(info->card, cap.card, sizeof(info->card));
        memcpy(info->driver, cap.driver, sizeof(info->driver));
        memcpy(info->bus, cap.bus_info, sizeof(info->bus));
    }
    return 0;
}
int camera_v4l2_probe(int fd, camera_v4l2_info *info) {
    enum v4l2_buf_type type;
    return capture_type(fd, &type, info);
}
int camera_v4l2_enum_format(int fd, uint32_t index, uint32_t *fourcc) {
    enum v4l2_buf_type type;
    int rc = capture_type(fd, &type, NULL);
    if (rc) return rc;
    struct v4l2_fmtdesc desc = {0};
    desc.index = index; desc.type = type;
    rc = xioctl(fd, VIDIOC_ENUM_FMT, &desc);
    if (!rc) *fourcc = desc.pixelformat;
    return rc;
}
int camera_v4l2_enum_size(int fd, uint32_t fourcc, uint32_t index, camera_v4l2_size *out) {
    struct v4l2_frmsizeenum size = {0};
    size.index = index; size.pixel_format = fourcc;
    int rc = xioctl(fd, VIDIOC_ENUM_FRAMESIZES, &size);
    if (rc) return rc;
    memset(out, 0, sizeof(*out));
    out->kind = size.type;
    if (size.type == V4L2_FRMSIZE_TYPE_DISCRETE) {
        out->min_w = out->max_w = size.discrete.width;
        out->min_h = out->max_h = size.discrete.height;
        out->step_w = out->step_h = 1;
    } else {
        out->min_w = size.stepwise.min_width; out->max_w = size.stepwise.max_width;
        out->min_h = size.stepwise.min_height; out->max_h = size.stepwise.max_height;
        out->step_w = size.stepwise.step_width; out->step_h = size.stepwise.step_height;
    }
    return 0;
}
int camera_v4l2_enum_interval(int fd, uint32_t fourcc, uint32_t w, uint32_t h, uint32_t index, camera_v4l2_interval *out) {
    struct v4l2_frmivalenum val = {0};
    val.index = index; val.pixel_format = fourcc; val.width = w; val.height = h;
    int rc = xioctl(fd, VIDIOC_ENUM_FRAMEINTERVALS, &val);
    if (rc) return rc;
    memset(out, 0, sizeof(*out)); out->kind = val.type;
    if (val.type == V4L2_FRMIVAL_TYPE_DISCRETE) {
        out->min_n = out->max_n = val.discrete.numerator;
        out->min_d = out->max_d = val.discrete.denominator;
    } else {
        out->min_n = val.stepwise.min.numerator; out->min_d = val.stepwise.min.denominator;
        out->max_n = val.stepwise.max.numerator; out->max_d = val.stepwise.max.denominator;
        out->step_n = val.stepwise.step.numerator; out->step_d = val.stepwise.step.denominator;
    }
    return 0;
}
int camera_v4l2_format_set(int fd, camera_v4l2_format *out, int apply) {
    enum v4l2_buf_type type;
    int rc = capture_type(fd, &type, NULL);
    if (rc) return rc;
    struct v4l2_format fmt = {0}; fmt.type = type;
    if (type == V4L2_BUF_TYPE_VIDEO_CAPTURE) {
        fmt.fmt.pix.width = out->width; fmt.fmt.pix.height = out->height;
        fmt.fmt.pix.pixelformat = out->fourcc; fmt.fmt.pix.field = V4L2_FIELD_NONE;
    } else {
        fmt.fmt.pix_mp.width = out->width; fmt.fmt.pix_mp.height = out->height;
        fmt.fmt.pix_mp.pixelformat = out->fourcc; fmt.fmt.pix_mp.field = V4L2_FIELD_NONE;
    }
    rc = xioctl(fd, apply ? VIDIOC_S_FMT : VIDIOC_TRY_FMT, &fmt);
    if (rc) return rc;
    uint32_t field, colorspace, quantization;
    if (type == V4L2_BUF_TYPE_VIDEO_CAPTURE) {
        out->width = fmt.fmt.pix.width; out->height = fmt.fmt.pix.height;
        out->fourcc = fmt.fmt.pix.pixelformat; out->strides[0] = fmt.fmt.pix.bytesperline;
        out->planes = 1; field = fmt.fmt.pix.field;
        colorspace = fmt.fmt.pix.colorspace; quantization = fmt.fmt.pix.quantization; out->ycbcr = fmt.fmt.pix.ycbcr_enc;
    } else {
        out->width = fmt.fmt.pix_mp.width; out->height = fmt.fmt.pix_mp.height;
        out->fourcc = fmt.fmt.pix_mp.pixelformat; out->planes = fmt.fmt.pix_mp.num_planes;
        if (out->planes == 0 || out->planes > 2) return -ENOTSUP;
        for (uint32_t p = 0; p < out->planes; ++p) out->strides[p] = fmt.fmt.pix_mp.plane_fmt[p].bytesperline;
        field = fmt.fmt.pix_mp.field;
        colorspace = fmt.fmt.pix_mp.colorspace; quantization = fmt.fmt.pix_mp.quantization; out->ycbcr = fmt.fmt.pix_mp.ycbcr_enc;
    }
    if (!out->ycbcr) out->ycbcr = V4L2_MAP_YCBCR_ENC_DEFAULT(colorspace);
    if (!quantization) quantization = V4L2_MAP_QUANTIZATION_DEFAULT(out->fourcc == V4L2_PIX_FMT_RGB24, colorspace, out->ycbcr);
    out->full_range = quantization == V4L2_QUANTIZATION_FULL_RANGE;
    if (field != V4L2_FIELD_NONE) return -ENOTSUP;
    if (!apply) return 0;
    struct v4l2_streamparm parm = {0}; parm.type = type;
    rc = xioctl(fd, VIDIOC_G_PARM, &parm);
    if (rc == -EINVAL || rc == -ENOTTY) { out->fps = 0; return 0; }
    if (rc) return rc;
    if (parm.parm.capture.capability & V4L2_CAP_TIMEPERFRAME) {
        memset(&parm, 0, sizeof(parm)); parm.type = type;
        parm.parm.capture.timeperframe.numerator = out->fps_denominator;
        parm.parm.capture.timeperframe.denominator = out->fps;
        rc = xioctl(fd, VIDIOC_S_PARM, &parm);
        if (rc) return rc;
    }
    uint32_t n = parm.parm.capture.timeperframe.numerator;
    uint32_t d = parm.parm.capture.timeperframe.denominator;
    out->fps = d; out->fps_denominator = n;
    return 0;
}

struct mapping { void *address; size_t length; };
struct camera_v4l2_stream {
    int fd, active;
    enum v4l2_buf_type type;
    uint32_t count, planes, dequeued;
    struct mapping *maps;
};
static void buffer_init(camera_v4l2_stream *s, struct v4l2_buffer *buf, struct v4l2_plane *planes, uint32_t index) {
    memset(buf, 0, sizeof(*buf)); memset(planes, 0, sizeof(*planes) * VIDEO_MAX_PLANES);
    buf->type = s->type; buf->memory = V4L2_MEMORY_MMAP; buf->index = index;
    if (s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE) {
        buf->length = s->planes; buf->m.planes = planes;
    }
}
int camera_v4l2_stop(camera_v4l2_stream *s) {
    if (!s) return 0;
    int rc = 0;
    if (s->active) rc = xioctl(s->fd, VIDIOC_STREAMOFF, &s->type);
    if (s->maps) {
        for (uint32_t i = 0; i < s->count * s->planes; ++i) {
            if (s->maps[i].length) munmap(s->maps[i].address, s->maps[i].length);
        }
    }
    struct v4l2_requestbuffers req = {0}; req.type = s->type; req.memory = V4L2_MEMORY_MMAP;
    int free_rc = xioctl(s->fd, VIDIOC_REQBUFS, &req);
    free(s->maps); free(s);
    return rc ? rc : free_rc;
}
int camera_v4l2_release(camera_v4l2_stream *s, uint32_t index) {
    if (index >= s->count || index != s->dequeued) return -EINVAL;
    struct v4l2_buffer buf; struct v4l2_plane planes[VIDEO_MAX_PLANES];
    buffer_init(s, &buf, planes, index);
    if (s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE) {
        for (uint32_t p = 0; p < s->planes; ++p) planes[p].length = s->maps[index * s->planes + p].length;
    }
    int rc = xioctl(s->fd, VIDIOC_QBUF, &buf);
    if (!rc) s->dequeued = UINT32_MAX;
    return rc;
}
int camera_v4l2_start(int fd, uint32_t count, camera_v4l2_stream **out) {
    *out = NULL;
    if (count < 2 || count > 32) return -EINVAL;
    camera_v4l2_stream *s = calloc(1, sizeof(*s));
    if (!s) return -ENOMEM;
    s->fd = fd; s->dequeued = UINT32_MAX;
    int rc = capture_type(fd, &s->type, NULL);
    if (rc) { free(s); return rc; }
    struct v4l2_format fmt = {0}; fmt.type = s->type;
    rc = xioctl(fd, VIDIOC_G_FMT, &fmt);
    if (rc) { free(s); return rc; }
    s->planes = s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE ? 1 : fmt.fmt.pix_mp.num_planes;
    if (!s->planes || s->planes > 2) { free(s); return -ENOTSUP; }
    struct v4l2_requestbuffers req = {0}; req.type = s->type; req.memory = V4L2_MEMORY_MMAP; req.count = count;
    rc = xioctl(fd, VIDIOC_REQBUFS, &req);
    if (rc) { free(s); return rc; }
    s->count = req.count;
    if (s->count < 2 || s->count > 32) { rc = -ENOMEM; goto fail; }
    s->maps = calloc(s->count * s->planes, sizeof(*s->maps));
    if (!s->maps) { rc = -ENOMEM; goto fail; }
    for (uint32_t i = 0; i < s->count; ++i) {
        struct v4l2_buffer buf; struct v4l2_plane planes[VIDEO_MAX_PLANES];
        buffer_init(s, &buf, planes, i);
        rc = xioctl(fd, VIDIOC_QUERYBUF, &buf);
        if (rc) goto fail;
        if (s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE && buf.length != s->planes) { rc = -EIO; goto fail; }
        for (uint32_t p = 0; p < s->planes; ++p) {
            size_t length = s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE ? buf.length : planes[p].length;
            off_t offset = s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE ? buf.m.offset : planes[p].m.mem_offset;
            if (!length || length > 128U * 1024 * 1024) { rc = -EIO; goto fail; }
            void *address = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, offset);
            if (address == MAP_FAILED) { rc = -errno; goto fail; }
            s->maps[i * s->planes + p] = (struct mapping){address, length};
        }
        s->dequeued = i;
        rc = camera_v4l2_release(s, i);
        if (rc) goto fail;
    }
    rc = xioctl(fd, VIDIOC_STREAMON, &s->type);
    if (rc) goto fail;
    s->active = 1; *out = s; return 0;
fail:
    camera_v4l2_stop(s); return rc;
}
int camera_v4l2_next(camera_v4l2_stream *s, int wake_fd, camera_v4l2_frame *out) {
    if (s->dequeued != UINT32_MAX) return -EBUSY;
    struct pollfd fds[2] = {{s->fd, POLLIN, 0}, {wake_fd, POLLIN, 0}};
    int rc;
    do { rc = poll(fds, 2, 1000); } while (rc < 0 && errno == EINTR);
    if (rc < 0) return -errno;
    if (!rc) return -ETIMEDOUT;
    if (fds[1].revents) return -ECANCELED;
    if (fds[0].revents & (POLLHUP | POLLNVAL)) return -ENODEV;
    if (!(fds[0].revents & POLLIN)) return -EIO;
    struct v4l2_buffer buf; struct v4l2_plane planes[VIDEO_MAX_PLANES];
    buffer_init(s, &buf, planes, 0);
    rc = xioctl(s->fd, VIDIOC_DQBUF, &buf);
    if (rc) return rc;
    if (buf.index >= s->count) return -EIO;
    s->dequeued = buf.index;
    if (s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE && buf.length != s->planes) return -EIO;
    memset(out, 0, sizeof(*out)); out->index = buf.index; out->planes = s->planes;
    out->damaged = !!(buf.flags & V4L2_BUF_FLAG_ERROR);
    /* Native clock remains separate from the host wall clock in FrameHub. */
    if (buf.timestamp.tv_sec >= 0 && buf.timestamp.tv_sec <= (INT64_MAX - 999999999) / 1000000000 &&
        buf.timestamp.tv_usec >= 0 && buf.timestamp.tv_usec < 1000000)
    out->sequence = buf.sequence;
    out->monotonic = (buf.flags & V4L2_BUF_FLAG_TIMESTAMP_MASK) == V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC;
        out->timestamp_ns = (int64_t)buf.timestamp.tv_sec * 1000000000 + buf.timestamp.tv_usec * 1000;
    for (uint32_t p = 0; p < s->planes; ++p) {
        struct mapping *m = &s->maps[buf.index * s->planes + p];
        size_t used = s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE ? buf.bytesused : planes[p].bytesused;
        size_t offset = s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE ? 0 : planes[p].data_offset;
        if (used > m->length || offset > used) return -EIO;
        out->data[p] = (const uint8_t *)m->address + offset; out->lengths[p] = used - offset;
    }
    return 0;
}
int camera_v4l2_query_control(int fd, uint32_t id, camera_v4l2_control *out) {
    struct v4l2_queryctrl q = {0}; q.id = id;
    int rc = xioctl(fd, VIDIOC_QUERYCTRL, &q);
    if (rc) return rc;
    if (q.flags & (V4L2_CTRL_FLAG_DISABLED | V4L2_CTRL_FLAG_WRITE_ONLY)) return -ENOTSUP;
    if (q.type != V4L2_CTRL_TYPE_INTEGER && q.type != V4L2_CTRL_TYPE_BOOLEAN && q.type != V4L2_CTRL_TYPE_MENU) return -ENOTSUP;
    out->min = q.minimum; out->max = q.maximum; out->step = q.step; out->def = q.default_value;
    out->read_only = !!(q.flags & V4L2_CTRL_FLAG_READ_ONLY);
    return 0;
}
int camera_v4l2_get_control(int fd, uint32_t id, int32_t *value) {
    struct v4l2_control c = {0}; c.id = id;
    int rc = xioctl(fd, VIDIOC_G_CTRL, &c);
    if (!rc) *value = c.value;
    return rc;
}
int camera_v4l2_set_control(int fd, uint32_t id, int32_t value) {
    struct v4l2_control c = {0}; c.id = id; c.value = value;
    return xioctl(fd, VIDIOC_S_CTRL, &c);
}
int camera_v4l2_auto_exposure(int fd, int enabled) {
    int mode = V4L2_EXPOSURE_MANUAL;
    if (enabled) {
        struct v4l2_querymenu menu = {0}; menu.id = V4L2_CID_EXPOSURE_AUTO; menu.index = V4L2_EXPOSURE_APERTURE_PRIORITY;
        mode = xioctl(fd, VIDIOC_QUERYMENU, &menu) == 0 ? V4L2_EXPOSURE_APERTURE_PRIORITY : V4L2_EXPOSURE_AUTO;
    }
    return camera_v4l2_set_control(fd, V4L2_CID_EXPOSURE_AUTO, mode);
}
