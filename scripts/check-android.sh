#!/usr/bin/env bash
set -euo pipefail
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to an installed Android NDK}"
target="${1:-aarch64-linux-android}"
if (($#)); then shift; fi
case "$target" in
  aarch64-linux-android) clang_target=aarch64-linux-android24 ;;
  armv7-linux-androideabi) clang_target=armv7a-linux-androideabi24 ;;
  *) echo "Supported targets: aarch64-linux-android, armv7-linux-androideabi" >&2; exit 2 ;;
esac
if [[ "$target" == armv7-linux-androideabi ]]; then
  export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_RUSTFLAGS="-C target-feature=+neon"
  export CFLAGS_armv7_linux_androideabi="-mfpu=neon-vfpv4"
  export CXXFLAGS_armv7_linux_androideabi="-mfpu=neon-vfpv4"
fi
camera_root="$(cd "$(dirname "$0")/.." && pwd)"
prebuilt=("$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*)
camera_tools="${prebuilt[0]}/bin"
[ -x "$camera_tools/$clang_target-clang" ]
key="${target//-/_}"
upper="$(printf '%s' "$key" | tr '[:lower:]' '[:upper:]')"
cd "$camera_root"
env "CC_$key=$camera_tools/$clang_target-clang" \
    "CXX_$key=$camera_tools/$clang_target-clang++" \
    "AR_$key=$camera_tools/llvm-ar" \
    "CARGO_TARGET_${upper}_LINKER=$camera_tools/$clang_target-clang" \
    "CMAKE_TOOLCHAIN_FILE_$key=$camera_root/cmake/android.cmake" \
    cargo build --lib --target "$target" --no-default-features \
    --features backend-uvc,backend-camera2,backend-v4l2 "$@"
