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
cd "$camera_root/android_example"
./gradlew assembleDebug assembleDebugAndroidTest
"$camera_adb" -s "$camera_serial" install -r -g build/outputs/apk/debug/camera-demo-debug.apk
"$camera_adb" -s "$camera_serial" install -r -g build/outputs/apk/androidTest/debug/camera-demo-debug-androidTest.apk
"$camera_adb" -s "$camera_serial" shell am instrument -w \
 -e class com.medivh.camera.BackendInstrumentedTest#captureLifecycleAndBuffers \
 -e backend "$camera_backend" "$@" \
 com.medivh.camera.demo.test/androidx.test.runner.AndroidJUnitRunner | tee "$camera_log"
echo "Instrumentation log: $camera_log"
# am instrument often exits 0 when the JUnit test failed.
grep -q '^OK (1 test)' "$camera_log"
