//! Windows Media Foundation 摄像头后端
//!
//! 该模块提供 Windows 平台的原生摄像头支持。
//!
//! # 平台支持
//!
//! - Windows Vista 及以上 (推荐 Windows 10+)
//!
//! # 功能特点
//!
//! Media Foundation 后端相比 DirectShow/UVC 有以下优势：
//!
//! - 现代 Windows 媒体 API
//! - 更好的硬件加速支持
//! - 支持 Windows Hello 相机
//! - 与 Windows 相机权限集成 (Windows 10+)
//!
//! # 架构
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
// 非 Windows 平台的占位实现
#[cfg(not(all(target_os = "windows", any(feature = "native", feature = "backend-mf"))))]
mod camera_stub;
#[cfg(not(all(target_os = "windows", any(feature = "native", feature = "backend-mf"))))]
pub use camera_stub::MFCamera;
