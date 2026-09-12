//! AVFoundation camera backend for macOS and iOS.
//!
//! Provides native Apple camera support through objc2 bindings.
//!
//! # Platform support
//!
//! - macOS and iOS using the linked Apple SDK frameworks
//! - Deployment support depends on the APIs used by the host application
//!
//! # Features
//!
//! Compared with UVC, the AVFoundation backend provides:
//!
//! - Support for native Apple cameras such as FaceTime HD and iOS cameras.
//! - Better power management.
//! - Front and rear camera switching.
//! - Integration with system camera permissions.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    AVFoundationCamera                        │
//! │  (implements CameraManager, StreamingCamera, CameraControl)  │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    CaptureSession                            │
//! │  (wraps AVCaptureSession, AVCaptureDeviceInput,              │
//! │   AVCaptureVideoDataOutput)                                  │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    FrameDelegate                             │
//! │  (frame callbacks, conversion, and channel delivery)         │
//! └─────────────────────────────────────────────────────────────┘
//! ```

pub(crate) mod camera;
mod capture;
mod delegate;
mod device;

pub use camera::AVFoundationCamera;
