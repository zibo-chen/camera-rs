//! AVFoundation 设备枚举和权限处理

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

/// 权限状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AVAuthorizationStatus {
    /// 用户尚未做出选择
    NotDetermined,
    /// 用户无法更改此应用程序的状态（受限）
    Restricted,
    /// 用户明确拒绝
    Denied,
    /// 用户已授权
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

/// 获取当前相机权限状态
pub fn authorization_status() -> AVAuthorizationStatus {
    unsafe {
        let video_type = AVMediaTypeVideo.expect("AVMediaTypeVideo should be available");
        let status = AVCaptureDevice::authorizationStatusForMediaType(video_type);
        AVAuthorizationStatus::from(status)
    }
}

/// 请求相机权限
///
/// 返回一个 Future，在用户做出选择后完成
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

    // 等待用户响应
    rx.recv().unwrap_or(false)
}

/// 获取设备类型列表
#[allow(deprecated)]
fn get_device_types() -> Vec<&'static AVCaptureDeviceType> {
    unsafe {
        let mut types = vec![AVCaptureDeviceTypeBuiltInWideAngleCamera];

        #[cfg(target_os = "macos")]
        types.push(AVCaptureDeviceTypeExternalUnknown);

        types
    }
}

/// 查询所有可用的摄像头设备
pub fn query_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
    unsafe {
        let mut devices = Vec::new();

        // 创建设备类型数组
        let device_types = get_device_types();

        // 创建 NSArray
        let types_array: Retained<NSArray<AVCaptureDeviceType>> =
            NSArray::from_slice(&device_types);

        // 创建 discovery session
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

/// 根据索引获取 AVCaptureDevice
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
            return Err(CameraError::DeviceNotFound(format!(
                "Device index {} not found",
                index
            )));
        }

        let device = av_devices.objectAtIndex_unchecked(index as usize);
        Ok(device.retain())
    }
}

/// 根据 unique ID 获取 AVCaptureDevice
#[allow(dead_code)]
pub(crate) fn get_device_by_id(unique_id: &str) -> CameraResult<Retained<AVCaptureDevice>> {
    unsafe {
        let ns_id = NSString::from_str(unique_id);
        AVCaptureDevice::deviceWithUniqueID(&ns_id)
            .ok_or_else(|| CameraError::DeviceNotFound(format!("Device {} not found", unique_id)))
    }
}

/// 获取设备支持的配置
pub(crate) fn get_supported_configs(device: &AVCaptureDevice) -> CameraResult<Vec<CameraConfig>> {
    unsafe {
        let mut configs = Vec::new();
        let formats = device.formats();
        let count = formats.len();

        for i in 0..count {
            let format = formats.objectAtIndex_unchecked(i);
            if let Some(config) = parse_format(format) {
                // 获取该格式支持的帧率范围
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

        // 去重 - 需要考虑视频格式
        configs.sort_by(|a, b| {
            // 将 VideoFormat 转为判别序号进行排序
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

/// 解析 AVCaptureDeviceFormat 为 CameraConfig
fn parse_format(format: &AVCaptureDeviceFormat) -> Option<CameraConfig> {
    unsafe {
        let format_desc = format.formatDescription();

        // 获取分辨率
        let dimensions = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&format_desc);

        let width = dimensions.width as u32;
        let height = dimensions.height as u32;

        // 获取像素格式 - 使用方法调用
        let media_subtype = format_desc.media_sub_type();

        let video_format = fourcc_to_format(media_subtype)?;

        Some(CameraConfig {
            format: video_format,
            width,
            height,
            fps_denominator: 1,
            fps: 30, // 默认值，会在调用者处更新
        })
    }
}

/// 将 FourCC 转换为 VideoFormat
pub(crate) fn fourcc_to_format(fourcc: u32) -> Option<VideoFormat> {
    // 常见的 FourCC 代码
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
        // Keep formats implemented by CaptureDelegate ahead of NV12. NV12 is
        // enumerated by many macOS cameras, but the current frame converter
        // cannot decode it into RGB yet.
        VideoFormat::RGB => 100,
        VideoFormat::YUYV => 90,
        VideoFormat::UYVY => 80,
        VideoFormat::MJPEG => 70,
        VideoFormat::NV12 => 10,
        _ => 0,
    }
}

/// 根据配置查找最佳匹配的 AVCaptureDeviceFormat
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

            // 获取格式的视频格式类型
            let media_subtype = format_desc.media_sub_type();
            let video_format = match fourcc_to_format(media_subtype) {
                Some(f) => f,
                None => continue, // 跳过不支持的格式
            };

            // 计算匹配分数
            let mut score = 0;
            let mut fps_supported = false;

            // 视频格式匹配得高分
            if video_format == config.format {
                score += 2000;
            } else {
                // 跳过格式不匹配的
                continue;
            }

            // 分辨率精确匹配得高分
            if width == config.width && height == config.height {
                score += 1000;
            } else {
                // 跳过分辨率不匹配的格式
                continue;
            }

            // 检查帧率支持
            let frame_rate_ranges = format.videoSupportedFrameRateRanges();
            let range_count = frame_rate_ranges.len();

            for j in 0..range_count {
                let range = frame_rate_ranges.objectAtIndex_unchecked(j);
                let min_fps = range.minFrameRate();
                let max_fps = range.maxFrameRate();
                let requested_fps = config.frame_rate()?.as_f64();
                if fps_matches_range(requested_fps, min_fps, max_fps) {
                    fps_supported = true;
                    // 精确匹配最大帧率得更高分
                    if (requested_fps - max_fps).abs() <= FRAME_RATE_EPSILON {
                        score += 100;
                    } else {
                        score += 50;
                    }
                    break;
                }
            }

            // 只有支持指定帧率的格式才考虑
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

        // 如果没有精确匹配，尝试寻找最接近的分辨率（同格式）
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

                // 获取格式的视频格式类型
                let media_subtype = format_desc.media_sub_type();
                let video_format = match fourcc_to_format(media_subtype) {
                    Some(f) => f,
                    None => continue,
                };

                // 优先匹配相同视频格式
                if video_format != config.format {
                    continue;
                }

                // 检查帧率支持
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

                // 计算分辨率差异分数（越小越好）
                let diff = ((width as i32 - config.width as i32).abs()
                    + (height as i32 - config.height as i32).abs())
                    as i32;
                let score = -diff; // 负分，差异越小分数越高

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

        // 如果仍然没有匹配，尝试使用任意可用格式（跨视频格式回退）
        // 这对于 macOS 内置摄像头很重要，因为它们通常只支持 NV12
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

                // 获取格式的视频格式类型
                let media_subtype = format_desc.media_sub_type();
                let video_format = match fourcc_to_format(media_subtype) {
                    Some(f) => f,
                    None => continue,
                };

                // 检查帧率支持
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

                // 计算分辨率差异分数
                let diff = ((width as i32 - config.width as i32).abs()
                    + (height as i32 - config.height as i32).abs())
                    as i32;

                // 分数 = 格式优先级 * 1000 - 分辨率差异
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
            CameraError::UnsupportedFormat(format!(
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
        // 只测试函数能正常调用
        let _status = authorization_status();
    }

    #[test]
    fn test_query_devices() {
        let result = query_devices();
        // 在没有摄像头的环境中可能返回空列表，但不应该出错
        assert!(result.is_ok());
    }

    #[test]
    fn cross_format_fallback_prefers_decodeable_formats_before_nv12() {
        assert!(
            cross_format_fallback_priority(VideoFormat::RGB)
                > cross_format_fallback_priority(VideoFormat::NV12)
        );
        assert!(
            cross_format_fallback_priority(VideoFormat::YUYV)
                > cross_format_fallback_priority(VideoFormat::NV12)
        );
        assert!(
            cross_format_fallback_priority(VideoFormat::UYVY)
                > cross_format_fallback_priority(VideoFormat::NV12)
        );
        assert!(
            cross_format_fallback_priority(VideoFormat::MJPEG)
                > cross_format_fallback_priority(VideoFormat::NV12)
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
