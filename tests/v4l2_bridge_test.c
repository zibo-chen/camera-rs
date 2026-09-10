/* Deterministic driver fault injection; no camera/root required.
 * cc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror tests/v4l2_bridge_test.c -o /tmp/v4l2-bridge-test
 */
#include <assert.h>
#include <errno.h>
#include <linux/videodev2.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

static unsigned long fail_request;
static unsigned mapped, unmapped, allocations_freed, queued, stream_off;
static int mplane, fail_map, poll_mode, invalid_index, invalid_bytes, invalid_offset, corrupt;
static unsigned char pixels[8][16];
static int fake_ioctl(int fd, unsigned long request, void *arg) {
    (void)fd;
    if (request == fail_request) { errno = EIO; return -1; }
    if (request == VIDIOC_QUERYCAP) {
        struct v4l2_capability *c = arg;
        c->capabilities = V4L2_CAP_DEVICE_CAPS | V4L2_CAP_VIDEO_OUTPUT;
        c->device_caps = V4L2_CAP_STREAMING | (mplane ? V4L2_CAP_VIDEO_CAPTURE_MPLANE : V4L2_CAP_VIDEO_CAPTURE);
    } else if (request == VIDIOC_G_FMT) {
        struct v4l2_format *f = arg;
        f->fmt.pix_mp.num_planes = mplane ? 2 : 1;
    } else if (request == VIDIOC_REQBUFS) {
        struct v4l2_requestbuffers *r = arg;
        if (!r->count) allocations_freed++;
        else r->count = 3;
    } else if (request == VIDIOC_QUERYBUF) {
        struct v4l2_buffer *b = arg;
        if (mplane) {
            for (unsigned i = 0; i < b->length; ++i) { b->m.planes[i].length = 16; b->m.planes[i].m.mem_offset = (b->index * 2 + i) * 16; }
        } else { b->length = 16; b->m.offset = b->index * 16; }
    } else if (request == VIDIOC_QBUF) { queued++; }
    else if (request == VIDIOC_STREAMOFF) { stream_off++; }
    else if (request == VIDIOC_DQBUF) {
        struct v4l2_buffer *b = arg;
        b->index = invalid_index ? 500 : 1;
        b->timestamp.tv_sec = 1; b->timestamp.tv_usec = 2;
        b->flags = corrupt ? V4L2_BUF_FLAG_ERROR : 0;
        if (mplane) {
            for (unsigned i = 0; i < b->length; ++i) {
                b->m.planes[i].bytesused = invalid_bytes ? 100 : 8;
                b->m.planes[i].data_offset = invalid_offset ? 9 : 2;
            }
        } else { b->bytesused = invalid_bytes ? 100 : 8; }
    }
    return 0;
}
static void *fake_mmap(void *address, size_t length, int prot, int flags, int fd, off_t offset) {
    (void)address; (void)length; (void)prot; (void)flags; (void)fd;
    if (fail_map && mapped == 1) { errno = ENOMEM; return MAP_FAILED; }
    mapped++; assert(offset / 16 < 8); return pixels[offset / 16];
}
static int fake_munmap(void *address, size_t length) { (void)address; assert(length == 16); unmapped++; return 0; }
static int fake_poll(struct pollfd *fds, nfds_t nfds, int timeout) {
    assert(nfds == 2 && timeout > 0);
    if (poll_mode == 1) return 0;
    if (poll_mode == 2) fds[1].revents = POLLIN;
    else fds[0].revents = poll_mode == 3 ? POLLHUP : POLLIN;
    return 1;
}
#define ioctl(fd, request, arg) fake_ioctl(fd, request, arg)
#define mmap fake_mmap
#define munmap fake_munmap
#define poll fake_poll
#include "../cpp/v4l2_bridge.c"

static void reset(int multi) {
    fail_request = 0; mapped = unmapped = allocations_freed = queued = stream_off = 0;
    mplane = multi; fail_map = poll_mode = invalid_index = invalid_bytes = invalid_offset = corrupt = 0;
}
static void capture_and_release(int multi) {
    reset(multi);
    camera_v4l2_stream *s = NULL; camera_v4l2_frame f;
    assert(camera_v4l2_start(10, 4, &s) == 0 && s);
    assert(queued == 3 && mapped == (multi ? 6U : 3U));
    assert(camera_v4l2_next(s, 11, &f) == 0);
    assert(f.index == 1 && f.planes == (multi ? 2U : 1U));
    assert(f.timestamp_ns == 1000002000);
    assert(f.lengths[0] == (multi ? 6U : 8U));
    assert(f.data[0] == pixels[multi ? 2 : 1] + (multi ? 2 : 0));
    assert(camera_v4l2_next(s, 11, &f) == -EBUSY);
    assert(camera_v4l2_release(s, 0) == -EINVAL);
    assert(camera_v4l2_release(s, 1) == 0 && queued == 4);
    poll_mode = 1; assert(camera_v4l2_next(s, 11, &f) == -ETIMEDOUT);
    poll_mode = 2; assert(camera_v4l2_next(s, 11, &f) == -ECANCELED);
    poll_mode = 3; assert(camera_v4l2_next(s, 11, &f) == -ENODEV);
    poll_mode = 0; corrupt = 1;
    assert(camera_v4l2_next(s, 11, &f) == 0 && f.damaged == 1);
    assert(camera_v4l2_release(s, f.index) == 0);
    assert(camera_v4l2_stop(s) == 0);
    assert(unmapped == mapped && allocations_freed == 1 && stream_off == 1);
}
int main(void) {
    capture_and_release(0); capture_and_release(1);
    for (int multi = 0; multi <= 1; ++multi) {
        const unsigned long errors[] = {VIDIOC_QUERYBUF, VIDIOC_QBUF, VIDIOC_STREAMON};
        for (unsigned i = 0; i < sizeof(errors) / sizeof(errors[0]); ++i) {
            reset(multi); fail_request = errors[i]; camera_v4l2_stream *s = NULL;
            assert(camera_v4l2_start(10, 4, &s) == -EIO && !s);
            assert(mapped == unmapped && allocations_freed == 1);
        }
        reset(multi); fail_map = 1; camera_v4l2_stream *s = NULL;
        assert(camera_v4l2_start(10, 4, &s) == -ENOMEM && !s);
        assert(mapped == unmapped && allocations_freed == 1);
        for (int fault = 0; fault < 3; ++fault) {
            if (!multi && fault == 2) continue;
            reset(multi); assert(camera_v4l2_start(10, 4, &s) == 0);
            invalid_index = fault == 0; invalid_bytes = fault == 1; invalid_offset = fault == 2;
            camera_v4l2_frame f;
            assert(camera_v4l2_next(s, 11, &f) == -EIO);
            assert(camera_v4l2_stop(s) == 0);
            assert(mapped == unmapped && stream_off == 1);
        }
        reset(multi); assert(camera_v4l2_start(10, 4, &s) == 0);
        fail_request = VIDIOC_STREAMOFF;
        assert(camera_v4l2_stop(s) == -EIO && mapped == unmapped && allocations_freed == 1);
    }
    puts("V4L2 bridge fault-injection tests passed (single/multi-plane, startup rollback, invalid buffers, cancellation, disconnect, cleanup)");
    return 0;
}
