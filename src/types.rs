//! Core data type definitions.

use crate::error::CameraError;

/// Result type for camera operations.
pub type CameraResult<T> = Result<T, CameraError>;

/// Video pixel and encoded formats.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    /// MJPEG compressed format.
    MJPEG,
    /// Uncompressed YUYV format.
    YUYV,
    /// Uncompressed UYVY format.
    UYVY,
    /// NV12 format (YUV 4:2:0).
    NV12,
    /// RGB24 format.
    RGB,
    /// H.264 encoded format.
    H264,
    /// Grayscale format.
    Gray,
}

impl VideoFormat {
    /// Returns whether this is a compressed format.
    pub fn is_compressed(&self) -> bool {
        matches!(self, VideoFormat::MJPEG | VideoFormat::H264)
    }

    /// Returns bytes per pixel for fixed-size uncompressed formats.
    pub fn bytes_per_pixel(&self) -> Option<usize> {
        match self {
            VideoFormat::YUYV | VideoFormat::UYVY => Some(2),
            VideoFormat::NV12 => None, // NV12 uses 1.5 bytes per pixel.
            VideoFormat::RGB => Some(3),
            VideoFormat::Gray => Some(1),
            _ => None, // Compressed formats have no fixed size per pixel.
        }
    }

    /// Calculates the frame data size for the given resolution.
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

/// Camera stream configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraConfig {
    /// Video format.
    pub format: VideoFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frame rate in frames per second.
    pub fps: u32,
    /// Frame rate denominator; the rate is fps / fps_denominator.
    pub fps_denominator: u32,
}

impl CameraConfig {
    /// Creates a new configuration.
    pub fn new(format: VideoFormat, width: u32, height: u32, fps: u32) -> Self {
        Self {
            format,
            width,
            height,
            fps,
            fps_denominator: 1,
        }
    }

    /// Default MJPEG configuration (640x480 at 30 FPS).
    pub fn default_mjpeg() -> Self {
        Self::new(VideoFormat::MJPEG, 640, 480, 30)
    }

    /// HD MJPEG configuration (1280x720 at 30 FPS).
    pub fn hd_mjpeg() -> Self {
        Self::new(VideoFormat::MJPEG, 1280, 720, 30)
    }

    /// Full HD MJPEG configuration (1920x1080 at 30 FPS).
    pub fn full_hd_mjpeg() -> Self {
        Self::new(VideoFormat::MJPEG, 1920, 1080, 30)
    }

    /// Default YUYV configuration (640x480 at 30 FPS).
    pub fn default_yuyv() -> Self {
        Self::new(VideoFormat::YUYV, 640, 480, 30)
    }

    /// Calculates the frame size for an uncompressed format.
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

    /// Validates the configuration.
    pub fn validate(&self) -> CameraResult<()> {
        if self.width == 0 || self.height == 0 {
            return Err(CameraError::invalid_config(
                "Resolution cannot be 0".to_string(),
            ));
        }
        if self.fps == 0 || self.fps_denominator == 0 {
            return Err(CameraError::invalid_config(
                "Frame rate numerator and denominator must be positive".to_string(),
            ));
        }
        if !self.format.is_compressed() && self.frame_size().is_none() {
            return Err(CameraError::invalid_config(format!(
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

/// Camera device information.
#[derive(Debug, Clone)]
pub struct CameraDeviceInfo {
    /// Device index.
    pub index: u32,
    /// Device name.
    pub name: String,
    /// Device description.
    pub description: String,
    /// Vendor ID (USB VID), when available for the backend.
    pub vendor_id: Option<u16>,
    /// Product ID (USB PID), when available for the backend.
    pub product_id: Option<u16>,
    /// Serial number.
    pub serial_number: Option<String>,
    /// Platform-specific device path.
    pub device_path: Option<String>,
}

impl CameraDeviceInfo {
    /// Creates basic device information.
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

    /// Returns the device's unique identifier.
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

    /// Returns a user-friendly display name.
    pub fn display_name(&self) -> &str {
        &self.name
    }
}

// ============================================================================
// Camera controls.
// ============================================================================

/// Camera control type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraControlType {
    /// Brightness.
    Brightness,
    /// Contrast.
    Contrast,
    /// Hue.
    Hue,
    /// Saturation.
    Saturation,
    /// Sharpness.
    Sharpness,
    /// Gamma.
    Gamma,
    /// White balance.
    WhiteBalance,
    /// Backlight compensation.
    BacklightCompensation,
    /// Gain.
    Gain,
    /// Horizontal pan.
    Pan,
    /// Vertical tilt.
    Tilt,
    /// Zoom.
    Zoom,
    /// Exposure.
    Exposure,
    /// Iris.
    Iris,
    /// Focus.
    Focus,
    /// Automatic exposure.
    AutoExposure,
    /// Automatic focus.
    AutoFocus,
    /// Automatic white balance.
    AutoWhiteBalance,
}

impl CameraControlType {
    /// Returns the control's display name.
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

    /// Returns whether this control selects an automatic mode.
    pub fn is_auto_control(&self) -> bool {
        matches!(
            self,
            Self::AutoExposure | Self::AutoFocus | Self::AutoWhiteBalance
        )
    }
}

/// Camera control value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraControlValue {
    /// Integer value with an automatic-mode flag.
    Integer { value: i32, is_auto: bool },
    /// Boolean value.
    Boolean(bool),
    /// Floating-point value.
    Float(f32),
}

impl CameraControlValue {
    /// Creates a manual integer value.
    pub fn manual(value: i32) -> Self {
        Self::Integer {
            value,
            is_auto: false,
        }
    }

    /// Creates an automatic integer value.
    pub fn auto(value: i32) -> Self {
        Self::Integer {
            value,
            is_auto: true,
        }
    }

    /// Attempts to convert the value to `i32`.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Self::Integer { value, .. } => Some(*value),
            Self::Boolean(b) => Some(if *b { 1 } else { 0 }),
            Self::Float(f) => Some(*f as i32),
        }
    }

    /// Attempts to convert the value to `bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            Self::Integer { value, .. } => Some(*value != 0),
            _ => None,
        }
    }

    /// Returns whether automatic mode is enabled.
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

/// Valid range for a camera control.
#[derive(Debug, Clone, Copy)]
pub struct CameraControlRange {
    /// Minimum value.
    pub min: i32,
    /// Maximum value.
    pub max: i32,
    /// Step size.
    pub step: i32,
    /// Default value.
    pub default: i32,
    /// Whether automatic mode is supported.
    pub supports_auto: bool,
}

impl CameraControlRange {
    /// Creates a control range.
    pub fn new(min: i32, max: i32, step: i32, default: i32, supports_auto: bool) -> Self {
        Self {
            min,
            max,
            step,
            default,
            supports_auto,
        }
    }

    /// Checks whether a value is within the range.
    pub fn is_in_range(&self, value: i32) -> bool {
        let (min, max) = self.ordered_bounds();
        value >= min && value <= max
    }

    /// Clamps a value to the range.
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
        assert_eq!(mjpeg_config.frame_size(), None); // Compressed format.
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
            Err(ref error) if error.kind() == crate::CameraErrorKind::InvalidArgument
        ));

        let nv12 = CameraConfig::new(VideoFormat::NV12, 2, 3, 30);
        assert!(matches!(
            nv12.validate(),
            Err(ref error) if error.kind() == crate::CameraErrorKind::InvalidArgument
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
