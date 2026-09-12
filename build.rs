use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    println!("cargo:rustc-check-cfg=cfg(camera_v4l2)");
    let v4l2_enabled = matches!(target_os.as_str(), "linux" | "android")
        && env::var_os("CARGO_FEATURE_BACKEND_V4L2").is_some();
    if v4l2_enabled {
        println!("cargo:rustc-cfg=camera_v4l2");
    }

    // Detect whether the UVC backend feature is enabled.
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

    // Android requires the system log library.
    if target_os == "android" {
        println!("cargo:rustc-link-lib=log");
    }

    if backend_uvc_enabled && target_os == "macos" {
        println!("cargo:rustc-link-lib=framework=IOKit");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Security");
    }
    let apple_enabled = env::var_os("CARGO_FEATURE_BACKEND_AVFOUNDATION").is_some();
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

    // Compile the native UVC libraries only when the backend is enabled.
    if backend_uvc_enabled {
        compile_libusb(&thirdparty_dir, &target_os);
        compile_libuvc(&thirdparty_dir, &target_os);
        println!("cargo:warning=UVC native libraries compiled");
    } else {
        println!("cargo:warning=Skipping UVC native libraries (backend-uvc is disabled)");
    }

    // Compile the NDK Camera2 bridge only for an enabled Android backend.
    let backend_camera2_enabled = env::var("CARGO_FEATURE_BACKEND_CAMERA2").is_ok();
    if backend_camera2_enabled && target_os == "android" {
        compile_ndk_camera2_bridge(&manifest_dir);
        println!("cargo:warning=Android NDK Camera2 bridge compiled");
    } else if backend_camera2_enabled {
        println!("cargo:warning=Skipping the NDK Camera2 bridge on this target");
    }
}

fn compile_libusb(thirdparty_dir: &Path, target_os: &str) {
    let libusb_dir = thirdparty_dir.join("libusb/libusb");

    if !libusb_dir.exists() {
        panic!("backend-uvc requires bundled libusb sources; reinstall the source package");
    }

    println!("cargo:warning=Compiling bundled libusb");

    // Generate the platform-specific config.h.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    generate_libusb_config(&out_dir, target_os);

    let mut build = cc::Build::new();

    // Core sources.
    build
        .file(libusb_dir.join("core.c"))
        .file(libusb_dir.join("descriptor.c"))
        .file(libusb_dir.join("hotplug.c"))
        .file(libusb_dir.join("io.c"))
        .file(libusb_dir.join("strerror.c"))
        .file(libusb_dir.join("sync.c"));

    // Platform-specific sources.
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
        println!("cargo:warning=Bundled libusb UVC access is unsupported on Windows");
        println!("cargo:warning=Use the Media Foundation backend on Windows");
        return;
    }

    // Include directories, with the generated config.h taking precedence.
    build
        .include(&out_dir) // Directory containing the generated config.h.
        .include(&libusb_dir)
        .include(thirdparty_dir.join("libusb"))
        .include(libusb_dir.join("os"));

    // Platform-specific configuration.
    if target_os == "android" {
        let android_config = thirdparty_dir.join("libusb/android");
        if android_config.exists() {
            build.include(android_config);
        }
    }

    // Compiler definitions.
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

    // Compiler options.
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
        r#"/* libusb config.h for Linux and Android. */
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
#define PACKAGE_STRING "libusb-1.0 1.0.29"
#define PACKAGE_TARNAME "libusb-1.0"
#define PACKAGE_URL "http://libusb.info"
#define PACKAGE_VERSION "1.0.29"
#define VERSION "1.0.29"

#define POLL_NFDS_TYPE nfds_t
#define STDC_HEADERS 1
#define USE_SYSTEM_LOGGING_FACILITY 1
#define _GNU_SOURCE 1

#endif /* LIBUSB_CONFIG_H */
"#
    } else if target_os == "macos" {
        r#"/* libusb config.h for macOS. */
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
#define PACKAGE_STRING "libusb-1.0 1.0.29"
#define PACKAGE_TARNAME "libusb-1.0"
#define PACKAGE_URL "http://libusb.info"
#define PACKAGE_VERSION "1.0.29"
#define VERSION "1.0.29"

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

    println!(
        "cargo:warning=Generated libusb config: {}",
        config_path.display()
    );
}

fn compile_libuvc(thirdparty_dir: &Path, target_os: &str) {
    let libuvc_dir = thirdparty_dir.join("libuvc");
    let src_dir = libuvc_dir.join("src");

    if !src_dir.exists() {
        panic!("backend-uvc requires vendored libuvc sources");
    }

    println!("cargo:warning=Compiling bundled libuvc");

    let mut build = cc::Build::new();

    // All source files.
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

    // libuvc always transports native MJPEG. The independent decode feature
    // controls whether camera-rs can convert it to RGB.
    if env::var_os("CARGO_FEATURE_DECODE_MJPEG").is_some() {
        println!("cargo:warning=MJPEG transport and TurboJPEG decoding enabled");
    } else {
        println!("cargo:warning=MJPEG transport enabled without RGB decoding");
    }

    // Include directories.
    build
        .include(libuvc_dir.join("include"))
        .include(libuvc_dir.join("include/libuvc"))
        .include(thirdparty_dir.join("libusb/libusb"));

    if env::var_os("CARGO_FEATURE_NATIVE_DEBUG_LOGS").is_some() {
        build.define("CAMERA_NATIVE_DEBUG_LOGS", "1");
    }

    // libuvc version definitions.
    build
        .define("LIBUVC_VERSION_MAJOR", "0")
        .define("LIBUVC_VERSION_MINOR", "0")
        .define("LIBUVC_VERSION_PATCH", "7")
        .define("LIBUVC_VERSION_STR", "\"0.0.7\"")
        .define("LIBUVC_VERSION_INT", "0x000007");

    if target_os == "android" {
        build.define("__ANDROID__", "1");
    }

    // Compiler options.
    build
        .flag_if_supported("-fPIC")
        .flag_if_supported("-std=gnu11")
        .flag_if_supported("-Wno-everything")
        .warnings(false);

    build.try_compile("uvc").unwrap_or_else(|e| {
        panic!("libuvc compilation failed: {}", e);
    });

    println!("cargo:warning=Bundled libuvc compiled");
}

fn compile_ndk_camera2_bridge(manifest_dir: &Path) {
    let cpp_dir = manifest_dir.join("cpp");
    let source_file = cpp_dir.join("ndk_camera2_bridge.cpp");

    if !source_file.exists() {
        panic!("backend-camera2 requires cpp/ndk_camera2_bridge.cpp");
    }

    println!("cargo:warning=Compiling the Android NDK Camera2 bridge");

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
            println!(
                "cargo:warning=Android NDK Camera2 bridge linked with camera2ndk and mediandk"
            );
        }
        Err(e) => {
            panic!("NDK Camera2 bridge compilation failed: {}", e);
        }
    }
}
