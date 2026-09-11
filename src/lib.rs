//! Async cross-platform camera capture with owned sessions and immutable frames.
//!
//! ```no_run
//! use camera::{CameraSystem,StreamRequest};
//! # async fn capture()->camera::CameraResult<()> {
//! let system=CameraSystem::new();
//! let device=system.devices().await?.into_iter().next()
//!     .ok_or_else(||camera::CameraError::DeviceNotFound("No camera".into()))?;
//! let mut camera=system.open(&device.id).await?;
//! let session=camera.start(StreamRequest::builder().resolution(1280,720).build()?).await?;
//! let mut frames=session.subscribe();
//! let frame=frames.next().await?;
//! println!("{} bytes, {:?}",frame.bytes().len(),frame.layout());
//! session.stop().await?;
//! # Ok(()) }
//! ```
mod api;
#[allow(dead_code)]
mod backends;
mod controls;
mod conversion;
mod error;
mod format;
mod frame;
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
#[allow(dead_code)]
mod traits;
// Native adapters keep a small internal compatibility vocabulary.
#[allow(dead_code)]
mod types;
#[allow(dead_code)]
mod utils;

pub use api::{
    Camera, CameraSystem, CaptureSession, DeviceCapabilities, DeviceId, DeviceInfo,
    IdentityStability, SessionState,
};
pub use backends::BackendType;
pub use controls::{
    ControlDescriptor, ControlId, ControlMode, ControlOutcome, ControlRange, ControlReadback,
    ControlUnit, ControlValue,
};
pub use conversion::{ConversionRequest, RgbConverter};
pub use error::{CameraError, Result};
pub use format::*;
pub(crate) use frame::FrameHub;
pub use frame::{CapturedFrame, Frame, FrameKey, FrameMetrics, FrameReceiver};
pub use traits::StreamStats;
pub(crate) use traits::{CameraControl, CameraManager, StreamingCamera};
pub use types::{CameraConfig, CameraResult, VideoFormat};
pub(crate) use types::{
    CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
};
#[cfg(test)]
mod api_regressions_tests;
mod device_watch;
#[cfg(test)]
mod shared_frames_tests;
pub use device_watch::{DeviceSnapshot, DeviceWatcher};

#[cfg(test)]
mod yuv_planes_tests;
