//! Android Camera2 NDK backend
//!
//! This module provides native Android camera support using the NDK Camera2 API
//! (ACameraManager, ACameraDevice, ACameraCaptureSession, AImageReader).
//!
//! # Architecture
//!
//! ```text
//! +----------------------------------------------+
//! |            Camera2Camera (Rust)               |
//! |  (CameraManager, StreamingCamera, Control)    |
//! +----------------------------------------------+
//!                      | FFI (extern "C")
//!                      v
//! +----------------------------------------------+
//! |        ndk_camera2_bridge.cpp (C++)           |
//! |  ACameraManager -> ACameraDevice ->           |
//! |  ACameraCaptureSession + AImageReader         |
//! +----------------------------------------------+
//!                      |
//!                      v
//! +----------------------------------------------+
//! |        Android NDK Camera2 / Media            |
//! |  (libcamera2ndk.so, libmediandk.so)           |
//! +----------------------------------------------+
//! ```
//!
//! # Performance
//!
//! - Uses `AImageReader_acquireLatestImage` to skip stale frames (low latency)
//! - YUV->RGB conversion in Rust with SIMD-friendly layout
//! - Single-frame ring buffer to minimize memory allocation
//! - Frame callback runs on AImageReader's internal thread, conversion on Rust side
//!
//! # Platform Support
//!
//! - Android 7.0 (API Level 24) and above (NDK Camera2 API minimum)

mod camera;
mod ffi;

pub use camera::Camera2Camera;
