//! AVFoundation device enumeration and permission handling.

use crate::error::CameraError;
use crate::types::{CameraConfig, CameraDeviceInfo, CameraResult, VideoFormat};

use objc2::rc::Retained;
use objc2::Message;
#[cfg(target_os = "macos")]
#[allow(deprecated)]
use objc2_av_foundation::AVCaptureDeviceTypeExternalUnknown;
use objc2_av_foundation::{
    AVAuthorizationStatus as AVAuthStatus, AVCaptureDevice, AVCaptureDeviceDiscoverySession,
    AVCaptureDeviceFormat, AVCaptureDevicePosition, AVCaptureDeviceType,
    AVCaptureDeviceTypeBuiltInWideAngleCamera, AVMediaTypeVideo,
};
use objc2_foundation::{NSArray, NSString};

const FRAME_RATE_EPSILON: f64 = 0.01;

pub(super) fn fps_matches_range(requested: f64, min_fps: f64, max_fps: f64) -> bool {
    requested >= min_fps - FRAME_RATE_EPSILON && requested <= max_fps + FRAME_RATE_EPSILON
}

/// Camera permission status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AVAuthorizationStatus {
    /// The user has not made a choice.
    NotDetermined,
    /// The user cannot change this application's restricted status.
    Restricted,
    /// The user explicitly denied access.
    Denied,
    /// The user granted access.
    Authorized,
}

impl From<AVAuthStatus> for AVAuthorizationStatus {
    fn from(status: AVAuthStatus) -> Self {
        match status {
            AVAuthStatus::NotDetermined => AVAuthorizationStatus::NotDetermined,
            AVAuthStatus::Restricted => AVAuthorizationStatus::Restricted,
            AVAuthStatus::Denied => AVAuthorizationStatus::Denied,
            AVAuthStatus::Authorized => AVAuthorizationStatus::Authorized,
            _ => AVAuthorizationStatus::NotDetermined,
        }
    }
}

/// Returns the current camera permission status.
pub fn authorization_status() -> AVAuthorizationStatus {
    unsafe {
        let video_type = AVMediaTypeVideo.expect("AVMediaTypeVideo should be available");
        let status = AVCaptureDevice::authorizationStatusForMediaType(video_type);
        AVAuthorizationStatus::from(status)
    }
}

/// Requests camera permission.
///
/// Returns a future that completes after the user responds.
pub async fn request_authorization() -> bool {
    use block2::StackBlock;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let block = StackBlock::new(move |granted: objc2::runtime::Bool| {
        let _ = tx.send(granted.as_bool());
    });

    unsafe {
        let video_type = AVMediaTypeVideo.expect("AVMediaTypeVideo should be available");
        AVCaptureDevice::requestAccessForMediaType_completionHandler(video_type, &block);
    }

    // Wait for the user's response.
    rx.recv().unwrap_or(false)
}

/// Returns the discovery device types.
#[allow(deprecated)]
fn get_device_types() -> Vec<&'static AVCaptureDeviceType> {
    unsafe {
        #[cfg(target_os = "macos")]
        let types = vec![
            AVCaptureDeviceTypeBuiltInWideAngleCamera,
            AVCaptureDeviceTypeExternalUnknown,
        ];

        #[cfg(target_os = "ios")]
        let types = vec![AVCaptureDeviceTypeBuiltInWideAngleCamera];

        types
    }
}

/// Queries all available camera devices.
pub fn query_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
    unsafe {
        let mut devices = Vec::new();

        // Build the device type array.
        let device_types = get_device_types();

        // Create the NSArray.
        let types_array: Retained<NSArray<AVCaptureDeviceType>> =
            NSArray::from_slice(&device_types);

        // Create the discovery session.
        let video_type = AVMediaTypeVideo.expect("AVMediaTypeVideo should be available");
        let discovery_session =
            AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &types_array,
                Some(video_type),
                AVCaptureDevicePosition::Unspecified,
            );

        let av_devices = discovery_session.devices();
        let count = av_devices.len();

        for index in 0..count {
            let device = av_devices.objectAtIndex_unchecked(index);
            let name = device.localizedName().to_string();
            let unique_id = device.uniqueID().to_string();
            let manufacturer = device.manufacturer().to_string();

            let position = device.position();
            let position_str = match position {
                AVCaptureDevicePosition::Front => "Front",
                AVCaptureDevicePosition::Back => "Back",
                _ => "Unspecified",
            };

            let description = format!("{} - {} ({})", manufacturer, name, position_str);

            devices.push(CameraDeviceInfo {
                index: index as u32,
                name,
                description,
                vendor_id: None,
                product_id: None,
                serial_number: Some(unique_id.clone()),
                device_path: Some(unique_id),
            });
        }

        Ok(devices)
    }
}

/// Returns an AVCaptureDevice by index.
pub(crate) fn get_device_by_index(index: u32) -> CameraResult<Retained<AVCaptureDevice>> {
    unsafe {
        let device_types = get_device_types();
        let types_array: Retained<NSArray<AVCaptureDeviceType>> =
            NSArray::from_slice(&device_types);

        let video_type = AVMediaTypeVideo.expect("AVMediaTypeVideo should be available");
        let discovery_session =
            AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &types_array,
                Some(video_type),
                AVCaptureDevicePosition::Unspecified,
            );

        let av_devices = discovery_session.devices();
        let count = av_devices.len();

        if (index as usize) >= count {
            return Err(CameraError::device_not_found(format!(
                "Device index {} not found",
                index
            )));
        }

        let device = av_devices.objectAtIndex_unchecked(index as usize);
        Ok(device.retain())
    }
}

/// Returns an AVCaptureDevice by unique ID.
#[allow(dead_code)]
pub(crate) fn get_device_by_id(unique_id: &str) -> CameraResult<Retained<AVCaptureDevice>> {
    unsafe {
        let ns_id = NSString::from_str(unique_id);
        AVCaptureDevice::deviceWithUniqueID(&ns_id)
            .ok_or_else(|| CameraError::device_not_found(format!("Device {} not found", unique_id)))
    }
}

/// Returns configurations supported by the device.
pub(crate) fn get_supported_configs(device: &AVCaptureDevice) -> CameraResult<Vec<CameraConfig>> {
    unsafe {
        let mut configs = Vec::new();
        let formats = device.formats();
        let count = formats.len();

        for i in 0..count {
            let format = formats.objectAtIndex_unchecked(i);
            if let Some(config) = parse_format(format) {
                // Enumerate frame-rate ranges for this format.
                let frame_rate_ranges = format.videoSupportedFrameRateRanges();
                let range_count = frame_rate_ranges.len();

                for j in 0..range_count {
                    let range = frame_rate_ranges.objectAtIndex_unchecked(j);
                    let min_fps = range.minFrameRate();
                    let max_fps = range.maxFrameRate();

                    let mut rates = Vec::new();
                    for duration in [range.minFrameDuration(), range.maxFrameDuration()] {
                        if duration.value > 0
                            && duration.value <= u32::MAX as i64
                            && duration.timescale > 0
                        {
                            rates.push(crate::FrameRate::new(
                                duration.timescale as u32,
                                duration.value as u32,
                            )?);
                        }
                    }
                    for rate in [15, 24, 25, 30, 60, 120, 240] {
                        if rate as f64 >= min_fps && rate as f64 <= max_fps {
                            rates.push(crate::FrameRate::new(rate, 1)?);
                        }
                    }
                    for rate in rates {
                        configs.push(config.clone().with_frame_rate(rate));
                    }
                }
            }
        }

        // Deduplicate while preserving distinct video formats.
        configs.sort_by(|a, b| {
            // Convert VideoFormat to a discriminant for ordering.
            let format_ord = |f: &crate::types::VideoFormat| -> u8 {
                match f {
                    crate::types::VideoFormat::MJPEG => 0,
                    crate::types::VideoFormat::YUYV => 1,
                    crate::types::VideoFormat::UYVY => 2,
                    crate::types::VideoFormat::NV12 => 3,
                    crate::types::VideoFormat::RGB => 4,
                    crate::types::VideoFormat::H264 => 5,
                    crate::types::VideoFormat::Gray => 6,
                }
            };
            (
                format_ord(&a.format),
                a.width,
                a.height,
                a.fps,
                a.fps_denominator,
            )
                .cmp(&(
                    format_ord(&b.format),
                    b.width,
                    b.height,
                    b.fps,
                    b.fps_denominator,
                ))
        });
        configs.dedup_by(|a, b| {
            a.format == b.format
                && a.width == b.width
                && a.height == b.height
                && a.frame_rate().ok() == b.frame_rate().ok()
        });

        Ok(configs)
    }
}

/// Converts an AVCaptureDeviceFormat into a CameraConfig.
fn parse_format(format: &AVCaptureDeviceFormat) -> Option<CameraConfig> {
    unsafe {
        let format_desc = format.formatDescription();

        // Read the dimensions.
        let dimensions = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&format_desc);

        let width = dimensions.width as u32;
        let height = dimensions.height as u32;

        // Read the pixel format through the Objective-C method.
        let media_subtype = format_desc.media_sub_type();

        let video_format = fourcc_to_format(media_subtype)?;

        Some(CameraConfig {
            format: video_format,
            width,
            height,
            fps_denominator: 1,
            fps: 30, // Default value updated by the caller.
        })
    }
}

/// Converts a FourCC value into a VideoFormat.
pub(crate) fn fourcc_to_format(fourcc: u32) -> Option<VideoFormat> {
    // Common FourCC values.
    // https://developer.apple.com/documentation/corevideo/cvpixelformattype
    const KCVPIXELFORMATTYPE_422YPCBCR8: u32 = 0x32767579; // '2vuy' - UYVY
    const KCVPIXELFORMATTYPE_422YPCBCR8_YUVS: u32 = 0x79757673; // 'yuvs' - YUYV
    const KCVPIXELFORMATTYPE_420YPCBCR8BIPLANARFULLRANGE: u32 = 0x34323066; // '420f' - NV12
    const KCVPIXELFORMATTYPE_420YPCBCR8BIPLANARVIDEORANGE: u32 = 0x34323076; // '420v' - NV12
    const KCVPIXELFORMATTYPE_32BGRA: u32 = 0x42475241; // 'BGRA'
    const KCVPIXELFORMATTYPE_32ARGB: u32 = 0x00000020; // 32
    const KCMVIDEOCODECTYPE_JPEG: u32 = 0x6A706567; // 'jpeg'
    const KCMVIDEOCODECTYPE_JPEG_OPENDML: u32 = 0x646D6231; // 'dmb1'

    match fourcc {
        KCVPIXELFORMATTYPE_422YPCBCR8 => Some(VideoFormat::UYVY),
        KCVPIXELFORMATTYPE_422YPCBCR8_YUVS => Some(VideoFormat::YUYV),
        KCVPIXELFORMATTYPE_420YPCBCR8BIPLANARFULLRANGE
        | KCVPIXELFORMATTYPE_420YPCBCR8BIPLANARVIDEORANGE => Some(VideoFormat::NV12),
        KCVPIXELFORMATTYPE_32BGRA | KCVPIXELFORMATTYPE_32ARGB => Some(VideoFormat::RGB),
        KCMVIDEOCODECTYPE_JPEG | KCMVIDEOCODECTYPE_JPEG_OPENDML => Some(VideoFormat::MJPEG),
        _ => {
            log::debug!("Unknown FourCC: 0x{:08X}", fourcc);
            None
        }
    }
}

fn cross_format_fallback_priority(format: VideoFormat) -> i32 {
    match format {
        // NV12 is camera-rs' cheapest Apple RGB path: AVFoundation can deliver
        // it directly and the delegate converts it with the two-row SIMD kernel.
        VideoFormat::NV12 => 110,
        VideoFormat::RGB => 100,
        VideoFormat::YUYV => 90,
        VideoFormat::UYVY => 80,
        VideoFormat::MJPEG => 70,
        _ => 0,
    }
}

/// Finds the AVCaptureDeviceFormat that best matches a configuration.
pub(crate) fn find_best_format(
    device: &AVCaptureDevice,
    config: &CameraConfig,
) -> CameraResult<Retained<AVCaptureDeviceFormat>> {
    unsafe {
        let formats = device.formats();
        let count = formats.len();

        let mut best_match: Option<Retained<AVCaptureDeviceFormat>> = None;
        let mut best_score = i32::MIN;

        log::debug!(
            "Finding best format for {:?} {}x{}@{}fps among {} formats",
            config.format,
            config.width,
            config.height,
            config.fps,
            count
        );

        for i in 0..count {
            let format = formats.objectAtIndex_unchecked(i);
            let format_desc = format.formatDescription();
            let dimensions = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&format_desc);

            let width = dimensions.width as u32;
            let height = dimensions.height as u32;

            // Resolve the format's VideoFormat value.
            let media_subtype = format_desc.media_sub_type();
            let video_format = match fourcc_to_format(media_subtype) {
                Some(f) => f,
                None => continue, // Skip unsupported formats.
            };

            // Calculate the match score.
            let mut score = 0;
            let mut fps_supported = false;

            // Strongly prefer an exact video format match.
            if video_format == config.format {
                score += 2000;
            } else {
                // Reject mismatched formats in the exact pass.
                continue;
            }

            // Strongly prefer an exact resolution match.
            if width == config.width && height == config.height {
                score += 1000;
            } else {
                // Reject mismatched resolutions in the exact pass.
                continue;
            }

            // Check frame-rate support.
            let frame_rate_ranges = format.videoSupportedFrameRateRanges();
            let range_count = frame_rate_ranges.len();

            for j in 0..range_count {
                let range = frame_rate_ranges.objectAtIndex_unchecked(j);
                let min_fps = range.minFrameRate();
                let max_fps = range.maxFrameRate();
                let requested_fps = config.frame_rate()?.as_f64();
                if fps_matches_range(requested_fps, min_fps, max_fps) {
                    fps_supported = true;
                    // Prefer an exact maximum frame-rate match.
                    if (requested_fps - max_fps).abs() <= FRAME_RATE_EPSILON {
                        score += 100;
                    } else {
                        score += 50;
                    }
                    break;
                }
            }

            // Consider only formats that support the requested frame rate.
            if !fps_supported {
                continue;
            }

            log::debug!(
                "Format candidate: {:?} {}x{} fps_supported={} score={}",
                video_format,
                width,
                height,
                fps_supported,
                score
            );

            if score > best_score {
                best_score = score;
                best_match = Some(format.retain());
            }
        }

        // If no exact match exists, find the closest resolution in the same format.
        if best_match.is_none() {
            log::warn!(
                "No exact match for {:?} {}x{}@{}fps, searching for closest format...",
                config.format,
                config.width,
                config.height,
                config.fps
            );

            for i in 0..count {
                let format = formats.objectAtIndex_unchecked(i);
                let format_desc = format.formatDescription();
                let dimensions =
                    objc2_core_media::CMVideoFormatDescriptionGetDimensions(&format_desc);

                let width = dimensions.width as u32;
                let height = dimensions.height as u32;

                // Resolve the format's VideoFormat value.
                let media_subtype = format_desc.media_sub_type();
                let video_format = match fourcc_to_format(media_subtype) {
                    Some(f) => f,
                    None => continue,
                };

                // Keep the requested video format.
                if video_format != config.format {
                    continue;
                }

                // Check frame-rate support.
                let frame_rate_ranges = format.videoSupportedFrameRateRanges();
                let range_count = frame_rate_ranges.len();

                let mut fps_supported = false;
                for j in 0..range_count {
                    let range = frame_rate_ranges.objectAtIndex_unchecked(j);
                    let min_fps = range.minFrameRate();
                    let max_fps = range.maxFrameRate();
                    if fps_matches_range(config.frame_rate()?.as_f64(), min_fps, max_fps) {
                        fps_supported = true;
                        break;
                    }
                }

                if !fps_supported {
                    continue;
                }

                // Smaller resolution differences produce better scores.
                let diff = ((width as i32 - config.width as i32).abs()
                    + (height as i32 - config.height as i32).abs())
                    as i32;
                let score = -diff; // Negative distance: smaller differences rank higher.

                if score > best_score {
                    best_score = score;
                    best_match = Some(format.retain());
                    log::debug!(
                        "Fallback format (same video format): {:?} {}x{} (diff={}, score={})",
                        video_format,
                        width,
                        height,
                        diff,
                        score
                    );
                }
            }
        }

        // If no same-format candidate exists, allow a cross-format fallback.
        // This matters for built-in macOS cameras that often expose only NV12.
        if best_match.is_none() {
            log::warn!(
                "No matching format for {:?}, trying any available format for {}x{}@{}fps...",
                config.format,
                config.width,
                config.height,
                config.fps
            );

            for i in 0..count {
                let format = formats.objectAtIndex_unchecked(i);
                let format_desc = format.formatDescription();
                let dimensions =
                    objc2_core_media::CMVideoFormatDescriptionGetDimensions(&format_desc);

                let width = dimensions.width as u32;
                let height = dimensions.height as u32;

                // Resolve the format's VideoFormat value.
                let media_subtype = format_desc.media_sub_type();
                let video_format = match fourcc_to_format(media_subtype) {
                    Some(f) => f,
                    None => continue,
                };

                // Check frame-rate support.
                let frame_rate_ranges = format.videoSupportedFrameRateRanges();
                let range_count = frame_rate_ranges.len();

                let mut fps_supported = false;
                for j in 0..range_count {
                    let range = frame_rate_ranges.objectAtIndex_unchecked(j);
                    let min_fps = range.minFrameRate();
                    let max_fps = range.maxFrameRate();
                    if fps_matches_range(config.frame_rate()?.as_f64(), min_fps, max_fps) {
                        fps_supported = true;
                        break;
                    }
                }

                if !fps_supported {
                    continue;
                }

                // Calculate resolution distance.
                let diff = ((width as i32 - config.width as i32).abs()
                    + (height as i32 - config.height as i32).abs())
                    as i32;

                // Score = format priority * 1000 - resolution distance.
                let score = cross_format_fallback_priority(video_format) * 1000 - diff;

                if score > best_score {
                    best_score = score;
                    best_match = Some(format.retain());
                    log::debug!(
                        "Cross-format fallback: {:?} {}x{} (diff={}, score={})",
                        video_format,
                        width,
                        height,
                        diff,
                        score
                    );
                }
            }
        }

        if let Some(ref format) = best_match {
            let format_desc = format.formatDescription();
            let dimensions = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&format_desc);
            let media_subtype = format_desc.media_sub_type();
            let actual_format = fourcc_to_format(media_subtype);
            log::info!(
                "Selected format: {:?} {}x{} (requested: {:?} {}x{}@{}fps)",
                actual_format,
                dimensions.width,
                dimensions.height,
                config.format,
                config.width,
                config.height,
                config.fps
            );
        }

        best_match.ok_or_else(|| {
            CameraError::unsupported_format(format!(
                "No matching format for {:?} {}x{}@{:.6}fps ({}/{})",
                config.format,
                config.width,
                config.height,
                config.frame_rate().map_or(0.0, |rate| rate.as_f64()),
                config.fps,
                config.fps_denominator
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authorization_status() {
        // Verify that the function can be called safely.
        let _status = authorization_status();
    }

    #[test]
    fn test_query_devices() {
        let result = query_devices();
        // A host without cameras may return an empty list without error.
        assert!(result.is_ok());
    }

    #[test]
    fn cross_format_fallback_prefers_directly_convertible_nv12() {
        assert!(
            cross_format_fallback_priority(VideoFormat::NV12)
                > cross_format_fallback_priority(VideoFormat::RGB)
        );
        assert!(
            cross_format_fallback_priority(VideoFormat::NV12)
                > cross_format_fallback_priority(VideoFormat::YUYV)
        );
        assert!(
            cross_format_fallback_priority(VideoFormat::NV12)
                > cross_format_fallback_priority(VideoFormat::UYVY)
        );
        assert!(
            cross_format_fallback_priority(VideoFormat::NV12)
                > cross_format_fallback_priority(VideoFormat::MJPEG)
        );
    }

    #[test]
    fn fixed_fractional_frame_rate_accepts_integer_and_reconstructed_rates() {
        let reported = 30.000_029_970_029_97;
        assert!(fps_matches_range(30.0, reported, reported));
        assert!(fps_matches_range(
            3_000_003.0 / 100_000.0,
            reported,
            reported
        ));
        assert!(!fps_matches_range(29.0, reported, reported));
    }
}
