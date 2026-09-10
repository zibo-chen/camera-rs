#!/usr/bin/env bash
set -euo pipefail
camera_root="$(cd "$(dirname "$0")/.." && pwd)"
exec bash "$camera_root/scripts/build-android-adapter.sh" "$@"
