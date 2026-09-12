//! Async cross-platform camera capture with owned sessions and immutable frames.
#![warn(missing_docs)]
//!
//! ```no_run
//! use camera::{CameraSystem, CaptureProfile, DeviceSelector};
//! # async fn capture()->camera::CameraResult<()> {
//! let system=CameraSystem::new();
//! let mut capture=system.capture(DeviceSelector::Default)
//!     .profile(CaptureProfile::Preview)
//!     .resolution(1280,720)
//!     .start().await?;
//! let frame=capture.next_frame().await?;
//! println!("{} native bytes, {:?}",frame.bytes().len(),frame.layout());
//! capture.close().await?;
//! # Ok(()) }
//! ```
#[cfg(feature = "runtime-tokio")]
mod api;
#[cfg(feature = "runtime-tokio")]
#[allow(dead_code)]
mod backends;
#[cfg(feature = "blocking")]
pub mod blocking;
mod contract;
#[cfg(feature = "runtime-tokio")]
mod controls;
#[cfg(feature = "convert-rgb")]
mod conversion;
mod error;
#[cfg(feature = "runtime-tokio")]
#[cfg_attr(not(feature = "custom-backend"), allow(dead_code))]
mod extension;
mod format;
#[cfg(feature = "runtime-tokio")]
mod frame;
#[cfg(feature = "decode-mjpeg")]
mod mjpeg;
#[cfg(all(
    unix,
    any(
        all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ),
        test
    )
))]
mod owned_fd;
#[allow(dead_code)]
mod pixels;
#[cfg(feature = "runtime-tokio")]
#[allow(dead_code)]
mod traits;
// Native adapters keep a small internal compatibility vocabulary.
#[allow(dead_code)]
mod types;
#[allow(dead_code)]
mod utils;

#[cfg(feature = "runtime-tokio")]
pub use api::{
    CameraSystem, Capture, CaptureBuilder, Device, EventReceiver, Session, SessionEvent,
    SessionState, StateWatcher, SystemBuilder,
};
#[cfg(feature = "runtime-tokio")]
pub(crate) use backends::BackendType;
pub use contract::*;
#[cfg(feature = "runtime-tokio")]
pub use controls::{
    ControlDescriptor, ControlId, ControlMode, ControlOutcome, ControlRange, ControlReadback,
    ControlUnit, ControlValue, Exposure, Focus, Kelvin, WhiteBalance,
};
#[cfg(feature = "convert-rgb")]
pub use conversion::{selected_conversion_path, ConversionRequest, RgbConverter};
pub use error::{CameraError, CameraErrorKind, RecoveryHint};
#[cfg(all(feature = "runtime-tokio", feature = "custom-backend"))]
pub use extension::{
    BackendDevice, BackendDeviceInfo, BackendProvider, FrameSink, WritableFrameLease,
};
pub use format::{
    ClockDomain, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, FrameLayout, FrameRate,
    MemoryBudget, Orientation, OverflowPolicy, PixelFormat, PlaneLayout, ReconnectPolicy,
    SourceTimestamp, TransferFunction,
};
pub(crate) use format::{DeliveryPolicy, OutputFormat, SelectionPolicy, StreamRequest};
#[cfg(feature = "runtime-tokio")]
#[cfg(not(feature = "benchmark-internals"))]
pub(crate) use frame::FrameHub;
#[cfg(feature = "runtime-tokio")]
#[cfg(feature = "benchmark-internals")]
#[doc(hidden)]
pub use frame::FrameHub;
#[cfg(feature = "runtime-tokio")]
pub use frame::{CapturedFrame, Frame, FrameKey, FrameMetrics, FrameReceiver, RgbView};
#[cfg(feature = "runtime-tokio")]
pub use traits::StreamStats;
#[cfg(feature = "runtime-tokio")]
pub(crate) use traits::{CameraControl, CameraManager, StreamingCamera};
/// Result type returned by all core camera operations.
pub type CameraResult<T> = std::result::Result<T, CameraError>;
pub(crate) use types::{CameraConfig, VideoFormat};
#[cfg(feature = "runtime-tokio")]
pub(crate) use types::{
    CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
};
#[cfg(test)]
#[cfg(feature = "runtime-tokio")]
mod api_regressions_tests;
#[cfg(feature = "runtime-tokio")]
mod device_watch;
#[cfg(test)]
#[cfg(feature = "runtime-tokio")]
mod shared_frames_tests;
#[cfg(feature = "runtime-tokio")]
pub use device_watch::{DeviceSnapshot, DeviceWatcher};

#[cfg(test)]
#[cfg(feature = "runtime-tokio")]
mod yuv_planes_tests;
