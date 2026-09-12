//! AVFoundation 摄像头后端 (macOS/iOS)
//!
//! 该模块提供 Apple 平台的原生摄像头支持，使用 objc2 绑定。
//!
//! # 平台支持
//!
//! - macOS and iOS using the linked Apple SDK frameworks
//! - Deployment support depends on the APIs used by the host application
//!
//! # 功能特点
//!
//! AVFoundation 后端相比 UVC 有以下优势：
//!
//! - 支持 Apple 原生相机（FaceTime HD、iOS 设备相机等）
//! - 更好的电源管理
//! - 支持相机切换（前/后置）
//! - 与系统相机权限集成
//!
//! # 架构
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    AVFoundationCamera                        │
//! │  (实现 CameraManager, StreamingCamera, CameraControl)        │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    CaptureSession                            │
//! │  (封装 AVCaptureSession, AVCaptureDeviceInput,              │
//! │   AVCaptureVideoDataOutput)                                  │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    FrameDelegate                             │
//! │  (帧回调处理，格式转换，通过 channel 传递帧数据)              │
//! └─────────────────────────────────────────────────────────────┘
//! ```

pub(crate) mod camera;
mod capture;
mod delegate;
mod device;

pub use camera::AVFoundationCamera;
