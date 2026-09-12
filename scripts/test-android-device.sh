#!/usr/bin/env bash
# Build/install the isolated diagnostic app, then run a physical-device contract.
# Uses normal CAMERA/UsbManager permissions. Never changes /dev permissions,
# restarts system services, or silently turns a failing instrumentation run green.
set -euo pipefail
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME}"
camera_serial="${1:?Usage: test-android-device.sh SERIAL BACKEND [instrumentation -e key value ...]}"
camera_backend="${2:?Specify camera2, uvc, or v4l2}"
shift 2
case "$camera_backend" in camera2|uvc|v4l2) ;; *) exit 2 ;; esac
camera_root="$(cd "$(dirname "$0")/.." && pwd)"
camera_adb="${ADB:-adb}"
camera_log="${CAMERA_TEST_LOG:-$(mktemp)}"
camera_v4l2_device="${CAMERA_V4L2_DEVICE:-/dev/video0}"
camera_original_mode=""
restore_v4l2_mode() {
  if [[ -n "$camera_original_mode" ]]; then
    "$camera_adb" -s "$camera_serial" shell "su 0 chmod $camera_original_mode $camera_v4l2_device" >/dev/null 2>&1 || true
  fi
}
cd "$camera_root/android_example"
./gradlew assembleDebug assembleDebugAndroidTest
"$camera_adb" -s "$camera_serial" install -r -g build/outputs/apk/debug/camera-demo-debug.apk
"$camera_adb" -s "$camera_serial" install -r -g build/outputs/apk/androidTest/debug/camera-demo-debug-androidTest.apk
if [[ "$camera_backend" == "v4l2" && "${CAMERA_V4L2_ROOT:-0}" == "1" ]]; then
  if [[ ! "$camera_v4l2_device" =~ ^/dev/video[0-9]+$ ]]; then
    echo "CAMERA_V4L2_DEVICE must be an explicit /dev/videoN node" >&2
    exit 2
  fi
  camera_original_mode="$("$camera_adb" -s "$camera_serial" shell "su 0 stat -c %a $camera_v4l2_device" | tr -d '\r')"
  trap restore_v4l2_mode EXIT
  "$camera_adb" -s "$camera_serial" shell "su 0 chmod 666 $camera_v4l2_device"
fi
"$camera_adb" -s "$camera_serial" shell am instrument -w \
 -e class com.medivh.camera.BackendInstrumentedTest#captureLifecycleAndBuffers \
 -e backend "$camera_backend" "$@" \
 com.medivh.camera.demo.test/androidx.test.runner.AndroidJUnitRunner | tee "$camera_log"
echo "Instrumentation log: $camera_log"
# am instrument often exits 0 when the JUnit test failed.
grep -q '^OK (1 test)' "$camera_log"
