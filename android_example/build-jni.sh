#!/usr/bin/env bash
set -euo pipefail
camera_root="$(cd "$(dirname "$0")/.." && pwd)"
target="${1:-aarch64-linux-android}"
if (($# <= 1)); then
  exec bash "$camera_root/scripts/build-android-adapter.sh" "$target" \
    --no-default-features --features camera2,uvc,v4l2,convert-rgb,decode-mjpeg
fi
exec bash "$camera_root/scripts/build-android-adapter.sh" "$@"
