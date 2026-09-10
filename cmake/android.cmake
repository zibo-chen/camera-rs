# Used by Cargo's CMake dependencies (e.g. turbojpeg-sys). Camera2 needs API 24.
if(NOT DEFINED ENV{ANDROID_NDK_HOME})
  message(FATAL_ERROR "Set ANDROID_NDK_HOME to an installed Android NDK")
endif()
set(ANDROID_PLATFORM android-24 CACHE STRING "Camera library minimum Android API")
include("$ENV{ANDROID_NDK_HOME}/build/cmake/android.toolchain.cmake")
