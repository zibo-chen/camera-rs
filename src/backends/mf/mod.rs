//! Windows Media Foundation camera backend.
//!
//! Provides native camera support on Windows.
//!
//! # Platform support
//!
//! - Windows Vista and newer (Windows 10 or newer recommended).
//!
//! # Features
//!
//! Compared with DirectShow/UVC, Media Foundation provides:
//!
//! - A modern Windows media API.
//! - Better hardware acceleration support.
//! - Windows Hello camera support.
//! - Windows camera permission integration on Windows 10 and newer.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      MFCamera (Rust)                        │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                   Media Foundation API                       │
//! │  ┌────────────────┐ ┌──────────────┐ ┌───────────────────┐ │
//! │  │IMFMediaSource  │ │IMFSourceReader│ │IMFMediaType      │ │
//! │  └────────────────┘ └──────────────┘ └───────────────────┘ │
//! └─────────────────────────────────────────────────────────────┘
//! ```

#[cfg(all(target_os = "windows", any(feature = "native", feature = "backend-mf")))]
mod camera;
#[cfg(all(target_os = "windows", any(feature = "native", feature = "backend-mf")))]
mod com;
#[cfg(all(target_os = "windows", any(feature = "native", feature = "backend-mf")))]
mod control;
#[cfg(all(target_os = "windows", any(feature = "native", feature = "backend-mf")))]
mod convert;
#[cfg(all(target_os = "windows", any(feature = "native", feature = "backend-mf")))]
mod device;

#[cfg(all(target_os = "windows", any(feature = "native", feature = "backend-mf")))]
pub use camera::MFCamera;

#[cfg(all(target_os = "windows", any(feature = "native", feature = "backend-mf")))]
fn native_error(
    kind: crate::CameraErrorKind,
    stage: crate::OperationStage,
    operation: &'static str,
    error: windows::core::Error,
) -> crate::CameraError {
    crate::CameraError::native(
        kind,
        crate::BackendId::MEDIA_FOUNDATION,
        stage,
        operation.into(),
        i64::from(error.code().0),
        error.to_string(),
    )
    .with_source(error)
}
// Stub implementation for non-Windows platforms.
#[cfg(not(all(target_os = "windows", any(feature = "native", feature = "backend-mf"))))]
mod camera_stub;
#[cfg(not(all(target_os = "windows", any(feature = "native", feature = "backend-mf"))))]
pub use camera_stub::MFCamera;
