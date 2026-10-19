#include <assert.h>
#include <errno.h>
#include <linux/videodev2.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
static uint32_t supported;
static int writes, last_mode, menu_failure, query_failure, write_failure;
static int control_type = V4L2_CTRL_TYPE_MENU, control_flags, min_mode,
           max_mode = 3;
static int fake_ioctl(int fd, unsigned long command, void *arg) {
  (void)fd;
  if (command == VIDIOC_QUERYCTRL) {
    if (query_failure) {
      errno = query_failure;
      return -1;
    }
    struct v4l2_queryctrl *q = arg;
    q->type = control_type;
    q->flags = control_flags;
    q->minimum = min_mode;
    q->maximum = max_mode;
    q->step = 1;
    q->default_value = 1;
    return 0;
  }
  if (command == VIDIOC_QUERYMENU) {
    struct v4l2_querymenu *m = arg;
    if (menu_failure) {
      errno = EIO;
      return -1;
    }
    if (m->index < 4 && (supported & (1U << m->index)))
      return 0;
    errno = EINVAL;
    return -1;
  }
  if (command == VIDIOC_S_CTRL) {
    if (write_failure) {
      errno = write_failure;
      return -1;
    }
    struct v4l2_control *c = arg;
    ++writes;
    last_mode = c->value;
    if (c->value >= 0 && c->value < 4 && (supported & (1U << c->value)))
      return 0;
  }
  errno = EINVAL;
  return -1;
}
#define ioctl(fd, command, arg) fake_ioctl(fd, command, arg)
#include "../cpp/v4l2_bridge.c"
int main(void) {
  supported = 1U << V4L2_EXPOSURE_MANUAL;
  assert(camera_v4l2_auto_exposure(10, 1) == -ENOTSUP);
  assert(writes == 0);
  assert(camera_v4l2_auto_exposure(10, 0) == 0);
  assert(last_mode == V4L2_EXPOSURE_MANUAL);
  uint32_t modes = 0;
  assert(camera_v4l2_exposure_modes(10, &modes) == 0 && modes == 1);
  supported |= 1U << V4L2_EXPOSURE_APERTURE_PRIORITY;
  assert(camera_v4l2_exposure_modes(10, &modes) == 0 && modes == 3);
  assert(camera_v4l2_auto_exposure(10, 1) == 0);
  assert(last_mode == V4L2_EXPOSURE_APERTURE_PRIORITY);
  supported = (1U << V4L2_EXPOSURE_MANUAL) | (1U << V4L2_EXPOSURE_AUTO);
  assert(camera_v4l2_auto_exposure(10, 1) == 0 &&
         last_mode == V4L2_EXPOSURE_AUTO);
  supported = 1U << V4L2_EXPOSURE_AUTO;
  assert(camera_v4l2_exposure_modes(10, &modes) == 0 && modes == 2);
  assert(camera_v4l2_auto_exposure(10, 0) == -ENOTSUP);
  supported = 1U << V4L2_EXPOSURE_SHUTTER_PRIORITY;
  assert(camera_v4l2_exposure_modes(10, &modes) == -ENOTSUP);
  menu_failure = 1;
  assert(camera_v4l2_exposure_modes(10, &modes) == -EIO);
  menu_failure = 0;
  supported = 1U << V4L2_EXPOSURE_MANUAL;
  assert(camera_v4l2_exposure_modes(10, NULL) == -EINVAL);
  min_mode = max_mode = V4L2_EXPOSURE_MANUAL;
  assert(camera_v4l2_exposure_modes(10, &modes) == 0 && modes == 1);
  min_mode = 0;
  max_mode = 3;
  control_flags = V4L2_CTRL_FLAG_DISABLED;
  assert(camera_v4l2_exposure_modes(10, &modes) == -ENOTSUP);
  control_flags = 0;
  control_type = V4L2_CTRL_TYPE_INTEGER;
  assert(camera_v4l2_exposure_modes(10, &modes) == -ENOTSUP);
  control_type = V4L2_CTRL_TYPE_MENU;
  query_failure = EACCES;
  assert(camera_v4l2_exposure_modes(10, &modes) == -EACCES);
  assert(camera_v4l2_auto_exposure(10, 1) == -EACCES);
  query_failure = 0;
  write_failure = EIO;
  assert(camera_v4l2_auto_exposure(10, 0) == -EIO);
  puts("V4L2 exposure menu tests passed");
  return 0;
}
