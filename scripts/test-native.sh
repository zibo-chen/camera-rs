#!/usr/bin/env bash
set -euo pipefail
camera_root="$(cd "$(dirname "$0")/.." && pwd)"
camera_tmp="$(mktemp -d)"
trap 'rm -rf "$camera_tmp"' EXIT
cd "$camera_root"
"${CXX:-c++}" -std=c++11 -Wall -Wextra -Werror tests/camera2_metadata_test.cpp -o "$camera_tmp/metadata"
"$camera_tmp/metadata"
case "$(uname -s)" in
 Darwin) camera_link=(-Wl,-dead_strip) ;;
 Linux) camera_link=(-Wl,--gc-sections) ;;
 *) exit 0 ;;
esac
"${CC:-cc}" -std=c11 -D_POSIX_C_SOURCE=200809L -ffunction-sections -fdata-sections \
 -I3rdparty/libuvc/include -I3rdparty/libusb/libusb tests/uvc_lifecycle_test.c \
 "${camera_link[@]}" -pthread -fsanitize=address,undefined -o "$camera_tmp/uvc"
"$camera_tmp/uvc"
"${CC:-cc}" -std=c11 -D_POSIX_C_SOURCE=200809L -D__ANDROID__ -ffunction-sections -fdata-sections \
 -I3rdparty/libuvc/include -I3rdparty/libusb/libusb tests/uvc_lifecycle_test.c \
 "${camera_link[@]}" -pthread -fsanitize=address,undefined -o "$camera_tmp/uvc-android-handoff"
"$camera_tmp/uvc-android-handoff"
"${CC:-cc}" -std=gnu11 -ffunction-sections -fdata-sections \
 -I3rdparty/libuvc/include -I3rdparty/libusb/libusb tests/uvc_negotiation_test.c \
 "${camera_link[@]}" -pthread -fsanitize=address,undefined -o "$camera_tmp/uvc-negotiation"
"$camera_tmp/uvc-negotiation"
for camera_android in native android; do
 camera_defines=()
 if [[ "$camera_android" == android ]]; then camera_defines=(-D__ANDROID__); fi
 "${CC:-cc}" -std=gnu11 "${camera_defines[@]}" -ffunction-sections -fdata-sections \
  -I3rdparty/libuvc/include -I3rdparty/libusb/libusb tests/uvc_transfer_budget_test.c \
  "${camera_link[@]}" -pthread -fsanitize=address,undefined -o "$camera_tmp/uvc-budget-$camera_android"
 "$camera_tmp/uvc-budget-$camera_android"
done
if [[ $(uname -s) == Linux ]]; then
 for camera_test in v4l2_bridge v4l2_control_modes; do
  "${CC:-cc}" -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror \
   -fsanitize=address,undefined "tests/${camera_test}_test.c" -o "$camera_tmp/$camera_test"
  "$camera_tmp/$camera_test"
 done
fi
