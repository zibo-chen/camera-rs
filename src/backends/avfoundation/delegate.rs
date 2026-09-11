//! AVFoundation 帧回调委托
//!
//! 实现 AVCaptureVideoDataOutputSampleBufferDelegate 协议，
//! 接收视频帧并转换为 RGB 格式。

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureOutput, AVCaptureVideoDataOutputSampleBufferDelegate,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_2020,
    kCVImageBufferYCbCrMatrix_ITU_R_601_4, kCVImageBufferYCbCrMatrix_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_SMPTE_240M_1995, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSObject, NSObjectProtocol};

pub type FrameBuffer = crate::FrameHub;
pub struct DelegateState {
    hub: Arc<FrameBuffer>,
    session: u64,
}

define_class!(
    /// AVCaptureVideoDataOutputSampleBufferDelegate 实现
    #[unsafe(super(NSObject))]
    #[ivars = DelegateState]
    pub struct CaptureDelegate;

    unsafe impl NSObjectProtocol for CaptureDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for CaptureDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn capture_output_did_output(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            handle_sample_buffer(sample_buffer, self.ivars());
        }

        #[unsafe(method(captureOutput:didDropSampleBuffer:fromConnection:))]
        fn capture_output_did_drop(
            &self,
            _output: &AVCaptureOutput,
            _sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            log::trace!("Frame dropped");
        }
    }
);

impl CaptureDelegate {
    /// 创建新的委托
    pub fn new(hub: Arc<FrameBuffer>) -> Retained<Self> {
        let session = hub.session();
        let this = Self::alloc().set_ivars(DelegateState { hub, session });
        unsafe { msg_send![super(this), init] }
    }
}

/// 处理采样缓冲区
fn handle_sample_buffer(sample_buffer: &CMSampleBuffer, state: &DelegateState) {
    // SAFETY: 这个函数需要 unsafe 因为：
    // 1. CMSampleBuffer 操作需要正确的内存管理
    // 2. CVPixelBuffer Lock/Unlock 操作需要正确配对
    // 3. 指针操作需要确保内存有效
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        handle_sample_buffer_unsafe(sample_buffer, state);
    }));
    if result.is_err() {
        log::error!("AVFoundation callback panicked; frame discarded");
    }
}

/// 处理采样缓冲区 (unsafe 实现)
unsafe fn handle_sample_buffer_unsafe(sample_buffer: &CMSampleBuffer, state: &DelegateState) {
    let frame_buffer = &state.hub;
    if !frame_buffer.is_streaming() {
        return;
    }

    // 获取像素缓冲区
    let pixel_buffer = match unsafe { sample_buffer.image_buffer() } {
        Some(pb) => pb,
        None => {
            if frame_buffer.wants_native() {
                if let (Some(block), Some(description)) = (
                    sample_buffer.data_buffer(),
                    sample_buffer.format_description(),
                ) {
                    let size =
                        objc2_core_media::CMVideoFormatDescriptionGetDimensions(&description);
                    let format = match super::device::fourcc_to_format(description.media_sub_type())
                    {
                        Some(crate::VideoFormat::MJPEG) => crate::PixelFormat::Mjpeg,
                        _ => {
                            log::warn!("Unsupported encoded AVFoundation format");
                            return;
                        }
                    };
                    let time = sample_buffer.presentation_time_stamp();
                    let timestamp = if time.flags.contains(objc2_core_media::CMTimeFlags::Valid)
                        && time.timescale > 0
                    {
                        i64::try_from(time.value as i128 * 1_000_000_000 / time.timescale as i128)
                            .ok()
                            .map(|nanoseconds| crate::SourceTimestamp {
                                nanoseconds,
                                clock: crate::ClockDomain::MediaPresentation,
                            })
                    } else {
                        None
                    };
                    let layout = crate::FrameLayout::packed(
                        size.width as u32,
                        size.height as u32,
                        format,
                        0,
                        block.data_length(),
                    );
                    if let Err(error) =
                        frame_buffer.publish_native_into(state.session, layout, timestamp, |out| {
                            let code = block.copy_data_bytes(
                                0,
                                out.len(),
                                std::ptr::NonNull::new(out.as_mut_ptr().cast()).unwrap(),
                            );
                            if code == 0 {
                                Ok(())
                            } else {
                                Err(crate::CameraError::InvalidFormat(format!(
                                    "CMBlockBuffer copy: {code}"
                                )))
                            }
                        })
                    {
                        log::warn!("Encoded AVFoundation frame rejected: {error}");
                    }
                }
            }
            return;
        }
    };

    // CVImageBuffer 和 CVPixelBuffer 是相同的类型
    let pixel_buffer: &CVPixelBuffer = &pixel_buffer;

    // 锁定像素缓冲区
    let lock_result = CVPixelBufferLockBaseAddress(pixel_buffer, CVPixelBufferLockFlags::ReadOnly);

    if lock_result != 0 {
        log::warn!("Failed to lock pixel buffer: {}", lock_result);
        return;
    }

    struct PixelLock<'a>(&'a CVPixelBuffer);
    impl Drop for PixelLock<'_> {
        fn drop(&mut self) {
            unsafe {
                CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly);
            }
        }
    }
    let _lock = PixelLock(pixel_buffer);
    // 获取像素数据
    let width = CVPixelBufferGetWidth(pixel_buffer) as u32;
    let height = CVPixelBufferGetHeight(pixel_buffer) as u32;
    let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);
    let base_address = CVPixelBufferGetBaseAddress(pixel_buffer);
    let pixel_format = CVPixelBufferGetPixelFormatType(pixel_buffer);

    let time = sample_buffer.presentation_time_stamp();
    let timestamp =
        if time.flags.contains(objc2_core_media::CMTimeFlags::Valid) && time.timescale > 0 {
            i64::try_from(time.value as i128 * 1_000_000_000 / time.timescale as i128).ok()
        } else {
            None
        };
    let timestamp = timestamp.map(|nanoseconds| crate::SourceTimestamp {
        nanoseconds,
        clock: crate::ClockDomain::MediaPresentation,
    });
    if frame_buffer.wants_native()
        && CVPixelBufferGetPlaneCount(pixel_buffer) == 2
        && matches!(pixel_format, 0x34323076 | 0x34323066)
    {
        let mut layout = crate::FrameLayout::packed(width, height, crate::PixelFormat::Nv12, 0, 0);
        layout.planes.clear();
        layout.color.range = if pixel_format == 0x34323066 {
            crate::ColorRange::Full
        } else {
            crate::ColorRange::Limited
        };
        if let Some(value) =
            pixel_buffer.attachment(kCVImageBufferYCbCrMatrixKey, std::ptr::null_mut())
        {
            for (key, matrix) in [
                (
                    kCVImageBufferYCbCrMatrix_ITU_R_601_4,
                    crate::ColorMatrix::Bt601,
                ),
                (
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                    crate::ColorMatrix::Bt709,
                ),
                (
                    kCVImageBufferYCbCrMatrix_ITU_R_2020,
                    crate::ColorMatrix::Bt2020,
                ),
                (
                    kCVImageBufferYCbCrMatrix_SMPTE_240M_1995,
                    crate::ColorMatrix::Smpte240M,
                ),
            ] {
                if *value == **key {
                    layout.color.matrix = matrix;
                    break;
                }
            }
        }
        let mut parts = Vec::new();
        let mut offset = 0usize;
        for plane in 0..2 {
            let ptr = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, plane);
            let stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, plane);
            let Some(length) = stride
                .checked_mul(CVPixelBufferGetHeightOfPlane(pixel_buffer, plane))
                .filter(|&n| n > 0 && n <= 128 * 1024 * 1024)
            else {
                return;
            };
            if ptr.is_null() {
                return;
            }
            parts.push(std::slice::from_raw_parts(ptr as *const u8, length));
            layout.planes.push(crate::PlaneLayout {
                offset,
                length,
                row_stride: stride,
                pixel_stride: if plane == 0 { 1 } else { 2 },
            });
            offset += length;
        }
        if let Err(e) = frame_buffer.publish_native(state.session, layout, timestamp, None, &parts)
        {
            log::warn!("AVFoundation native NV12: {e}");
        }
        return;
    }
    if base_address.is_null() {
        log::warn!("Null packed CVPixelBuffer");
        return;
    }
    if frame_buffer.wants_native() {
        let format = match pixel_format {
            0x42475241 => crate::PixelFormat::Bgra8,
            0x00000020 => crate::PixelFormat::Argb8,
            0x79757673 => crate::PixelFormat::Yuyv,
            0x32767579 => crate::PixelFormat::Uyvy,
            _ => {
                log::warn!("Unsupported native CVPixelBuffer format");
                return;
            }
        };
        let Some(length) = bytes_per_row.checked_mul(height as usize) else {
            return;
        };
        if length > 128 * 1024 * 1024 {
            return;
        }
        let data = std::slice::from_raw_parts(base_address as *const u8, length);
        let layout = crate::FrameLayout::packed(width, height, format, bytes_per_row, length);
        if let Err(e) = frame_buffer.publish_native(state.session, layout, timestamp, None, &[data])
        {
            log::warn!("AVFoundation native frame: {e}");
        }
        return;
    }
    let result =
        frame_buffer.publish_rgb_metadata(state.session, width, height, timestamp, None, |rgb| {
            let bytes_per_pixel = match pixel_format {
                0x42475241 | 0x00000020 => 4,
                0x79757673 | 0x32767579 => 2,
                _ => {
                    return Err(crate::CameraError::UnsupportedFormat(format!(
                        "AVFoundation pixel format {pixel_format:08x}"
                    )))
                }
            };
            if bytes_per_row < width as usize * bytes_per_pixel
                || (bytes_per_pixel == 2 && !width.is_multiple_of(2))
            {
                return Err(crate::CameraError::InvalidFormat(
                    "Invalid AVFoundation row stride/dimensions".into(),
                ));
            }
            match pixel_format {
                0x42475241 | 0x00000020 => {
                    let source_length = (height as usize - 1)
                        .checked_mul(bytes_per_row)
                        .and_then(|bytes| bytes.checked_add(width as usize * 4))
                        .ok_or_else(|| {
                            crate::CameraError::InvalidFormat(
                                "AVFoundation packed frame size overflow".into(),
                            )
                        })?;
                    let source =
                        std::slice::from_raw_parts(base_address as *const u8, source_length);
                    if pixel_format == 0x42475241 {
                        crate::utils::color_convert::bgra8888_to_rgb_into(
                            source,
                            width as usize,
                            height as usize,
                            bytes_per_row,
                            rgb,
                        )?;
                    } else {
                        crate::utils::color_convert::argb8888_to_rgb_into(
                            source,
                            width as usize,
                            height as usize,
                            bytes_per_row,
                            rgb,
                        )?;
                    }
                }
                0x79757673 => {
                    yuyv_to_rgb(base_address as *const u8, rgb, width, height, bytes_per_row)
                }
                0x32767579 => {
                    uyvy_to_rgb(base_address as *const u8, rgb, width, height, bytes_per_row)
                }
                _ => unreachable!(),
            }
            Ok(())
        });
    if let Err(error) = result {
        log::warn!("AVFoundation frame rejected: {}", error);
    }
}

/// YUYV 转 RGB
fn yuyv_to_rgb(src: *const u8, dst: &mut [u8], width: u32, height: u32, bytes_per_row: usize) {
    for y in 0..height as usize {
        for x in (0..width as usize).step_by(2) {
            let src_offset = y * bytes_per_row + x * 2;
            let dst_offset = (y * width as usize + x) * 3;

            unsafe {
                let y0 = *src.add(src_offset) as i32;
                let u = *src.add(src_offset + 1) as i32;
                let y1 = *src.add(src_offset + 2) as i32;
                let v = *src.add(src_offset + 3) as i32;

                // 第一个像素
                let (r, g, b) = yuv_to_rgb(y0, u, v);
                dst[dst_offset] = r;
                dst[dst_offset + 1] = g;
                dst[dst_offset + 2] = b;

                // 第二个像素
                if x + 1 < width as usize {
                    let (r, g, b) = yuv_to_rgb(y1, u, v);
                    dst[dst_offset + 3] = r;
                    dst[dst_offset + 4] = g;
                    dst[dst_offset + 5] = b;
                }
            }
        }
    }
}

/// UYVY 转 RGB
fn uyvy_to_rgb(src: *const u8, dst: &mut [u8], width: u32, height: u32, bytes_per_row: usize) {
    for y in 0..height as usize {
        for x in (0..width as usize).step_by(2) {
            let src_offset = y * bytes_per_row + x * 2;
            let dst_offset = (y * width as usize + x) * 3;

            unsafe {
                let u = *src.add(src_offset) as i32;
                let y0 = *src.add(src_offset + 1) as i32;
                let v = *src.add(src_offset + 2) as i32;
                let y1 = *src.add(src_offset + 3) as i32;

                // 第一个像素
                let (r, g, b) = yuv_to_rgb(y0, u, v);
                dst[dst_offset] = r;
                dst[dst_offset + 1] = g;
                dst[dst_offset + 2] = b;

                // 第二个像素
                if x + 1 < width as usize {
                    let (r, g, b) = yuv_to_rgb(y1, u, v);
                    dst[dst_offset + 3] = r;
                    dst[dst_offset + 4] = g;
                    dst[dst_offset + 5] = b;
                }
            }
        }
    }
}

/// YUV 转 RGB
#[inline]
fn yuv_to_rgb(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    let c = y - 16;
    let d = u - 128;
    let e = v - 128;

    let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
    let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
    let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;

    (r, g, b)
}

unsafe fn publish_encoded(
    sample: &CMSampleBuffer,
    state: &DelegateState,
) -> crate::CameraResult<()> {
    let block = sample
        .data_buffer()
        .ok_or(crate::CameraError::BufferEmpty)?;
    let desc = sample
        .format_description()
        .ok_or(crate::CameraError::BufferEmpty)?;
    let dimensions = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&desc);
    let format = match desc.media_sub_type() {
        0x6a706567 => crate::PixelFormat::Mjpeg,
        0x61766331 => crate::PixelFormat::H264,
        _ => {
            return Err(crate::CameraError::UnsupportedFormat(
                "Encoded AVFoundation subtype".into(),
            ))
        }
    };
    let time = sample.presentation_time_stamp();
    let timestamp =
        if time.flags.contains(objc2_core_media::CMTimeFlags::Valid) && time.timescale > 0 {
            i64::try_from(time.value as i128 * 1_000_000_000 / time.timescale as i128)
                .ok()
                .map(|nanoseconds| crate::SourceTimestamp {
                    nanoseconds,
                    clock: crate::ClockDomain::MediaPresentation,
                })
        } else {
            None
        };
    let layout = crate::FrameLayout::packed(
        dimensions.width as u32,
        dimensions.height as u32,
        format,
        0,
        block.data_length(),
    );
    state
        .hub
        .publish_native_into(state.session, layout, timestamp, |bytes| {
            let ptr = std::ptr::NonNull::new(bytes.as_mut_ptr().cast())
                .ok_or(crate::CameraError::BufferEmpty)?;
            let status = block.copy_data_bytes(0, bytes.len(), ptr);
            if status != 0 {
                return Err(crate::CameraError::InvalidFormat(format!(
                    "CMBlockBuffer copy: {status}"
                )));
            }
            Ok(())
        })?;
    Ok(())
}
