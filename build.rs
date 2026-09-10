use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    println!("cargo:rustc-check-cfg=cfg(camera_v4l2)");
    let v4l2_enabled = (target_os == "linux" && env::var_os("CARGO_FEATURE_NATIVE").is_some())
        || (matches!(target_os.as_str(), "linux" | "android")
            && env::var_os("CARGO_FEATURE_BACKEND_V4L2").is_some());
    if v4l2_enabled {
        println!("cargo:rustc-cfg=camera_v4l2");
    }

    // 检查是否启用了 backend-uvc feature
    let backend_uvc_enabled = env::var("CARGO_FEATURE_BACKEND_UVC").is_ok()
        && matches!(target_os.as_str(), "linux" | "macos" | "android");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=3rdparty/");
    println!("cargo:rerun-if-changed=cpp/");
    println!(
        "cargo:warning=Building camera for {} {} (UVC: {})",
        target_arch, target_os, backend_uvc_enabled
    );

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let thirdparty_dir = manifest_dir.join("3rdparty");

    // Android 必须链接 log 库
    if target_os == "android" {
        println!("cargo:rustc-link-lib=log");
    }

    if backend_uvc_enabled && target_os == "macos" {
        println!("cargo:rustc-link-lib=framework=IOKit");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Security");
    }
    let apple_enabled = env::var_os("CARGO_FEATURE_NATIVE").is_some()
        || env::var_os("CARGO_FEATURE_BACKEND_AVFOUNDATION").is_some();
    if apple_enabled && matches!(target_os.as_str(), "macos" | "ios") {
        for framework in ["AVFoundation", "CoreMedia", "CoreVideo", "CoreFoundation"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }

    if v4l2_enabled {
        cc::Build::new()
            .file("cpp/v4l2_bridge.c")
            .flag_if_supported("-std=c11")
            .define("_GNU_SOURCE", None)
            .warnings(true)
            .compile("camera_v4l2");
    }

    // 仅在启用 backend-uvc feature 时编译 UVC 相关的 C 库
    if backend_uvc_enabled {
        compile_libusb(&thirdparty_dir, &target_os);
        compile_libuvc(&thirdparty_dir, &target_os);
        println!("cargo:warning=✓ UVC C 库编译完成");
    } else {
        println!("cargo:warning=跳过 UVC C 库编译 (未启用 backend-uvc feature)");
    }

    // 仅在 Android 平台且启用 backend-camera2 feature 时编译 NDK Camera2 桥接层
    let backend_camera2_enabled = env::var("CARGO_FEATURE_BACKEND_CAMERA2").is_ok()
        || env::var_os("CARGO_FEATURE_NATIVE").is_some();
    if backend_camera2_enabled && target_os == "android" {
        compile_ndk_camera2_bridge(&manifest_dir);
        println!("cargo:warning=✓ NDK Camera2 桥接层编译完成");
    } else if backend_camera2_enabled {
        println!("cargo:warning=跳过 NDK Camera2 编译 (非 Android 平台)");
    }
}

fn compile_libusb(thirdparty_dir: &Path, target_os: &str) {
    let libusb_dir = thirdparty_dir.join("libusb/libusb");

    if !libusb_dir.exists() {
        panic!("backend-uvc requires bundled libusb sources; reinstall the source package");
    }

    println!("cargo:warning=编译 libusb...");

    // 生成平台特定的 config.h
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    generate_libusb_config(&out_dir, target_os);

    let mut build = cc::Build::new();

    // 核心源文件
    build
        .file(libusb_dir.join("core.c"))
        .file(libusb_dir.join("descriptor.c"))
        .file(libusb_dir.join("hotplug.c"))
        .file(libusb_dir.join("io.c"))
        .file(libusb_dir.join("strerror.c"))
        .file(libusb_dir.join("sync.c"));

    // 平台特定源文件
    if target_os == "android" || target_os == "linux" {
        let os_dir = libusb_dir.join("os");
        build
            .file(os_dir.join("events_posix.c"))
            .file(os_dir.join("threads_posix.c"))
            .file(os_dir.join("linux_usbfs.c"))
            .file(os_dir.join("linux_netlink.c"));
    } else if target_os == "macos" {
        let os_dir = libusb_dir.join("os");
        build
            .file(os_dir.join("darwin_usb.c"))
            .file(os_dir.join("events_posix.c"))
            .file(os_dir.join("threads_posix.c"));
    } else if target_os == "windows" {
        println!("cargo:warning=⚠ Windows 不支持 libusb 直接访问 UVC 摄像头");
        println!("cargo:warning=  请使用 Windows Media Foundation 或 DirectShow");
        return;
    }

    // 包含目录 - 优先使用生成的 config.h
    build
        .include(&out_dir) // 生成的 config.h 所在目录
        .include(&libusb_dir)
        .include(thirdparty_dir.join("libusb"))
        .include(libusb_dir.join("os"));

    // 平台特定配置
    if target_os == "android" {
        let android_config = thirdparty_dir.join("libusb/android");
        if android_config.exists() {
            build.include(android_config);
        }
    }

    // 编译定义
    build
        .define("LIBUSB_DESCRIBE", "\"\"")
        .define("ENABLE_LOGGING", "1");

    if env::var_os("CARGO_FEATURE_NATIVE_DEBUG_LOGS").is_some() {
        build.define("ENABLE_DEBUG_LOGGING", "1");
    }

    if target_os == "android" || target_os == "linux" {
        build
            .define("OS_LINUX", "1")
            .define("HAVE_LINUX_NETLINK_H", "1")
            .define("USBI_TIMERFD_AVAILABLE", "1");
    } else if target_os == "macos" {
        build.define("OS_DARWIN", "1").define("PLATFORM_POSIX", "1");
    }

    // 编译选项
    build
        .flag_if_supported("-fPIC")
        .flag_if_supported("-std=gnu11")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-sign-compare")
        .warnings(false);

    // A failed bundled build must not silently select an unrelated system ABI.
    build.compile("usb-1.0");
}

fn generate_libusb_config(out_dir: &Path, target_os: &str) {
    use std::fs::File;
    use std::io::Write;

    let config_content = if target_os == "linux" || target_os == "android" {
        r#"/* libusb config.h - 适用于 Linux/Android */
#ifndef LIBUSB_CONFIG_H
#define LIBUSB_CONFIG_H

/* Linux specific */
#define OS_LINUX 1
#define PLATFORM_POSIX 1
#define THREADS_POSIX 1

/* Printf format attributes */
#if defined(__GNUC__) && (__GNUC__ > 4 || (__GNUC__ == 4 && __GNUC_MINOR__ >= 3))
#define PRINTF_FORMAT(a, b) __attribute__((__format__(__printf__, a, b)))
#else
#define PRINTF_FORMAT(a, b)
#endif

/* Default visibility */
#define DEFAULT_VISIBILITY __attribute__((visibility("default")))

/* Enable debug message logging */
#define ENABLE_LOGGING 1

/* Standard headers */
#define HAVE_DLFCN_H 1
#define HAVE_INTTYPES_H 1
#define HAVE_MEMORY_H 1
#define HAVE_STDINT_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRINGS_H 1
#define HAVE_STRING_H 1
#define HAVE_STRUCT_TIMESPEC 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_UNISTD_H 1
#define HAVE_GETTIMEOFDAY 1
#define HAVE_CLOCK_GETTIME 1

/* Linux specific features */
#define HAVE_TIMERFD 1
#define USBI_TIMERFD_AVAILABLE 1
#define HAVE_LINUX_NETLINK_H 1
#define HAVE_ASM_TYPES_H 1
#define HAVE_LINUX_FILTER_H 1

/* Package info */
#define PACKAGE "libusb-1.0"
#define PACKAGE_BUGREPORT "libusb-devel@lists.sourceforge.net"
#define PACKAGE_NAME "libusb-1.0"
#define PACKAGE_STRING "libusb-1.0 1.0.26"
#define PACKAGE_TARNAME "libusb-1.0"
#define PACKAGE_URL "http://libusb.info"
#define PACKAGE_VERSION "1.0.26"
#define VERSION "1.0.26"

#define POLL_NFDS_TYPE nfds_t
#define STDC_HEADERS 1
#define USE_SYSTEM_LOGGING_FACILITY 1
#define _GNU_SOURCE 1

#endif /* LIBUSB_CONFIG_H */
"#
    } else if target_os == "macos" {
        r#"/* libusb config.h - 适用于 macOS */
#ifndef LIBUSB_CONFIG_H
#define LIBUSB_CONFIG_H

/* Darwin/macOS specific */
#define OS_DARWIN 1
#define PLATFORM_POSIX 1
#define THREADS_POSIX 1

/* Printf format attributes */
#if defined(__GNUC__) && (__GNUC__ > 4 || (__GNUC__ == 4 && __GNUC_MINOR__ >= 3))
#define PRINTF_FORMAT(a, b) __attribute__((__format__(__printf__, a, b)))
#else
#define PRINTF_FORMAT(a, b)
#endif

/* Default visibility */
#define DEFAULT_VISIBILITY __attribute__((visibility("default")))

/* Enable debug message logging */
#define ENABLE_LOGGING 1

/* Standard headers */
#define HAVE_DLFCN_H 1
#define HAVE_INTTYPES_H 1
#define HAVE_MEMORY_H 1
#define HAVE_STDINT_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRINGS_H 1
#define HAVE_STRING_H 1
#define HAVE_STRUCT_TIMESPEC 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_UNISTD_H 1
#define HAVE_GETTIMEOFDAY 1
#define HAVE_CLOCK_GETTIME 1

/* macOS doesn't have timerfd */
#undef HAVE_TIMERFD
#undef USBI_TIMERFD_AVAILABLE

/* Package info */
#define PACKAGE "libusb-1.0"
#define PACKAGE_BUGREPORT "libusb-devel@lists.sourceforge.net"
#define PACKAGE_NAME "libusb-1.0"
#define PACKAGE_STRING "libusb-1.0 1.0.26"
#define PACKAGE_TARNAME "libusb-1.0"
#define PACKAGE_URL "http://libusb.info"
#define PACKAGE_VERSION "1.0.26"
#define VERSION "1.0.26"

#define POLL_NFDS_TYPE nfds_t
#define STDC_HEADERS 1
#define USE_SYSTEM_LOGGING_FACILITY 1
#define _GNU_SOURCE 1

#endif /* LIBUSB_CONFIG_H */
"#
    } else {
        r#"/* libusb config.h - generic */
#ifndef LIBUSB_CONFIG_H
#define LIBUSB_CONFIG_H
#define PLATFORM_POSIX 1
#define THREADS_POSIX 1
#define ENABLE_LOGGING 1
#define DEFAULT_VISIBILITY
#define PRINTF_FORMAT(a, b)
#endif
"#
    };

    let config_path = out_dir.join("config.h");
    let mut file = File::create(&config_path).expect("Failed to create config.h");
    file.write_all(config_content.as_bytes())
        .expect("Failed to write config.h");

    println!("cargo:warning=✓ 生成 config.h: {}", config_path.display());
}

fn compile_libuvc(thirdparty_dir: &Path, target_os: &str) {
    let libuvc_dir = thirdparty_dir.join("libuvc");
    let src_dir = libuvc_dir.join("src");

    if !src_dir.exists() {
        panic!("backend-uvc requires vendored libuvc sources");
    }

    println!("cargo:warning=编译 libuvc...");

    let mut build = cc::Build::new();

    // 所有源文件
    build
        .file(src_dir.join("ctrl.c"))
        .file(src_dir.join("ctrl-gen.c"))
        .file(src_dir.join("device.c"))
        .file(src_dir.join("diag.c"))
        .file(src_dir.join("frame.c"))
        .file(src_dir.join("init.c"))
        .file(src_dir.join("stream.c"))
        .file(src_dir.join("misc.c"))
        .file("cpp/uvc_abi_probe.c");

    // MJPEG 支持 - 不需要编译 C 库，使用 Rust 的 turbojpeg crate
    // frame-mjpeg.c 会提供原始 MJPEG 数据，我们在 Rust 层解码
    println!("cargo:warning=✓ MJPEG 支持通过 turbojpeg Rust crate 提供");

    // 包含目录
    build
        .include(libuvc_dir.join("include"))
        .include(libuvc_dir.join("include/libuvc"))
        .include(thirdparty_dir.join("libusb/libusb"));

    if env::var_os("CARGO_FEATURE_NATIVE_DEBUG_LOGS").is_some() {
        build.define("CAMERA_NATIVE_DEBUG_LOGS", "1");
    }

    // libuvc 版本定义
    build
        .define("LIBUVC_VERSION_MAJOR", "0")
        .define("LIBUVC_VERSION_MINOR", "0")
        .define("LIBUVC_VERSION_PATCH", "6")
        .define("LIBUVC_VERSION_STR", "\"0.0.6\"")
        .define("LIBUVC_VERSION_INT", "0x000006");

    if target_os == "android" {
        build.define("__ANDROID__", "1");
    }

    // 编译选项
    build
        .flag_if_supported("-fPIC")
        .flag_if_supported("-std=gnu11")
        .flag_if_supported("-Wno-everything")
        .warnings(false);

    build.try_compile("uvc").unwrap_or_else(|e| {
        panic!("libuvc compilation failed: {}", e);
    });

    println!("cargo:warning=✓ libuvc 编译完成");
}

fn compile_ndk_camera2_bridge(manifest_dir: &Path) {
    let cpp_dir = manifest_dir.join("cpp");
    let source_file = cpp_dir.join("ndk_camera2_bridge.cpp");

    if !source_file.exists() {
        panic!("backend-camera2 requires cpp/ndk_camera2_bridge.cpp");
    }

    println!("cargo:warning=编译 NDK Camera2 桥接层...");

    let mut build = cc::Build::new();

    build
        .file(&source_file)
        .cpp(true)
        .include(&cpp_dir)
        .flag("-std=c++17")
        .flag_if_supported("-fPIC")
        .flag_if_supported("-Wno-unused-parameter")
        .warnings(false);

    match build.try_compile("ndk_camera2_bridge") {
        Ok(_) => {
            // Link Android NDK camera and media libraries
            println!("cargo:rustc-link-lib=camera2ndk");
            println!("cargo:rustc-link-lib=mediandk");
            println!("cargo:rustc-link-lib=android");
            println!("cargo:warning=✓ NDK Camera2 桥接层编译完成，链接 camera2ndk + mediandk");
        }
        Err(e) => {
            panic!("NDK Camera2 bridge compilation failed: {}", e);
        }
    }
}
