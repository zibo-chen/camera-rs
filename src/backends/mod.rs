//! Backend abstraction layer.
//!
//! Provides a unified camera backend interface for multiple platform implementations:
//!
//! | Backend | Platform | Description |
//! |------|------|------|
//! | V4L2 | Linux | Native kernel camera capture (MMAP) |
//! | UVC | Linux, macOS, Android | USB camera support through libuvc |
//! | AVFoundation | macOS, iOS | Native Apple camera API |
//! | Camera2 | Android | Android framework camera API |
//! | Media Foundation | Windows | Windows media API |
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    medivh-camera API                        │
//! │  (CameraManager, StreamingCamera, CameraControl traits)     │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     backends module                        │
//! │  ┌─────────┐ ┌─────────────┐ ┌────────┐ ┌────────────────┐ │
//! │  │   UVC   │ │ AVFoundation│ │Camera2 │ │MediaFoundation │ │
//! │  │ (Linux, │ │   (macOS,   │ │(Android│ │   (Windows)    │ │
//! │  │ macOS,  │ │    iOS)     │ │  only) │ │                │ │
//! │  │Android) │ │             │ │        │ │                │ │
//! │  └─────────┘ └─────────────┘ └────────┘ └────────────────┘ │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ## Using a specific backend directly
//!
//! ## Using automatic backend selection

#[allow(unused_imports)]
use crate::CameraManager;
use crate::{CameraError, CameraResult};
#[allow(unused_imports)]
use std::sync::Arc;

// ==================== Backend modules ====================

// UVC backend: Linux, macOS, and Android through libusb/libuvc.
#[cfg(all(
    feature = "backend-uvc",
    any(target_os = "linux", target_os = "macos", target_os = "android")
))]
pub mod uvc;

// V4L2 supports Linux and explicitly enabled privileged Android access.
#[cfg(any(
    camera_v4l2,
    all(test, any(feature = "native", feature = "backend-v4l2"))
))]
pub mod v4l2;

// AVFoundation backend: macOS and iOS.
#[cfg(all(
    any(feature = "native", feature = "backend-avfoundation"),
    any(target_os = "macos", target_os = "ios")
))]
pub mod avfoundation;

// Camera2 backend: Android.
#[cfg(all(
    any(feature = "native", feature = "backend-camera2"),
    target_os = "android"
))]
pub mod camera2;

// Media Foundation backend: Windows.
#[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
pub mod mf;

// ==================== Backend type ====================

/// Camera backend type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendType {
    /// Automatically selects the preferred backend.
    Auto,
    /// UVC (USB Video Class) backend for cross-platform USB cameras.
    Uvc,
    /// Video4Linux2 - Linux kernel camera drivers
    V4l2,
    /// AVFoundation, the native Apple camera API.
    AVFoundation,
    /// Camera2, the native Android camera API.
    Camera2,
    /// Media Foundation, the native Windows media API.
    MediaFoundation,
}

impl BackendType {
    pub(crate) fn is_compiled(&self) -> bool {
        match self {
            Self::Auto => Self::default_for_platform().is_compiled(),
            Self::Uvc => cfg!(feature = "backend-uvc"),
            Self::V4l2 => cfg!(feature = "backend-v4l2"),
            Self::AVFoundation => cfg!(feature = "backend-avfoundation"),
            Self::Camera2 => cfg!(feature = "backend-camera2"),
            Self::MediaFoundation => cfg!(feature = "backend-mf"),
        }
    }

    pub(crate) fn supports_target(&self) -> bool {
        match self {
            Self::Auto => Self::default_for_platform().supports_target(),
            Self::Uvc => cfg!(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "android"
            )),
            Self::V4l2 => cfg!(any(target_os = "linux", target_os = "android")),
            Self::AVFoundation => cfg!(any(target_os = "macos", target_os = "ios")),
            Self::Camera2 => cfg!(target_os = "android"),
            Self::MediaFoundation => cfg!(target_os = "windows"),
        }
    }

    /// Returns the default backend for the current platform.
    pub fn default_for_platform() -> Self {
        if cfg!(all(
            target_os = "android",
            any(feature = "native", feature = "backend-camera2")
        )) {
            Self::Camera2
        } else if cfg!(all(
            any(target_os = "macos", target_os = "ios"),
            any(feature = "native", feature = "backend-avfoundation")
        )) {
            Self::AVFoundation
        } else if cfg!(all(
            target_os = "windows",
            any(feature = "native", feature = "backend-mf")
        )) {
            Self::MediaFoundation
        } else if cfg!(camera_v4l2) {
            Self::V4l2
        } else {
            Self::Uvc
        }
    }

    /// Checks whether this backend is available on the current platform.
    pub fn is_available(&self) -> bool {
        match self {
            BackendType::Auto => Self::default_for_platform().is_available(),
            BackendType::Uvc => cfg!(all(
                feature = "backend-uvc",
                any(
                    target_os = "linux",
                    target_os = "macos",
                    target_os = "android"
                )
            )),
            BackendType::V4l2 => cfg!(camera_v4l2),
            BackendType::AVFoundation => {
                cfg!(all(
                    any(feature = "native", feature = "backend-avfoundation"),
                    any(target_os = "macos", target_os = "ios")
                ))
            }
            BackendType::Camera2 => {
                cfg!(all(
                    any(feature = "native", feature = "backend-camera2"),
                    target_os = "android"
                ))
            }
            BackendType::MediaFoundation => {
                cfg!(all(
                    any(feature = "native", feature = "backend-mf"),
                    target_os = "windows"
                ))
            }
        }
    }

    /// Returns the human-readable backend name.
    pub fn display_name(&self) -> &'static str {
        match self {
            BackendType::Auto => "Auto",
            BackendType::Uvc => "UVC (libuvc)",
            BackendType::V4l2 => "Linux V4L2",
            BackendType::AVFoundation => "AVFoundation",
            BackendType::Camera2 => "Android Camera2",
            BackendType::MediaFoundation => "Windows Media Foundation",
        }
    }
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

impl From<BackendType> for crate::BackendId {
    fn from(value: BackendType) -> Self {
        match value {
            BackendType::Auto => Self::from_builtin("platform-default"),
            BackendType::Uvc => Self::UVC,
            BackendType::V4l2 => Self::V4L2,
            BackendType::AVFoundation => Self::AV_FOUNDATION,
            BackendType::Camera2 => Self::CAMERA2,
            BackendType::MediaFoundation => Self::MEDIA_FOUNDATION,
        }
    }
}

pub(crate) fn backend_type(id: &crate::BackendId) -> Option<BackendType> {
    match id.as_str() {
        "uvc" => Some(BackendType::Uvc),
        "v4l2" => Some(BackendType::V4l2),
        "avfoundation" => Some(BackendType::AVFoundation),
        "camera2" => Some(BackendType::Camera2),
        "media-foundation" => Some(BackendType::MediaFoundation),
        _ => None,
    }
}

// ==================== Backend factory functions ====================

/// Returns all backend types available on the current platform.
pub fn available_backends() -> Vec<BackendType> {
    vec![
        #[cfg(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ))]
        BackendType::Uvc,
        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        ))]
        BackendType::AVFoundation,
        #[cfg(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        ))]
        BackendType::Camera2,
        #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
        BackendType::MediaFoundation,
        #[cfg(camera_v4l2)]
        BackendType::V4l2,
    ]
}

// ==================== Camera type wrapper ====================

/// Unified camera type.
///
/// Because `StreamingCamera` contains async methods, it cannot be used directly
/// as a `dyn` trait object. This enum provides type-safe dispatch across backend
/// camera implementations.
///
/// # Usage
#[non_exhaustive]
#[derive(Clone)]
pub enum BackendCamera {
    /// Linux V4L2 camera
    #[cfg(camera_v4l2)]
    V4l2(Arc<v4l2::V4l2Camera>),

    /// UVC camera (USB Video Class).
    #[cfg(all(
        feature = "backend-uvc",
        any(target_os = "linux", target_os = "macos", target_os = "android")
    ))]
    Uvc(Arc<uvc::UvcCamera>),

    /// AVFoundation camera (macOS/iOS).
    #[cfg(all(
        any(feature = "native", feature = "backend-avfoundation"),
        any(target_os = "macos", target_os = "ios")
    ))]
    AVFoundation(Arc<avfoundation::AVFoundationCamera>),

    /// Camera2 camera (Android).
    #[cfg(all(
        any(feature = "native", feature = "backend-camera2"),
        target_os = "android"
    ))]
    Camera2(Arc<camera2::Camera2Camera>),

    /// Media Foundation camera (Windows).
    #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
    MediaFoundation(Arc<mf::MFCamera>),
}

fn no_camera_backend_enabled() -> CameraError {
    backend_unavailable(BackendType::default_for_platform())
}

fn backend_unavailable(backend: BackendType) -> CameraError {
    let id = backend.into();
    if backend.supports_target() {
        CameraError::backend_not_compiled(id)
    } else {
        CameraError::unsupported_target(id, std::env::consts::OS)
    }
}

impl BackendCamera {
    pub fn frame_hub(&self) -> Option<&crate::FrameHub> {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.frame_hub(),
            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.frame_hub(),
            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.frame_hub(),
            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.frame_hub(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.frame_hub(),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    pub fn get_shared_frame(&self) -> CameraResult<Option<std::sync::Arc<crate::CapturedFrame>>> {
        self.frame_hub()
            .map(|hub| hub.latest())
            .ok_or_else(no_camera_backend_enabled)
    }

    pub async fn wait_for_shared_frame(
        &self,
        after: Option<crate::FrameKey>,
        timeout: std::time::Duration,
    ) -> CameraResult<std::sync::Arc<crate::CapturedFrame>> {
        self.frame_hub()
            .ok_or_else(no_camera_backend_enabled)?
            .wait_after(after, timeout)
            .await
    }

    /// Returns the backend type.
    pub fn backend_type(&self) -> BackendType {
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(_) => BackendType::Uvc,

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(_) => BackendType::AVFoundation,

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(_) => BackendType::Camera2,

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(_) => BackendType::MediaFoundation,

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(_) => BackendType::V4l2,

            #[allow(unreachable_patterns)]
            _ => BackendType::Auto,
        }
    }

    pub(crate) fn opened_device_capabilities(&self) -> CameraResult<crate::DeviceCapabilities> {
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(camera) => camera.opened_device_capabilities(),
            #[allow(unreachable_patterns)]
            _ => Err(CameraError::invalid_state(
                "Opened-device capability queries are only required for descriptor-backed UVC"
                    .into(),
            )),
        }
    }

    // ==================== StreamingCamera interface ====================

    /// Starts video streaming.
    pub async fn start_stream(&self, config: crate::CameraConfig) -> CameraResult<()> {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.start_stream_arc(config).await,

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.start_stream_arc(config).await,

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.start_stream_arc(config).await,

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.start_stream_arc(config).await,

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.start_stream_arc(config).await,

            #[allow(unreachable_patterns)]
            _ => {
                let _ = config;
                Err(no_camera_backend_enabled())
            }
        }
    }

    /// Stops video streaming.
    pub async fn stop_stream(&self) -> CameraResult<()> {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.stop_stream_arc().await,

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.stop_stream_arc().await,

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.stop_stream_arc().await,

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.stop_stream_arc().await,

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.stop_stream_arc().await,

            #[allow(unreachable_patterns)]
            _ => Err(no_camera_backend_enabled()),
        }
    }

    /// Returns the latest frame.
    pub fn get_latest_frame(&self) -> CameraResult<Option<crate::pixels::Pixels<u8>>> {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.get_latest_frame(),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.get_latest_frame(),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.get_latest_frame(),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.get_latest_frame(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.get_latest_frame(),

            #[allow(unreachable_patterns)]
            _ => Err(no_camera_backend_enabled()),
        }
    }

    /// Waits for and returns the next frame.
    pub async fn wait_for_frame(
        &self,
        timeout: std::time::Duration,
    ) -> CameraResult<crate::pixels::Pixels<u8>> {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.wait_for_frame(timeout).await,

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.wait_for_frame(timeout).await,

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.wait_for_frame(timeout).await,

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.wait_for_frame(timeout).await,

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.wait_for_frame(timeout).await,

            #[allow(unreachable_patterns)]
            _ => {
                let _ = timeout;
                Err(no_camera_backend_enabled())
            }
        }
    }

    /// Returns whether the stream is running.
    pub fn is_streaming(&self) -> bool {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.is_streaming(),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.is_streaming(),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.is_streaming(),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.is_streaming(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.is_streaming(),

            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// Returns the current configuration.
    pub fn get_config(&self) -> Option<crate::CameraConfig> {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.get_config(),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.get_config(),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.get_config(),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.get_config(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.get_config(),

            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Returns stream statistics.
    pub fn get_stats(&self) -> crate::StreamStats {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.get_stats(),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.get_stats(),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.get_stats(),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.get_stats(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.get_stats(),

            #[allow(unreachable_patterns)]
            _ => crate::StreamStats::default(),
        }
    }

    /// Sets the buffer size.
    pub async fn set_buffer_size(&self, size: usize) -> CameraResult<()> {
        #[allow(unused_imports)]
        use crate::StreamingCamera;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.set_buffer_size(size).await,

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.set_buffer_size(size).await,

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.set_buffer_size(size).await,

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.set_buffer_size(size).await,

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.set_buffer_size(size).await,

            #[allow(unreachable_patterns)]
            _ => {
                let _ = size;
                Err(no_camera_backend_enabled())
            }
        }
    }

    pub fn set_camera2_options(&self, options: crate::Camera2Options) -> CameraResult<()> {
        match self {
            #[cfg(all(feature = "backend-camera2", target_os = "android"))]
            BackendCamera::Camera2(camera) => camera.set_options(options),
            #[allow(unreachable_patterns)]
            _ => {
                let _ = options;
                Err(CameraError::unsupported_target(
                    crate::BackendId::CAMERA2,
                    std::env::consts::OS,
                ))
            }
        }
    }

    pub fn set_avfoundation_options(
        &self,
        options: crate::AvFoundationOptions,
    ) -> CameraResult<()> {
        match self {
            #[cfg(all(
                feature = "backend-avfoundation",
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(camera) => camera.set_options(options),
            #[allow(unreachable_patterns)]
            _ => {
                let _ = options;
                Err(CameraError::unsupported_target(
                    crate::BackendId::AV_FOUNDATION,
                    std::env::consts::OS,
                ))
            }
        }
    }

    /// Releases backend resources.
    pub fn cleanup(&self) {
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.cleanup(),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.cleanup(),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.cleanup(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.cleanup(),

            // Other backends may not require an explicit cleanup method.
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    // ==================== CameraControl interface ====================

    /// Returns the current value of the specified control.
    pub fn get_control(
        &self,
        control: crate::CameraControlType,
    ) -> CameraResult<crate::CameraControlValue> {
        #[allow(unused_imports)]
        use crate::CameraControl;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.get_control(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.get_control(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.get_control(control),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.get_control(control),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.get_control(control),

            #[allow(unreachable_patterns)]
            _ => {
                let _ = control;
                Err(no_camera_backend_enabled())
            }
        }
    }

    /// Sets the specified control value.
    pub fn set_control(
        &self,
        control: crate::CameraControlType,
        value: crate::CameraControlValue,
    ) -> CameraResult<()> {
        #[allow(unused_imports)]
        use crate::CameraControl;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.set_control(control, value),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.set_control(control, value),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.set_control(control, value),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.set_control(control, value),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.set_control(control, value),

            #[allow(unreachable_patterns)]
            _ => {
                let _ = (control, value);
                Err(no_camera_backend_enabled())
            }
        }
    }

    /// Returns the valid range for the specified control.
    pub fn get_control_range(
        &self,
        control: crate::CameraControlType,
    ) -> CameraResult<crate::CameraControlRange> {
        #[allow(unused_imports)]
        use crate::CameraControl;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.get_control_range(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.get_control_range(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.get_control_range(control),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.get_control_range(control),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.get_control_range(control),

            #[allow(unreachable_patterns)]
            _ => {
                let _ = control;
                Err(no_camera_backend_enabled())
            }
        }
    }

    /// Checks whether the device supports the specified control.
    pub fn supports_control(&self, control: crate::CameraControlType) -> bool {
        #[allow(unused_imports)]
        use crate::CameraControl;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.supports_control(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.supports_control(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.supports_control(control),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.supports_control(control),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.supports_control(control),

            #[allow(unreachable_patterns)]
            _ => {
                let _ = control;
                false
            }
        }
    }

    /// Returns all controls supported by the device.
    pub fn get_supported_controls(&self) -> Vec<crate::CameraControlType> {
        #[allow(unused_imports)]
        use crate::CameraControl;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.get_supported_controls(),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.get_supported_controls(),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.get_supported_controls(),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.get_supported_controls(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.get_supported_controls(),

            #[allow(unreachable_patterns)]
            _ => Vec::new(),
        }
    }

    /// Resets the specified control to its default value.
    pub fn reset_control(&self, control: crate::CameraControlType) -> CameraResult<()> {
        #[allow(unused_imports)]
        use crate::CameraControl;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.reset_control(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.reset_control(control),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.reset_control(control),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.reset_control(control),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.reset_control(control),

            #[allow(unreachable_patterns)]
            _ => {
                let _ = control;
                Err(no_camera_backend_enabled())
            }
        }
    }

    /// Resets all controls to their default values.
    pub fn reset_all_controls(&self) -> CameraResult<()> {
        #[allow(unused_imports)]
        use crate::CameraControl;
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(cam) => cam.reset_all_controls(),

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ))]
            BackendCamera::AVFoundation(cam) => cam.reset_all_controls(),

            #[cfg(all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ))]
            BackendCamera::Camera2(cam) => cam.reset_all_controls(),

            #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
            BackendCamera::MediaFoundation(cam) => cam.reset_all_controls(),

            #[cfg(camera_v4l2)]
            BackendCamera::V4l2(cam) => cam.reset_all_controls(),

            #[allow(unreachable_patterns)]
            _ => Err(no_camera_backend_enabled()),
        }
    }
}

/// Creates a camera instance with the specified backend.
///
/// # Arguments
///
/// * `device_index` - Device index.
/// * `backend` - Backend type; use `BackendType::Auto` for automatic selection.
///
/// # Returns
///
/// Returns a `BackendCamera` wrapping the concrete backend implementation.
///
/// # Fallback strategy
///
/// If the requested backend is unavailable on the current platform, this falls
/// back to the platform default instead of returning an error.
pub fn create_camera(_device_index: u32, backend: BackendType) -> CameraResult<BackendCamera> {
    create_camera_with_hub(_device_index, backend, crate::FrameHub::default())
}
pub(crate) fn create_camera_with_hub(
    _device_index: u32,
    backend: BackendType,
    _hub: crate::FrameHub,
) -> CameraResult<BackendCamera> {
    create_selected_camera(_device_index, backend, _hub, None)
}
pub(crate) fn create_selected_camera(
    _device_index: u32,
    backend: BackendType,
    _hub: crate::FrameHub,
    _expected: Option<&str>,
) -> CameraResult<BackendCamera> {
    // Use the platform default for Auto or an unavailable backend.
    let effective_backend = if backend == BackendType::Auto {
        let fallback = BackendType::default_for_platform();
        if backend != BackendType::Auto && !backend.is_available() {
            log::warn!(
                "Backend {:?} is not available on this platform, falling back to {:?}",
                backend,
                fallback
            );
        }
        fallback
    } else {
        backend
    };

    match effective_backend {
        BackendType::Auto => {
            // All available variants are handled above.
            unreachable!("Auto backend should be resolved to specific backend")
        }

        #[cfg(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ))]
        BackendType::Uvc => {
            let camera = uvc::UvcCamera::new_selected(_device_index, _hub, _expected)?;
            let arc = Arc::new(camera);
            arc.set_self_ref();
            Ok(BackendCamera::Uvc(arc))
        }

        #[cfg(not(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        )))]
        BackendType::Uvc => Err(backend_unavailable(BackendType::Uvc)),

        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        ))]
        BackendType::AVFoundation => {
            let camera =
                avfoundation::AVFoundationCamera::new_selected(_device_index, _hub, _expected)?;
            Ok(BackendCamera::AVFoundation(Arc::new(camera)))
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        )))]
        BackendType::AVFoundation => Err(backend_unavailable(BackendType::AVFoundation)),

        #[cfg(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        ))]
        BackendType::Camera2 => {
            let camera = camera2::Camera2Camera::new_selected(_device_index, _hub, _expected)?;
            Ok(BackendCamera::Camera2(Arc::new(camera)))
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        )))]
        BackendType::Camera2 => Err(backend_unavailable(BackendType::Camera2)),

        #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
        BackendType::MediaFoundation => {
            let camera = mf::MFCamera::new_selected(_device_index, _hub, _expected)?;
            Ok(BackendCamera::MediaFoundation(Arc::new(camera)))
        }

        #[cfg(not(all(any(feature = "native", feature = "backend-mf"), target_os = "windows")))]
        BackendType::MediaFoundation => Err(backend_unavailable(BackendType::MediaFoundation)),

        #[cfg(camera_v4l2)]
        BackendType::V4l2 => {
            let camera = if let Some(path) = _expected {
                v4l2::V4l2Camera::from_path_with_hub(path, _hub)?
            } else {
                v4l2::V4l2Camera::new_with_hub(_device_index, _hub)?
            };
            Ok(BackendCamera::V4l2(Arc::new(camera)))
        }
        #[cfg(not(camera_v4l2))]
        BackendType::V4l2 => Err(backend_unavailable(BackendType::V4l2)),
    }
}

/// Creates a UVC camera from a file descriptor on Android.
///
/// Android applications cannot access USB device nodes directly. Obtain an FD
/// through the Java layer and pass it to this function.
///
/// # Arguments
///
/// * `fd` - File descriptor returned by `UsbDeviceConnection.getFileDescriptor()`.
///
/// # Returns
///
/// Returns the `BackendCamera::Uvc` variant.
#[cfg(all(
    all(
        feature = "backend-uvc",
        any(target_os = "linux", target_os = "macos", target_os = "android")
    ),
    unix
))]
pub fn create_camera_from_fd(fd: i32) -> CameraResult<BackendCamera> {
    log::info!("Creating UVC camera from fd={}", fd);
    let camera = uvc::UvcCamera::from_fd(fd)?;
    let arc = Arc::new(camera);
    arc.set_self_ref();
    Ok(BackendCamera::Uvc(arc))
}

/// Lists all available devices through the specified backend.
///
/// # Arguments
///
/// * `backend` - Backend type; use `BackendType::Auto` for the platform default.
///
/// # Returns
///
/// Returns the device list.
///
/// # Fallback strategy
///
/// If the requested backend is unavailable on the current platform, this falls
/// back to the platform default instead of returning an error.
pub fn list_devices(backend: BackendType) -> CameraResult<Vec<crate::CameraDeviceInfo>> {
    // Use the platform default for Auto or an unavailable backend.
    let effective_backend = if backend == BackendType::Auto {
        let fallback = BackendType::default_for_platform();
        if backend != BackendType::Auto && !backend.is_available() {
            log::warn!(
                "Backend {:?} is not available on this platform, falling back to {:?}",
                backend,
                fallback
            );
        }
        fallback
    } else {
        backend
    };

    match effective_backend {
        BackendType::Auto => {
            unreachable!("Auto backend should be resolved to specific backend")
        }

        #[cfg(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ))]
        BackendType::Uvc => <uvc::UvcCamera as CameraManager>::list_devices(),

        #[cfg(not(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        )))]
        BackendType::Uvc => Err(backend_unavailable(BackendType::Uvc)),

        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        ))]
        BackendType::AVFoundation => {
            <avfoundation::AVFoundationCamera as CameraManager>::list_devices()
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        )))]
        BackendType::AVFoundation => Err(backend_unavailable(BackendType::AVFoundation)),

        #[cfg(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        ))]
        BackendType::Camera2 => <camera2::Camera2Camera as CameraManager>::list_devices(),

        #[cfg(not(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        )))]
        BackendType::Camera2 => Err(backend_unavailable(BackendType::Camera2)),

        #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
        BackendType::MediaFoundation => <mf::MFCamera as CameraManager>::list_devices(),

        #[cfg(not(all(any(feature = "native", feature = "backend-mf"), target_os = "windows")))]
        BackendType::MediaFoundation => Err(backend_unavailable(BackendType::MediaFoundation)),

        #[cfg(camera_v4l2)]
        BackendType::V4l2 => v4l2::V4l2Camera::list_devices(),
        #[cfg(not(camera_v4l2))]
        BackendType::V4l2 => Err(backend_unavailable(BackendType::V4l2)),
    }
}

/// Returns the video configurations supported by a device through a backend.
///
/// `BackendType::Auto` resolves to the current platform default. An unavailable
/// backend follows [`list_devices`] and falls back to that default.
pub fn get_supported_configs(
    backend: BackendType,
    _device_index: u32,
) -> CameraResult<Vec<crate::CameraConfig>> {
    let effective_backend = if backend == BackendType::Auto {
        let fallback = BackendType::default_for_platform();
        if backend != BackendType::Auto && !backend.is_available() {
            log::warn!(
                "Backend {:?} is not available on this platform, falling back to {:?}",
                backend,
                fallback
            );
        }
        fallback
    } else {
        backend
    };

    match effective_backend {
        BackendType::Auto => {
            unreachable!("Auto backend should be resolved to specific backend")
        }

        #[cfg(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ))]
        BackendType::Uvc => <uvc::UvcCamera as CameraManager>::get_supported_configs(_device_index),

        #[cfg(not(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        )))]
        BackendType::Uvc => Err(backend_unavailable(BackendType::Uvc)),

        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        ))]
        BackendType::AVFoundation => {
            <avfoundation::AVFoundationCamera as CameraManager>::get_supported_configs(
                _device_index,
            )
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        )))]
        BackendType::AVFoundation => Err(backend_unavailable(BackendType::AVFoundation)),

        #[cfg(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        ))]
        BackendType::Camera2 => {
            <camera2::Camera2Camera as CameraManager>::get_supported_configs(_device_index)
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        )))]
        BackendType::Camera2 => Err(backend_unavailable(BackendType::Camera2)),

        #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
        BackendType::MediaFoundation => {
            <mf::MFCamera as CameraManager>::get_supported_configs(_device_index)
        }

        #[cfg(not(all(any(feature = "native", feature = "backend-mf"), target_os = "windows")))]
        BackendType::MediaFoundation => Err(backend_unavailable(BackendType::MediaFoundation)),

        #[cfg(camera_v4l2)]
        BackendType::V4l2 => v4l2::V4l2Camera::get_supported_configs(_device_index),
        #[cfg(not(camera_v4l2))]
        BackendType::V4l2 => Err(backend_unavailable(BackendType::V4l2)),
    }
}

pub(crate) fn get_device_capabilities(
    backend: BackendType,
    device_index: u32,
) -> CameraResult<crate::DeviceCapabilities> {
    #[cfg(camera_v4l2)]
    if backend == BackendType::V4l2 {
        return v4l2::V4l2Camera::device_capabilities(device_index);
    }
    Ok(crate::DeviceCapabilities::from_configurations(
        get_supported_configs(backend, device_index)?,
    ))
}

pub(crate) fn requested_configs(
    backend: BackendType,
    device_index: u32,
    request: &crate::CaptureRequest,
    advertised: &[crate::CameraConfig],
) -> CameraResult<Vec<crate::CameraConfig>> {
    #[cfg(camera_v4l2)]
    if backend == BackendType::V4l2 {
        return v4l2::V4l2Camera::requested_configs(device_index, request, advertised);
    }
    let _ = (backend, device_index, request, advertised);
    Ok(Vec::new())
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_type_default() {
        let default = BackendType::default_for_platform();
        assert_ne!(default, BackendType::Auto);
        if default.is_available() {
            assert!(available_backends().contains(&default));
        }
    }

    #[test]
    fn test_available_backends() {
        // At least one backend should be available when UVC is enabled by default.
        #[cfg(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ))]
        assert!(!available_backends().is_empty());

        #[cfg(not(any(
            all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ),
            all(
                any(feature = "native", feature = "backend-avfoundation"),
                any(target_os = "macos", target_os = "ios")
            ),
            all(
                any(feature = "native", feature = "backend-camera2"),
                target_os = "android"
            ),
            all(any(feature = "native", feature = "backend-mf"), target_os = "windows"),
            camera_v4l2
        )))]
        assert!(available_backends().is_empty());
    }

    #[test]
    fn test_backend_display() {
        assert_eq!(BackendType::Uvc.display_name(), "UVC (libuvc)");
        assert_eq!(BackendType::Auto.to_string(), "Auto");
    }

    #[test]
    #[cfg(all(
        any(target_os = "macos", target_os = "ios"),
        not(any(feature = "native", feature = "backend-avfoundation"))
    ))]
    fn get_supported_configs_reports_missing_apple_default_backend() {
        assert!(get_supported_configs(BackendType::AVFoundation, 0).is_err());
    }

    #[test]
    #[cfg(not(any(
        all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ),
        all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        ),
        all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        ),
        all(any(feature = "native", feature = "backend-mf"), target_os = "windows"),
        camera_v4l2
    )))]
    fn no_backend_build_returns_clear_factory_errors() {
        let create_err = match create_camera(0, BackendType::Auto) {
            Ok(_) => panic!("camera creation should fail when no backend is enabled"),
            Err(error) => error,
        };
        let list_err = list_devices(BackendType::Auto)
            .expect_err("device listing should fail when no backend is enabled");
        let config_err = get_supported_configs(BackendType::Auto, 0)
            .expect_err("config listing should fail when no backend is enabled");

        for error in [create_err, list_err, config_err] {
            assert!(
                matches!(
                    error.kind(),
                    crate::CameraErrorKind::BackendNotCompiled
                        | crate::CameraErrorKind::UnsupportedTarget
                ),
                "unexpected no-backend error: {error}"
            );
        }
    }

    #[test]
    #[cfg(all(
        target_os = "android",
        not(any(feature = "native", feature = "backend-camera2"))
    ))]
    fn get_supported_configs_reports_missing_android_default_backend() {
        let err = get_supported_configs(BackendType::Auto, 0)
            .expect_err("missing default backend feature should be reported");

        assert!(err
            .to_string()
            .contains("Camera2 backend is not available on this platform"));
    }

    #[test]
    #[cfg(all(
        target_os = "windows",
        not(any(feature = "native", feature = "backend-mf"))
    ))]
    fn get_supported_configs_reports_missing_windows_default_backend() {
        let err = get_supported_configs(BackendType::Auto, 0)
            .expect_err("missing default backend feature should be reported");

        assert!(err
            .to_string()
            .contains("Media Foundation backend is not available on this platform"));
    }
}
