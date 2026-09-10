//! 核心数据类型定义

use crate::error::CameraError;

/// 摄像头操作结果类型
pub type CameraResult<T> = Result<T, CameraError>;

/// 视频格式枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    /// MJPEG 压缩格式
    MJPEG,
    /// YUYV 未压缩格式
    YUYV,
    /// UYVY 未压缩格式  
    UYVY,
    /// NV12 格式 (YUV 4:2:0)
    NV12,
    /// RGB24 格式
    RGB,
    /// H264 编码
    H264,
    /// 灰度图
    Gray,
}

impl VideoFormat {
    /// 是否为压缩格式
    pub fn is_compressed(&self) -> bool {
        matches!(self, VideoFormat::MJPEG | VideoFormat::H264)
    }

    /// 获取每像素字节数（未压缩格式）
    pub fn bytes_per_pixel(&self) -> Option<usize> {
        match self {
            VideoFormat::YUYV | VideoFormat::UYVY => Some(2),
            VideoFormat::NV12 => None, // NV12 是 1.5 字节/像素
            VideoFormat::RGB => Some(3),
            VideoFormat::Gray => Some(1),
            _ => None, // 压缩格式不固定
        }
    }

    /// 计算给定分辨率的帧数据大小
    pub fn frame_data_size(&self, width: u32, height: u32) -> Option<usize> {
        let pixels = checked_pixel_count(width, height)?;
        match self {
            VideoFormat::YUYV | VideoFormat::UYVY => {
                if !width.is_multiple_of(2) {
                    return None;
                }
                pixels.checked_mul(2)
            }
            VideoFormat::NV12 => {
                if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
                    return None;
                }
                pixels.checked_add(pixels / 2) // Y + UV
            }
            VideoFormat::RGB => pixels.checked_mul(3),
            VideoFormat::Gray => Some(pixels),
            _ => None,
        }
    }
}

/// 摄像头配置
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraConfig {
    /// 视频格式
    pub format: VideoFormat,
    /// 宽度（像素）
    pub width: u32,
    /// 高度（像素）
    pub height: u32,
    /// 帧率（fps）
    pub fps: u32,
    /// Frame rate denominator; the rate is fps / fps_denominator.
    pub fps_denominator: u32,
}

impl CameraConfig {
    /// 创建新配置
    pub fn new(format: VideoFormat, width: u32, height: u32, fps: u32) -> Self {
        Self {
            format,
            width,
            height,
            fps,
            fps_denominator: 1,
        }
    }

    /// 默认 MJPEG 配置 (640x480@30fps)
    pub fn default_mjpeg() -> Self {
        Self::new(VideoFormat::MJPEG, 640, 480, 30)
    }

    /// HD MJPEG 配置 (1280x720@30fps)
    pub fn hd_mjpeg() -> Self {
        Self::new(VideoFormat::MJPEG, 1280, 720, 30)
    }

    /// Full HD MJPEG 配置 (1920x1080@30fps)
    pub fn full_hd_mjpeg() -> Self {
        Self::new(VideoFormat::MJPEG, 1920, 1080, 30)
    }

    /// 默认 YUYV 配置 (640x480@30fps)
    pub fn default_yuyv() -> Self {
        Self::new(VideoFormat::YUYV, 640, 480, 30)
    }

    /// 计算未压缩格式的帧大小
    pub fn frame_size(&self) -> Option<usize> {
        self.format.frame_data_size(self.width, self.height)
    }

    pub fn with_frame_rate(mut self, rate: crate::FrameRate) -> Self {
        self.fps = rate.numerator();
        self.fps_denominator = rate.denominator();
        self
    }
    pub fn frame_rate(&self) -> CameraResult<crate::FrameRate> {
        crate::FrameRate::new(self.fps, self.fps_denominator)
    }

    /// 验证配置是否合理
    pub fn validate(&self) -> CameraResult<()> {
        if self.width == 0 || self.height == 0 {
            return Err(CameraError::InvalidConfig(
                "Resolution cannot be 0".to_string(),
            ));
        }
        if self.fps == 0 || self.fps_denominator == 0 {
            return Err(CameraError::InvalidConfig(
                "Frame rate numerator and denominator must be positive".to_string(),
            ));
        }
        if !self.format.is_compressed() && self.frame_size().is_none() {
            return Err(CameraError::InvalidConfig(format!(
                "Resolution {}x{} cannot represent {:?} frame data",
                self.width, self.height, self.format
            )));
        }
        Ok(())
    }
}

fn checked_pixel_count(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }

    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    width.checked_mul(height)
}

/// 摄像头设备信息
#[derive(Debug, Clone)]
pub struct CameraDeviceInfo {
    /// 设备索引
    pub index: u32,
    /// 设备名称
    pub name: String,
    /// 设备描述
    pub description: String,
    /// 厂商ID (USB VID)，可能不适用于所有后端
    pub vendor_id: Option<u16>,
    /// 产品ID (USB PID)，可能不适用于所有后端
    pub product_id: Option<u16>,
    /// 序列号
    pub serial_number: Option<String>,
    /// 设备路径（平台特定）
    pub device_path: Option<String>,
}

impl CameraDeviceInfo {
    /// 创建简单的设备信息
    pub fn new(index: u32, name: String, description: String) -> Self {
        Self {
            index,
            name,
            description,
            vendor_id: None,
            product_id: None,
            serial_number: None,
            device_path: None,
        }
    }

    /// 获取设备的唯一标识符
    pub fn unique_id(&self) -> String {
        if let Some(path) = self.device_path.as_ref().filter(|p| !p.is_empty()) {
            return path.clone();
        }
        if let (Some(vid), Some(pid)) = (self.vendor_id, self.product_id) {
            if let Some(ref serial) = self.serial_number {
                format!("{:04x}:{:04x}:{}", vid, pid, serial)
            } else {
                format!("{:04x}:{:04x}:{}", vid, pid, self.index)
            }
        } else {
            format!("device:{}", self.index)
        }
    }

    /// 获取友好的显示名称
    pub fn display_name(&self) -> &str {
        &self.name
    }
}

// ============================================================================
// 摄像头控制参数
// ============================================================================

/// 摄像头控制参数类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraControlType {
    /// 亮度
    Brightness,
    /// 对比度
    Contrast,
    /// 色调
    Hue,
    /// 饱和度
    Saturation,
    /// 锐度
    Sharpness,
    /// 伽马值
    Gamma,
    /// 白平衡
    WhiteBalance,
    /// 背光补偿
    BacklightCompensation,
    /// 增益
    Gain,
    /// 水平云台
    Pan,
    /// 垂直云台
    Tilt,
    /// 变焦
    Zoom,
    /// 曝光
    Exposure,
    /// 光圈
    Iris,
    /// 对焦
    Focus,
    /// 自动曝光
    AutoExposure,
    /// 自动对焦
    AutoFocus,
    /// 自动白平衡
    AutoWhiteBalance,
}

impl CameraControlType {
    /// 获取控制参数的显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Brightness => "Brightness",
            Self::Contrast => "Contrast",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Sharpness => "Sharpness",
            Self::Gamma => "Gamma",
            Self::WhiteBalance => "White Balance",
            Self::BacklightCompensation => "Backlight Compensation",
            Self::Gain => "Gain",
            Self::Pan => "Pan",
            Self::Tilt => "Tilt",
            Self::Zoom => "Zoom",
            Self::Exposure => "Exposure",
            Self::Iris => "Iris",
            Self::Focus => "Focus",
            Self::AutoExposure => "Auto Exposure",
            Self::AutoFocus => "Auto Focus",
            Self::AutoWhiteBalance => "Auto White Balance",
        }
    }

    /// 是否为自动模式控制
    pub fn is_auto_control(&self) -> bool {
        matches!(
            self,
            Self::AutoExposure | Self::AutoFocus | Self::AutoWhiteBalance
        )
    }
}

/// 摄像头控制参数值
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraControlValue {
    /// 整数值（带自动标志）
    Integer { value: i32, is_auto: bool },
    /// 布尔值
    Boolean(bool),
    /// 浮点值
    Float(f32),
}

impl CameraControlValue {
    /// 创建手动模式的整数值
    pub fn manual(value: i32) -> Self {
        Self::Integer {
            value,
            is_auto: false,
        }
    }

    /// 创建自动模式的整数值
    pub fn auto(value: i32) -> Self {
        Self::Integer {
            value,
            is_auto: true,
        }
    }

    /// 尝试转换为 i32
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Self::Integer { value, .. } => Some(*value),
            Self::Boolean(b) => Some(if *b { 1 } else { 0 }),
            Self::Float(f) => Some(*f as i32),
        }
    }

    /// 尝试转换为 bool
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            Self::Integer { value, .. } => Some(*value != 0),
            _ => None,
        }
    }

    /// 是否为自动模式
    pub fn is_auto(&self) -> bool {
        match self {
            Self::Integer { is_auto, .. } => *is_auto,
            _ => false,
        }
    }
}

impl From<bool> for CameraControlValue {
    fn from(v: bool) -> Self {
        Self::Boolean(v)
    }
}

impl From<i32> for CameraControlValue {
    fn from(v: i32) -> Self {
        Self::Integer {
            value: v,
            is_auto: false,
        }
    }
}

impl From<f32> for CameraControlValue {
    fn from(v: f32) -> Self {
        Self::Float(v)
    }
}

/// 摄像头控制参数范围
#[derive(Debug, Clone, Copy)]
pub struct CameraControlRange {
    /// 最小值
    pub min: i32,
    /// 最大值
    pub max: i32,
    /// 步进值
    pub step: i32,
    /// 默认值
    pub default: i32,
    /// 是否支持自动模式
    pub supports_auto: bool,
}

impl CameraControlRange {
    /// 创建新的控制范围
    pub fn new(min: i32, max: i32, step: i32, default: i32, supports_auto: bool) -> Self {
        Self {
            min,
            max,
            step,
            default,
            supports_auto,
        }
    }

    /// 检查值是否在范围内
    pub fn is_in_range(&self, value: i32) -> bool {
        let (min, max) = self.ordered_bounds();
        value >= min && value <= max
    }

    /// 将值钳制到范围内
    pub fn clamp(&self, value: i32) -> i32 {
        let (min, max) = self.ordered_bounds();
        value.clamp(min, max)
    }

    fn ordered_bounds(&self) -> (i32, i32) {
        if self.min <= self.max {
            (self.min, self.max)
        } else {
            (self.max, self.min)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_format() {
        assert!(VideoFormat::MJPEG.is_compressed());
        assert!(!VideoFormat::YUYV.is_compressed());
        assert_eq!(VideoFormat::YUYV.bytes_per_pixel(), Some(2));
        assert_eq!(VideoFormat::RGB.bytes_per_pixel(), Some(3));
    }

    #[test]
    fn test_camera_config() {
        let config = CameraConfig::default_mjpeg();
        assert_eq!(config.width, 640);
        assert_eq!(config.height, 480);
        assert_eq!(config.fps, 30);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation() {
        let mut config = CameraConfig::default_mjpeg();
        config.width = 0;
        assert!(config.validate().is_err());

        config.width = 640;
        config.fps = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_frame_size() {
        let config = CameraConfig::default_yuyv();
        assert_eq!(config.frame_size(), Some(640 * 480 * 2));

        let mjpeg_config = CameraConfig::default_mjpeg();
        assert_eq!(mjpeg_config.frame_size(), None); // 压缩格式
    }

    #[test]
    fn frame_size_returns_none_when_dimensions_overflow() {
        assert_eq!(VideoFormat::RGB.frame_data_size(u32::MAX, u32::MAX), None);

        let config = CameraConfig::new(VideoFormat::YUYV, u32::MAX, u32::MAX, 30);
        assert_eq!(config.frame_size(), None);
    }

    #[test]
    fn frame_size_returns_none_for_zero_dimensions() {
        assert_eq!(VideoFormat::RGB.frame_data_size(0, 480), None);
        assert_eq!(VideoFormat::Gray.frame_data_size(640, 0), None);

        let config = CameraConfig::new(VideoFormat::YUYV, 0, 480, 30);
        assert_eq!(config.frame_size(), None);
    }

    #[test]
    fn packed_422_frame_size_rejects_odd_width() {
        assert_eq!(VideoFormat::YUYV.frame_data_size(3, 2), None);
        assert_eq!(VideoFormat::UYVY.frame_data_size(3, 2), None);

        let config = CameraConfig::new(VideoFormat::YUYV, 3, 2, 30);
        assert_eq!(config.frame_size(), None);
    }

    #[test]
    fn nv12_frame_size_rejects_odd_dimensions() {
        assert_eq!(VideoFormat::NV12.frame_data_size(3, 2), None);
        assert_eq!(VideoFormat::NV12.frame_data_size(2, 3), None);

        let config = CameraConfig::new(VideoFormat::NV12, 3, 2, 30);
        assert_eq!(config.frame_size(), None);
    }

    #[test]
    fn validate_rejects_unrepresentable_uncompressed_frame_sizes() {
        let yuyv = CameraConfig::new(VideoFormat::YUYV, 3, 2, 30);
        assert!(matches!(
            yuyv.validate(),
            Err(CameraError::InvalidConfig(_))
        ));

        let nv12 = CameraConfig::new(VideoFormat::NV12, 2, 3, 30);
        assert!(matches!(
            nv12.validate(),
            Err(CameraError::InvalidConfig(_))
        ));

        let mjpeg = CameraConfig::new(VideoFormat::MJPEG, 3, 3, 30);
        assert!(mjpeg.validate().is_ok());
    }

    #[test]
    fn camera_control_range_handles_reversed_bounds_without_panicking() {
        let range = CameraControlRange::new(100, 0, 1, 50, false);

        assert!(range.is_in_range(50));
        assert_eq!(range.clamp(-10), 0);
        assert_eq!(range.clamp(110), 100);
    }
}
