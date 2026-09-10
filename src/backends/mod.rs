//! 后端抽象层
//!
//! 提供统一的摄像头后端接口，支持多种平台实现：
//!
//! | 后端 | 平台 | 描述 |
//! |------|------|------|
//! | V4L2 | Linux | Native kernel camera capture (MMAP) |
//! | UVC | Linux, macOS, Android | 基于 libuvc 的 USB 摄像头支持 |
//! | AVFoundation | macOS, iOS | Apple 平台原生相机 API |
//! | Camera2 | Android | Android Framework 相机 API |
//! | Media Foundation | Windows | Windows 平台媒体 API |
//!
//! # 架构设计
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    medivh-camera API                        │
//! │  (CameraManager, StreamingCamera, CameraControl traits)     │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     backends 模块                           │
//! │  ┌─────────┐ ┌─────────────┐ ┌────────┐ ┌────────────────┐ │
//! │  │   UVC   │ │ AVFoundation│ │Camera2 │ │MediaFoundation │ │
//! │  │ (Linux, │ │   (macOS,   │ │(Android│ │   (Windows)    │ │
//! │  │ macOS,  │ │    iOS)     │ │  only) │ │                │ │
//! │  │Android) │ │             │ │        │ │                │ │
//! │  └─────────┘ └─────────────┘ └────────┘ └────────────────┘ │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # 使用方式
//!
//! ## 直接使用特定后端
//!
//! ## 使用自动后端选择（推荐）

#[allow(unused_imports)]
use crate::CameraManager;
use crate::{CameraError, CameraResult};
#[allow(unused_imports)]
use std::sync::Arc;

// ==================== 后端模块 ====================

// UVC 后端：Linux, macOS, Android (通过 libusb/libuvc)
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

// AVFoundation 后端：macOS, iOS
#[cfg(all(
    any(feature = "native", feature = "backend-avfoundation"),
    target_vendor = "apple"
))]
pub mod avfoundation;

// Camera2 后端：Android
#[cfg(all(
    any(feature = "native", feature = "backend-camera2"),
    target_os = "android"
))]
pub mod camera2;

// Media Foundation 后端：Windows
#[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
pub mod mf;

// ==================== 后端类型枚举 ====================

/// 摄像头后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendType {
    /// 自动选择最佳后端
    Auto,
    /// UVC (USB Video Class) - 跨平台 USB 摄像头
    Uvc,
    /// Video4Linux2 - Linux kernel camera drivers
    V4l2,
    /// AVFoundation - Apple 平台原生 API
    AVFoundation,
    /// Camera2 - Android 原生 API
    Camera2,
    /// Media Foundation - Windows 原生 API
    MediaFoundation,
}

impl BackendType {
    /// 获取当前平台默认的后端类型
    pub fn default_for_platform() -> Self {
        if cfg!(all(
            target_os = "android",
            any(feature = "native", feature = "backend-camera2")
        )) {
            Self::Camera2
        } else if cfg!(all(
            target_vendor = "apple",
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

    /// 检查后端在当前平台是否可用
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
                    target_vendor = "apple"
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

    /// 获取后端的人类可读名称
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

// ==================== 后端工厂函数 ====================

/// 获取所有在当前平台可用的后端类型
pub fn available_backends() -> Vec<BackendType> {
    vec![
        #[cfg(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ))]
        BackendType::Uvc,
        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            target_vendor = "apple"
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

// ==================== 摄像头类型包装 ====================

/// 统一的摄像头类型
///
/// 由于 `StreamingCamera` trait 包含 async 方法，无法直接用于 `dyn` trait object。
/// 此枚举提供了一种类型安全的方式来处理不同后端的摄像头实例。
///
/// # 使用方式
#[non_exhaustive]
#[derive(Clone)]
pub enum BackendCamera {
    /// Linux V4L2 camera
    #[cfg(camera_v4l2)]
    V4l2(Arc<v4l2::V4l2Camera>),

    /// UVC 摄像头 (USB Video Class)
    #[cfg(all(
        feature = "backend-uvc",
        any(target_os = "linux", target_os = "macos", target_os = "android")
    ))]
    Uvc(Arc<uvc::UvcCamera>),

    /// AVFoundation 摄像头 (macOS/iOS)
    #[cfg(all(
        any(feature = "native", feature = "backend-avfoundation"),
        target_vendor = "apple"
    ))]
    AVFoundation(Arc<avfoundation::AVFoundationCamera>),

    /// Camera2 摄像头 (Android)
    #[cfg(all(
        any(feature = "native", feature = "backend-camera2"),
        target_os = "android"
    ))]
    Camera2(Arc<camera2::Camera2Camera>),

    /// Media Foundation 摄像头 (Windows)
    #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
    MediaFoundation(Arc<mf::MFCamera>),
}

fn no_camera_backend_enabled() -> CameraError {
    CameraError::Other("no camera backend is enabled for this build".into())
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
                target_vendor = "apple"
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

    /// 获取后端类型
    pub fn backend_type(&self) -> BackendType {
        match self {
            #[cfg(all(
                feature = "backend-uvc",
                any(target_os = "linux", target_os = "macos", target_os = "android")
            ))]
            BackendCamera::Uvc(_) => BackendType::Uvc,

            #[cfg(all(
                any(feature = "native", feature = "backend-avfoundation"),
                target_vendor = "apple"
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

    // ==================== StreamingCamera 接口 ====================

    /// 启动视频流
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
                target_vendor = "apple"
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

    /// 停止视频流
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
                target_vendor = "apple"
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

    /// 获取最新帧
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
                target_vendor = "apple"
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

    /// 等待并获取下一帧
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
                target_vendor = "apple"
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

    /// 检查流是否正在运行
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
                target_vendor = "apple"
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

    /// 获取当前配置
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
                target_vendor = "apple"
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

    /// 获取流统计信息
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
                target_vendor = "apple"
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

    /// 设置缓冲区大小
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
                target_vendor = "apple"
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

    /// 清理资源
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

            // 其他后端可能没有 cleanup 方法，忽略即可
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    // ==================== CameraControl 接口 ====================

    /// 获取指定控制参数的当前值
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
                target_vendor = "apple"
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

    /// 设置指定控制参数的值
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
                target_vendor = "apple"
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

    /// 获取指定控制参数的取值范围
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
                target_vendor = "apple"
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

    /// 检查设备是否支持指定的控制参数
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
                target_vendor = "apple"
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

    /// 获取设备支持的所有控制参数
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
                target_vendor = "apple"
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

    /// 将控制参数重置为默认值
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
                target_vendor = "apple"
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

    /// 将所有控制参数重置为默认值
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
                target_vendor = "apple"
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

/// 使用指定后端创建摄像头实例
///
/// # Arguments
///
/// * `device_index` - 设备索引
/// * `backend` - 后端类型，使用 `BackendType::Auto` 自动选择
///
/// # Returns
///
/// 返回 `BackendCamera` 枚举，包装了具体的后端实现
///
/// # 回退策略
///
/// 如果指定的后端在当前平台不可用，会自动回退到平台默认后端，
/// 而不是返回错误。这样可以提供更好的跨平台兼容性。
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
    // 如果是 Auto 或指定的后端不可用，使用平台默认后端
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
            // 不应该到达这里
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
        BackendType::Uvc => Err(CameraError::Other(
            "UVC backend is not enabled. Enable feature 'backend-uvc'".into(),
        )),

        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            target_vendor = "apple"
        ))]
        BackendType::AVFoundation => {
            let camera =
                avfoundation::AVFoundationCamera::new_selected(_device_index, _hub, _expected)?;
            Ok(BackendCamera::AVFoundation(Arc::new(camera)))
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-avfoundation"),
            target_vendor = "apple"
        )))]
        BackendType::AVFoundation => Err(CameraError::Other(
            "AVFoundation backend is not available on this platform".into(),
        )),

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
        BackendType::Camera2 => Err(CameraError::Other(
            "Camera2 backend is not available on this platform".into(),
        )),

        #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
        BackendType::MediaFoundation => {
            let camera = mf::MFCamera::new_selected(_device_index, _hub, _expected)?;
            Ok(BackendCamera::MediaFoundation(Arc::new(camera)))
        }

        #[cfg(not(all(any(feature = "native", feature = "backend-mf"), target_os = "windows")))]
        BackendType::MediaFoundation => Err(CameraError::Other(
            "Media Foundation backend is not available on this platform".into(),
        )),

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
        BackendType::V4l2 => Err(CameraError::Other(
            "V4L2 backend requires Linux and feature 'backend-v4l2'".into(),
        )),
    }
}

/// 从文件描述符创建 UVC 摄像头 (Android 专用)
///
/// 在 Android 上，应用无法直接访问 USB 设备。需要通过 Java 层获取 fd 后传入此函数。
///
/// # Arguments
///
/// * `fd` - 从 UsbDeviceConnection.getFileDescriptor() 获取的文件描述符
///
/// # Returns
///
/// 返回 `BackendCamera::Uvc` 变体
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

/// 使用指定后端列出所有可用设备
///
/// # Arguments
///
/// * `backend` - 后端类型，使用 `BackendType::Auto` 使用默认后端
///
/// # Returns
///
/// 返回设备列表
///
/// # 回退策略
///
/// 如果指定的后端在当前平台不可用，会自动回退到平台默认后端，
/// 而不是返回错误。这样可以提供更好的跨平台兼容性。
pub fn list_devices(backend: BackendType) -> CameraResult<Vec<crate::CameraDeviceInfo>> {
    // 如果是 Auto 或指定的后端不可用，使用平台默认后端
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
        BackendType::Uvc => Err(CameraError::Other(
            "UVC backend is not enabled. Enable feature 'backend-uvc'".into(),
        )),

        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            target_vendor = "apple"
        ))]
        BackendType::AVFoundation => {
            <avfoundation::AVFoundationCamera as CameraManager>::list_devices()
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-avfoundation"),
            target_vendor = "apple"
        )))]
        BackendType::AVFoundation => Err(CameraError::Other(
            "AVFoundation backend is not available on this platform".into(),
        )),

        #[cfg(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        ))]
        BackendType::Camera2 => <camera2::Camera2Camera as CameraManager>::list_devices(),

        #[cfg(not(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        )))]
        BackendType::Camera2 => Err(CameraError::Other(
            "Camera2 backend is not available on this platform".into(),
        )),

        #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
        BackendType::MediaFoundation => <mf::MFCamera as CameraManager>::list_devices(),

        #[cfg(not(all(any(feature = "native", feature = "backend-mf"), target_os = "windows")))]
        BackendType::MediaFoundation => Err(CameraError::Other(
            "Media Foundation backend is not available on this platform".into(),
        )),

        #[cfg(camera_v4l2)]
        BackendType::V4l2 => v4l2::V4l2Camera::list_devices(),
        #[cfg(not(camera_v4l2))]
        BackendType::V4l2 => Err(CameraError::Other(
            "V4L2 backend requires Linux and feature 'backend-v4l2'".into(),
        )),
    }
}

/// 使用指定后端获取设备支持的视频配置。
///
/// `BackendType::Auto` 会解析为当前平台默认后端；不可用的后端会与
/// [`list_devices`] 保持一致，回退到平台默认后端。
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
        BackendType::Uvc => Err(CameraError::Other(
            "UVC backend is not enabled. Enable feature 'backend-uvc'".into(),
        )),

        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            target_vendor = "apple"
        ))]
        BackendType::AVFoundation => {
            <avfoundation::AVFoundationCamera as CameraManager>::get_supported_configs(
                _device_index,
            )
        }

        #[cfg(not(all(
            any(feature = "native", feature = "backend-avfoundation"),
            target_vendor = "apple"
        )))]
        BackendType::AVFoundation => Err(CameraError::Other(
            "AVFoundation backend is not available on this platform".into(),
        )),

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
        BackendType::Camera2 => Err(CameraError::Other(
            "Camera2 backend is not available on this platform".into(),
        )),

        #[cfg(all(any(feature = "native", feature = "backend-mf"), target_os = "windows"))]
        BackendType::MediaFoundation => {
            <mf::MFCamera as CameraManager>::get_supported_configs(_device_index)
        }

        #[cfg(not(all(any(feature = "native", feature = "backend-mf"), target_os = "windows")))]
        BackendType::MediaFoundation => Err(CameraError::Other(
            "Media Foundation backend is not available on this platform".into(),
        )),

        #[cfg(camera_v4l2)]
        BackendType::V4l2 => v4l2::V4l2Camera::get_supported_configs(_device_index),
        #[cfg(not(camera_v4l2))]
        BackendType::V4l2 => Err(CameraError::Other(
            "V4L2 backend requires Linux and feature 'backend-v4l2'".into(),
        )),
    }
}

// ==================== 测试 ====================

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
        // 至少应该有一个后端可用（UVC 默认启用）
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
                target_vendor = "apple"
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
        target_vendor = "apple",
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
            target_vendor = "apple"
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
            Err(error) => error.to_string(),
        };
        let list_err = list_devices(BackendType::Auto)
            .expect_err("device listing should fail when no backend is enabled")
            .to_string();
        let config_err = get_supported_configs(BackendType::Auto, 0)
            .expect_err("config listing should fail when no backend is enabled")
            .to_string();

        for message in [create_err, list_err, config_err] {
            assert!(
                message.contains("not available") || message.contains("not enabled"),
                "unexpected no-backend error: {message}"
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
