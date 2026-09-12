#!/usr/bin/env bash
set -euo pipefail
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to an installed Android NDK}"
camera_root="$(cd "$(dirname "$0")/.." && pwd)"
target="${1:-aarch64-linux-android}"
if (($#)); then shift; fi
case "$target" in
 aarch64-linux-android) clang_target=aarch64-linux-android24; abi=arm64-v8a ;;
 armv7-linux-androideabi) clang_target=armv7a-linux-androideabi24; abi=armeabi-v7a ;;
 *) echo "Supported targets: aarch64-linux-android, armv7-linux-androideabi" >&2; exit 2 ;;
esac
if [[ "$target" == armv7-linux-androideabi ]]; then
 export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_RUSTFLAGS="-C target-feature=+neon"
 export CFLAGS_armv7_linux_androideabi="-mfpu=neon-vfpv4"
 export CXXFLAGS_armv7_linux_androideabi="-mfpu=neon-vfpv4"
fi
prebuilt=("$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*)
camera_tools="${prebuilt[0]}/bin"
key="${target//-/_}"
upper="$(printf '%s' "$key" | tr '[:lower:]' '[:upper:]')"
env "CC_$key=$camera_tools/$clang_target-clang" \
 "CXX_$key=$camera_tools/$clang_target-clang++" \
 "AR_$key=$camera_tools/llvm-ar" \
 "CARGO_TARGET_${upper}_LINKER=$camera_tools/$clang_target-clang" \
 "CMAKE_TOOLCHAIN_FILE_$key=$camera_root/cmake/android.cmake" \
 cargo build --manifest-path "$camera_root/camera-android/Cargo.toml" --release --target "$target" "$@"
mkdir -p "$camera_root/android_example/src/main/jniLibs/$abi"
cp "$camera_root/camera-android/target/$target/release/libcamera_android.so" "$camera_root/android_example/src/main/jniLibs/$abi/"
# The Camera2 C++ bridge links the NDK shared C++ runtime. Package the matching ABI.
case "$target" in
 aarch64-linux-android) runtime_triple=aarch64-linux-android ;;
 armv7-linux-androideabi) runtime_triple=arm-linux-androideabi ;;
esac
cp "${prebuilt[0]}/sysroot/usr/lib/$runtime_triple/libc++_shared.so" "$camera_root/android_example/src/main/jniLibs/$abi/"
