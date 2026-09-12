//! Video format conversion.
//!
//! Converts Windows Media Foundation formats to RGB.

use crate::types::VideoFormat;
use windows::core::GUID;

/// Windows Media Format GUIDs
/// Reference: https://gix.github.io/media-types/#major-types
pub mod format_guids {
    use windows::core::GUID;

    /// YUY2 (YUYV) format.
    pub const MF_VIDEO_FORMAT_YUY2: GUID = GUID::from_values(
        0x3259_5559,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );

    /// MJPEG format.
    pub const MF_VIDEO_FORMAT_MJPEG: GUID = GUID::from_values(
        0x4750_4A4D,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );

    /// Grayscale format (Y8/GRAY).
    pub const MF_VIDEO_FORMAT_GRAY: GUID = GUID::from_values(
        0x3030_3859,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );

    /// NV12 format (YUV 4:2:0).
    pub const MF_VIDEO_FORMAT_NV12: GUID = GUID::from_values(
        0x3231_564E,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );

    /// RGB24 format.
    pub const MF_VIDEO_FORMAT_RGB24: GUID = GUID::from_values(
        0x0000_0014,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );

    /// RGB32 format.
    pub const MF_VIDEO_FORMAT_RGB32: GUID = GUID::from_values(
        0x0000_0016,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );

    /// UYVY format.
    pub const MF_VIDEO_FORMAT_UYVY: GUID = GUID::from_values(
        0x5956_5559,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );
}

use format_guids::*;

/// Converts a GUID into a VideoFormat.
pub fn guid_to_format(guid: GUID) -> Option<VideoFormat> {
    match guid {
        g if g == MF_VIDEO_FORMAT_YUY2 => Some(VideoFormat::YUYV),
        g if g == MF_VIDEO_FORMAT_MJPEG => Some(VideoFormat::MJPEG),
        g if g == MF_VIDEO_FORMAT_NV12 => Some(VideoFormat::NV12),
        g if g == MF_VIDEO_FORMAT_RGB24 => Some(VideoFormat::RGB),
        g if g == MF_VIDEO_FORMAT_GRAY => Some(VideoFormat::Gray),
        g if g == MF_VIDEO_FORMAT_UYVY => Some(VideoFormat::UYVY),
        _ => None,
    }
}

/// Converts a VideoFormat into a GUID.
pub fn format_to_guid(format: VideoFormat) -> Option<GUID> {
    match format {
        VideoFormat::YUYV => Some(MF_VIDEO_FORMAT_YUY2),
        VideoFormat::MJPEG => Some(MF_VIDEO_FORMAT_MJPEG),
        VideoFormat::NV12 => Some(MF_VIDEO_FORMAT_NV12),
        VideoFormat::RGB => Some(MF_VIDEO_FORMAT_RGB24),
        VideoFormat::Gray => Some(MF_VIDEO_FORMAT_GRAY),
        VideoFormat::UYVY => Some(MF_VIDEO_FORMAT_UYVY),
        _ => None,
    }
}
