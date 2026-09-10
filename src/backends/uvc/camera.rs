//! UVC 摄像头实现

use super::context::{StreamCtrl, UvcContext, UvcDeviceHandle};
use super::ffi;
use crate::error::{CameraError, Result};
use crate::pixels::Pixels as Array3;
use crate::traits::{CameraControl, CameraManager, StreamStats, StreamingCamera};
use crate::types::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraResult, VideoFormat,
};
use crate::utils::color_convert::ColorConverter;
use parking_lot::Mutex;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;
use turbojpeg::{Decompressor, Image, PixelFormat};

struct CaptureState {
    hub: crate::FrameHub,
    decoder: Mutex<Option<Decompressor>>,
    converter: ColorConverter,
}

// Owned by the camera until uvc_stop_streaming has joined the callback thread.
// This owns only capture state, never a reference back to the camera.
struct CallbackContext {
    state: Arc<CaptureState>,
    session: u64,
}

pub struct UvcCamera {
    device_index: u32,
    expected_id: Option<String>,
    context: Mutex<Option<UvcContext>>,
    device_handle: Mutex<Option<UvcDeviceHandle>>,
    stream_ctrl: Mutex<Option<StreamCtrl>>,
    current_config: Mutex<Option<CameraConfig>>,
    capture: Arc<CaptureState>,
    callback: Mutex<Option<Box<CallbackContext>>>,
    lifecycle: Mutex<()>,
}

impl UvcCamera {
    /// 创建新的 UVC 摄像头实例
    pub fn new(device_index: u32) -> Result<Self> {
        Self::new_with_hub(device_index, crate::FrameHub::default())
    }
    pub(crate) fn new_with_hub(device_index: u32, hub: crate::FrameHub) -> Result<Self> {
        Self::new_selected(device_index, hub, None)
    }
    pub(crate) fn new_selected(
        device_index: u32,
        hub: crate::FrameHub,
        expected: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            device_index,
            expected_id: expected.map(str::to_owned),
            context: Mutex::new(None),
            device_handle: Mutex::new(None),
            stream_ctrl: Mutex::new(None),
            current_config: Mutex::new(None),
            capture: Arc::new(CaptureState {
                hub,
                decoder: Mutex::new(None),
                converter: ColorConverter::new(),
            }),
            callback: Mutex::new(None),
            lifecycle: Mutex::new(()),
        })
    }

    pub(crate) fn exposure_modes(&self) -> CameraResult<Vec<crate::ControlMode>> {
        let handle = self.device_handle.lock();
        let handle = handle.as_ref().ok_or(CameraError::StreamStopped)?;
        let mut mask = 0;
        let code = unsafe {
            ffi::uvc_get_ae_mode(handle.as_ptr(), &mut mask, ffi::UvcReqCode::GetRes as u8)
        };
        if code != ffi::UVC_SUCCESS {
            return Err(CameraError::UvcError {
                code,
                message: "Query exposure modes".into(),
            });
        }
        let mut modes = Vec::new();
        if mask & 1 != 0 {
            modes.push(crate::ControlMode::Manual);
        }
        if mask & 14 != 0 {
            modes.push(crate::ControlMode::Automatic);
        }
        Ok(modes)
    }
    /// 从文件描述符创建(Android 使用)
    ///
    /// 在 Android 上，应用程序无法直接访问 USB 设备节点。
    /// 需要通过 UsbManager.openDevice() 获取 UsbDeviceConnection，
    /// 然后调用 getFileDescriptor() 获取文件描述符传递给此方法。
    #[cfg(unix)]
    pub fn from_fd(fd: i32) -> Result<Self> {
        log::info!("Creating UvcCamera from file descriptor: {}", fd);

        let camera = Self::new(0)?;
        let mut ctx = UvcContext::new()?;
        let devh = ctx.wrap_device(fd)?;

        // 查询并打印设备支持的格式
        Self::log_supported_formats(devh.as_ptr());

        *camera.context.lock() = Some(ctx);
        *camera.device_handle.lock() = Some(devh);

        log::info!("UvcCamera created successfully from fd");
        Ok(camera)
    }

    /// 打印设备支持的所有格式
    fn log_supported_formats(devh: *mut ffi::UvcDeviceHandle) {
        log::debug!(">>> Querying device supported formats...");
        unsafe {
            let format_desc_ptr = ffi::uvc_get_format_descs(devh);
            log::debug!(">>> format_desc_ptr is_null: {}", format_desc_ptr.is_null());

            if format_desc_ptr.is_null() {
                log::debug!(">>> No format descriptors found (NULL pointer)");
                return;
            }

            log::debug!("=== Device Supported Formats ===");
            let mut format_idx = 0;
            let mut current_format = format_desc_ptr;

            while !current_format.is_null() {
                let format = &*current_format;
                let subtype = format.b_descriptor_subtype;
                let format_name = match subtype as u8 {
                    0x04 => "UNCOMPRESSED (YUYV/NV12)",
                    0x06 => "MJPEG",
                    0x10 => "FRAME_BASED (H264)",
                    _ => "UNKNOWN",
                };

                log::debug!(
                    "Format[{}]: subtype=0x{:02x} ({}), guid={:?}",
                    format_idx,
                    subtype,
                    format_name,
                    format.guid_format
                );

                // 遍历帧描述符
                let mut frame_idx = 0;
                let mut current_frame = format.frame_descs;

                while !current_frame.is_null() {
                    let frame = &*current_frame;
                    let width = frame.w_width;
                    let height = frame.w_height;
                    let frame_interval = frame.dw_default_frame_interval;

                    let fps = 10_000_000u32.checked_div(frame_interval).unwrap_or(0);

                    log::debug!(
                        "  Frame[{}]: {}x{} @ {}fps (interval={})",
                        frame_idx,
                        width,
                        height,
                        fps,
                        frame_interval
                    );

                    frame_idx += 1;
                    current_frame = frame.next;
                }

                format_idx += 1;
                current_format = format.next;
            }
            log::debug!("=== End of Supported Formats ===");
        }
    }

    /// Compatibility shim; callbacks no longer require an Arc<UvcCamera>.
    pub fn set_self_ref(self: &Arc<Self>) {}

    pub fn cleanup(&self) {
        let _lifecycle = self.lifecycle.lock();
        self.stop_locked();
        *self.device_handle.lock() = None;
        *self.context.lock() = None;
    }

    fn stop_locked(&self) {
        self.capture.hub.stop();
        if let Some(devh) = self.device_handle.lock().as_mut() {
            devh.stop_streaming();
        }
        // libuvc synchronously cancels transfers and joins its callback thread.
        self.callback.lock().take();
        *self.stream_ctrl.lock() = None;
        *self.capture.decoder.lock() = None;
    }

    extern "C" fn frame_callback(frame: *mut ffi::UvcFrame, user_ptr: *mut c_void) {
        if frame.is_null() || user_ptr.is_null() {
            return;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            let context = &*(user_ptr as *const CallbackContext);
            let frame = &*frame;
            if frame.data.is_null() || frame.data_bytes == 0 {
                return Ok(false);
            }
            let data = std::slice::from_raw_parts(frame.data as *const u8, frame.data_bytes);
            if context.state.hub.wants_native() {
                let format = match frame.frame_format {
                    ffi::UvcFrameFormat::Mjpeg => crate::PixelFormat::Mjpeg,
                    ffi::UvcFrameFormat::H264 => crate::PixelFormat::H264,
                    ffi::UvcFrameFormat::Yuyv => crate::PixelFormat::Yuyv,
                    ffi::UvcFrameFormat::Uyvy => crate::PixelFormat::Uyvy,
                    ffi::UvcFrameFormat::Rgb => crate::PixelFormat::Rgb8,
                    ffi::UvcFrameFormat::Bgr => crate::PixelFormat::Bgr8,
                    ffi::UvcFrameFormat::Gray8 => crate::PixelFormat::Gray8,
                    _ => {
                        return Err(CameraError::UnsupportedFormat(
                            "Unsupported UVC native layout".into(),
                        ))
                    }
                };
                let stride = if frame.step > 0 {
                    frame.step
                } else {
                    frame.width as usize
                        * match format {
                            crate::PixelFormat::Rgb8 | crate::PixelFormat::Bgr8 => 3,
                            crate::PixelFormat::Yuyv | crate::PixelFormat::Uyvy => 2,
                            _ => 1,
                        }
                };
                let layout = crate::FrameLayout::packed(
                    frame.width,
                    frame.height,
                    format,
                    stride,
                    data.len(),
                );
                return context.state.hub.publish_native(
                    context.session,
                    layout,
                    None,
                    Some(frame.sequence as u64),
                    &[data],
                );
            }
            context.state.hub.publish_rgb_metadata(
                context.session,
                frame.width,
                frame.height,
                None,
                Some(frame.sequence as u64),
                |rgb| match frame.frame_format {
                    ffi::UvcFrameFormat::Mjpeg => {
                        let mut guard = context.state.decoder.lock();
                        if guard.is_none() {
                            *guard = Some(
                                Decompressor::new()
                                    .map_err(|e| CameraError::Other(e.to_string()))?,
                            );
                        }
                        let decoder = guard.as_mut().unwrap();
                        let header = decoder
                            .read_header(data)
                            .map_err(|e| CameraError::InvalidFormat(e.to_string()))?;
                        if header.width != frame.width as usize
                            || header.height != frame.height as usize
                        {
                            return Err(CameraError::InvalidFormat(
                                "MJPEG dimensions differ from negotiated frame".into(),
                            ));
                        }
                        decoder
                            .decompress(
                                data,
                                Image {
                                    pixels: rgb,
                                    width: header.width,
                                    height: header.height,
                                    pitch: header.width * 3,
                                    format: PixelFormat::RGB,
                                },
                            )
                            .map_err(|e| CameraError::InvalidFormat(e.to_string()))
                    }
                    ffi::UvcFrameFormat::Yuyv | ffi::UvcFrameFormat::Uyvy => {
                        let width = frame.width as usize;
                        let packed = width.checked_mul(2).ok_or_else(|| {
                            CameraError::InvalidFormat("UVC row size overflow".into())
                        })?;
                        let stride = if frame.step == 0 { packed } else { frame.step };
                        if stride < packed {
                            return Err(CameraError::InvalidFormat(
                                "UVC row stride is too short".into(),
                            ));
                        }
                        for (y, row) in rgb.chunks_exact_mut(width * 3).enumerate() {
                            let offset = y.checked_mul(stride).ok_or_else(|| {
                                CameraError::InvalidFormat("UVC stride overflow".into())
                            })?;
                            let src = data.get(offset..offset.saturating_add(packed)).ok_or_else(
                                || CameraError::InvalidFormat("Truncated UVC row".into()),
                            )?;
                            if frame.frame_format == ffi::UvcFrameFormat::Yuyv {
                                context.state.converter.yuyv_to_rgb_into(
                                    src,
                                    frame.width,
                                    1,
                                    row,
                                )?;
                            } else {
                                context.state.converter.uyvy_to_rgb_into(
                                    src,
                                    frame.width,
                                    1,
                                    row,
                                )?;
                            }
                        }
                        Ok(())
                    }
                    ffi::UvcFrameFormat::Rgb
                    | ffi::UvcFrameFormat::Bgr
                    | ffi::UvcFrameFormat::Gray8 => {
                        let channels = if frame.frame_format == ffi::UvcFrameFormat::Gray8 {
                            1
                        } else {
                            3
                        };
                        let packed = frame.width as usize * channels;
                        let stride = if frame.step == 0 { packed } else { frame.step };
                        if stride < packed {
                            return Err(CameraError::InvalidFormat("UVC stride too short".into()));
                        }
                        for (y, row) in rgb.chunks_exact_mut(frame.width as usize * 3).enumerate() {
                            let offset = y.checked_mul(stride).ok_or_else(|| {
                                CameraError::InvalidFormat("UVC row overflow".into())
                            })?;
                            let src = data.get(offset..offset.saturating_add(packed)).ok_or_else(
                                || CameraError::InvalidFormat("Truncated UVC row".into()),
                            )?;
                            if channels == 1 {
                                for (out, &gray) in row.as_chunks_mut::<3>().0.iter_mut().zip(src) {
                                    out.fill(gray);
                                }
                            } else if frame.frame_format == ffi::UvcFrameFormat::Rgb {
                                row.copy_from_slice(src);
                            } else {
                                for (out, input) in row
                                    .as_chunks_mut::<3>()
                                    .0
                                    .iter_mut()
                                    .zip(src.as_chunks::<3>().0.iter())
                                {
                                    out.copy_from_slice(&[input[2], input[1], input[0]]);
                                }
                            }
                        }
                        Ok(())
                    }
                    _ => Err(CameraError::UnsupportedFormat(format!(
                        "{:?}",
                        frame.frame_format
                    ))),
                },
            )
        }));
        match result {
            Ok(Err(error)) => log::warn!("UVC frame rejected: {}", error),
            Err(_) => log::error!("UVC callback panicked; frame discarded"),
            _ => {}
        }
    }

    /// 使用 turbojpeg 解码 MJPEG（兼容旧 API）
    #[allow(dead_code)]
    fn decode_mjpeg(jpeg_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        let mut decompressor = Decompressor::new().map_err(|e| {
            CameraError::Other(format!("Failed to create JPEG decompressor: {}", e))
        })?;

        // 读取 JPEG 头
        let header = decompressor
            .read_header(jpeg_data)
            .map_err(|e| CameraError::Other(format!("Failed to read JPEG header: {}", e)))?;

        // 验证尺寸
        if header.width != width as usize || header.height != height as usize {
            log::warn!(
                "JPEG size mismatch: expected {}x{}, actual {}x{}",
                width,
                height,
                header.width,
                header.height
            );
        }

        // 准备输出缓冲区
        let mut image = Image {
            pixels: vec![0; 3 * header.width * header.height],
            width: header.width,
            pitch: 3 * header.width,
            height: header.height,
            format: PixelFormat::RGB,
        };

        // 解压缩
        decompressor
            .decompress(jpeg_data, image.as_deref_mut())
            .map_err(|e| CameraError::Other(format!("JPEG decompression failed: {}", e)))?;

        Ok(image.pixels)
    }

    /// YUYV 转 RGB（兼容旧 API）
    #[allow(dead_code)]
    fn yuyv_to_rgb(yuyv_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        // 使用优化的颜色转换器
        ColorConverter::new().yuyv_to_rgb(yuyv_data, width, height)
    }

    /// UYVY 转 RGB（兼容旧 API）
    #[allow(dead_code)]
    fn uyvy_to_rgb(uyvy_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        // 使用优化的颜色转换器
        ColorConverter::new().uyvy_to_rgb(uyvy_data, width, height)
    }

    /// 转换格式枚举
    fn format_to_uvc(format: VideoFormat) -> ffi::UvcFrameFormat {
        match format {
            VideoFormat::MJPEG => ffi::UvcFrameFormat::Mjpeg,
            VideoFormat::YUYV => ffi::UvcFrameFormat::Yuyv,
            VideoFormat::UYVY => ffi::UvcFrameFormat::Uyvy,
            VideoFormat::RGB => ffi::UvcFrameFormat::Rgb,
            VideoFormat::H264 => ffi::UvcFrameFormat::H264,
            VideoFormat::Gray => ffi::UvcFrameFormat::Gray8,
            VideoFormat::NV12 => ffi::UvcFrameFormat::Nv12,
        }
    }
}

impl CameraManager for UvcCamera {
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
        #[cfg(target_os = "android")]
        {
            Err(CameraError::PermissionDenied(
                "Enumerate USB devices with camera-android and pass an authorized BorrowedFd"
                    .into(),
            ))
        }

        // 非 Android 平台: 使用 libuvc 扫描
        #[cfg(not(target_os = "android"))]
        {
            let mut ctx = UvcContext::new()?;
            let devices = ctx.get_device_list()?;

            let mut device_infos = Vec::new();
            for (index, device) in devices.iter().enumerate() {
                match device.to_device_info(index as u32) {
                    Ok(info) => device_infos.push(info),
                    Err(e) => log::warn!("Failed to get device {} info: {}", index, e),
                }
            }

            // CRITICAL: 确保 devices 在 context 之前被 drop
            // 避免 "device still referenced at libusb_exit" 错误
            drop(devices);
            drop(ctx);

            Ok(device_infos)
        }
    }

    fn get_supported_configs(device_index: u32) -> CameraResult<Vec<CameraConfig>> {
        // 打开设备并查询支持的格式
        let mut ctx = UvcContext::new()?;
        let devices = ctx.get_device_list()?;

        let device = devices.get(device_index as usize).ok_or_else(|| {
            CameraError::DeviceNotFound(format!("Device index {} not found", device_index))
        })?;

        let devh = device.open()?;

        // 获取格式描述符
        let mut configs = Vec::new();
        unsafe {
            let format_desc_ptr = ffi::uvc_get_format_descs(devh.as_ptr());

            if !format_desc_ptr.is_null() {
                let mut current_format = format_desc_ptr;

                while !current_format.is_null() {
                    let format = &*current_format;

                    // 判断格式类型
                    // 注意：b_descriptor_subtype 是 u32，但我们只需要低 8 位
                    let video_format = Self::descriptor_format(
                        format.b_descriptor_subtype as u8,
                        &format.guid_format,
                    );

                    if let Some(vf) = video_format {
                        // 遍历帧描述符
                        let mut current_frame = format.frame_descs;

                        while !current_frame.is_null() {
                            let frame = &*current_frame;

                            let width = frame.w_width as u32;
                            let height = frame.w_height as u32;
                            for fps in Self::descriptor_frame_rates(frame) {
                                if width > 0 && height > 0 {
                                    let candidate =
                                        CameraConfig::new(vf, width, height, fps.numerator())
                                            .with_frame_rate(fps);
                                    if !configs.contains(&candidate) {
                                        configs.push(candidate);
                                    }
                                }
                            }

                            current_frame = frame.next;
                        }
                    }

                    current_format = format.next;
                }
            }
        }

        // 清理
        drop(devh);
        drop(devices);
        drop(ctx);

        // An empty descriptor list is unknown capability, not a list of
        // hard-coded resolutions falsely advertised as supported by hardware.
        Ok(configs)
    }

    fn is_config_supported(device_index: u32, config: &CameraConfig) -> CameraResult<bool> {
        Ok(Self::get_supported_configs(device_index)?.contains(config))
    }
}

impl UvcCamera {
    fn descriptor_format(subtype: u8, guid: &[u8; 16]) -> Option<VideoFormat> {
        if subtype == 0x06 {
            return Some(VideoFormat::MJPEG);
        }
        if subtype != 0x04 {
            return None;
        }
        // UVC uncompressed GUIDs use the standard media subtype suffix.
        if guid[4..] != [0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113] {
            return None;
        }
        match &guid[..4] {
            b"YUY2" | b"YUYV" => Some(VideoFormat::YUYV),
            b"UYVY" => Some(VideoFormat::UYVY),
            _ => None,
        }
    }

    // libuvc owns these arrays and reports their discrete length in the
    // descriptor; continuous intervals are enumerated as representable integer FPS.
    unsafe fn descriptor_frame_rates(frame: &ffi::UvcFrameDesc) -> Vec<crate::FrameRate> {
        let mut intervals = vec![frame.dw_default_frame_interval];
        if frame.b_frame_interval_type > 0 && !frame.intervals.is_null() {
            intervals.extend_from_slice(std::slice::from_raw_parts(
                frame.intervals,
                frame.b_frame_interval_type as usize,
            ));
        } else if frame.b_frame_interval_type == 0 {
            intervals.extend([frame.dw_min_frame_interval, frame.dw_max_frame_interval]);
            for fps in 1..=240 {
                let interval = 10_000_000 / fps;
                if interval >= frame.dw_min_frame_interval
                    && interval <= frame.dw_max_frame_interval
                    && (frame.dw_frame_interval_step == 0
                        || (interval - frame.dw_min_frame_interval)
                            .is_multiple_of(frame.dw_frame_interval_step))
                {
                    intervals.push(interval);
                }
            }
        }
        let mut rates: Vec<_> = intervals
            .into_iter()
            .filter(|i| *i > 0)
            .filter_map(|i| crate::FrameRate::new(10_000_000, i).ok())
            .collect();
        rates.sort_unstable_by_key(|r| (r.numerator(), r.denominator()));
        rates.dedup();
        rates
    }

    /// 通过设备唯一ID查找设备索引
    ///
    /// 设备唯一ID格式为 "vendor_id:product_id:serial_number" 或 "vendor_id:product_id:index"
    ///
    /// # 参数
    /// - `unique_id`: 设备唯一标识符
    ///
    /// # 返回
    /// 如果找到匹配的设备，返回其索引和设备信息
    pub fn find_device_by_unique_id(unique_id: &str) -> CameraResult<(u32, CameraDeviceInfo)> {
        let devices = Self::list_devices()?;

        // 首先尝试精确匹配
        for device in &devices {
            if device.unique_id() == unique_id {
                log::info!(
                    "Found exact match for unique_id {}: device {}",
                    unique_id,
                    device.index
                );
                return Ok((device.index, device.clone()));
            }
        }

        // 如果精确匹配失败，尝试解析 unique_id 并进行部分匹配
        // 格式: "vendor_id:product_id:serial_or_index"
        let parts: Vec<&str> = unique_id.split(':').collect();
        if parts.len() >= 2 {
            let target_vid = u16::from_str_radix(parts[0], 16).ok();
            let target_pid = u16::from_str_radix(parts[1], 16).ok();

            if let (Some(vid), Some(pid)) = (target_vid, target_pid) {
                // 查找具有相同 VID:PID 的设备
                let matching_devices: Vec<_> = devices
                    .iter()
                    .filter(|d| d.vendor_id == Some(vid) && d.product_id == Some(pid))
                    .collect();

                if matching_devices.len() == 1 {
                    // 只有一个匹配的设备，直接返回
                    let device = matching_devices[0];
                    log::info!(
                        "Found single device with VID:PID {:04x}:{:04x}: device {}",
                        vid,
                        pid,
                        device.index
                    );
                    return Ok((device.index, device.clone()));
                } else if !matching_devices.is_empty() {
                    // 有多个匹配的设备，尝试通过序列号匹配
                    if parts.len() >= 3 {
                        let target_serial = parts[2];
                        for device in &matching_devices {
                            if let Some(ref serial) = device.serial_number {
                                if serial == target_serial {
                                    log::info!(
                                        "Found device with matching serial {}: device {}",
                                        target_serial,
                                        device.index
                                    );
                                    return Ok((device.index, (*device).clone()));
                                }
                            }
                        }
                    }

                    // 无法确定是哪个设备，返回错误
                    log::warn!(
                        "Found {} devices with VID:PID {:04x}:{:04x}, but cannot match unique_id {}",
                        matching_devices.len(), vid, pid, unique_id
                    );
                }
            }
        }

        Err(CameraError::DeviceNotFound(format!(
            "No device found with unique_id: {}",
            unique_id
        )))
    }

    /// 通过设备唯一ID或索引打开摄像头
    ///
    /// # 参数
    /// - `unique_id`: 可选的设备唯一ID。如果为 None，则使用设备索引
    /// - `device_index`: 当 unique_id 为 None 时使用的设备索引
    ///
    /// # 返回
    /// 返回摄像头实例和设备信息
    pub fn open_by_unique_id_or_index(
        unique_id: Option<&str>,
        device_index: u32,
    ) -> CameraResult<(Self, CameraDeviceInfo)> {
        let (actual_index, device_info) = if let Some(id) = unique_id {
            // 通过唯一ID查找设备
            log::info!("Opening camera by unique_id: {}", id);
            Self::find_device_by_unique_id(id)?
        } else {
            // 通过索引查找设备
            log::info!("Opening camera by index: {}", device_index);
            let devices = Self::list_devices()?;
            let device = devices
                .into_iter()
                .find(|d| d.index == device_index)
                .ok_or_else(|| {
                    CameraError::DeviceNotFound(format!("Device index {} not found", device_index))
                })?;
            (device_index, device)
        };

        let camera = Self::new(actual_index)?;
        Ok((camera, device_info))
    }

    pub async fn start_stream_arc(self: &Arc<Self>, config: CameraConfig) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || camera.start_sync(config))
            .await
            .map_err(|e| CameraError::Other(e.to_string()))?
    }

    fn start_sync(&self, mut config: CameraConfig) -> CameraResult<()> {
        let _lifecycle = self.lifecycle.lock();
        config.validate()?;

        if self.capture.hub.is_streaming() {
            return Err(CameraError::StreamError(
                "Stream already running".to_string(),
            ));
        }

        // 初始化上下文和设备
        {
            let mut ctx_guard = self.context.lock();
            if ctx_guard.is_none() {
                let mut ctx = UvcContext::new()?;
                let devices = ctx.get_device_list()?;

                let selected = if let Some(expected) = &self.expected_id {
                    let mut matches = devices.iter().enumerate().filter_map(|(i, d)| {
                        d.to_device_info(i as u32)
                            .ok()
                            .filter(|info| info.unique_id() == *expected)
                            .map(|_| d)
                    });
                    let found = matches
                        .next()
                        .ok_or_else(|| CameraError::DeviceNotFound(expected.clone()))?;
                    if matches.next().is_some() {
                        return Err(CameraError::AmbiguousDevice(expected.clone()));
                    }
                    Some(found)
                } else {
                    devices.get(self.device_index as usize)
                };
                let device = selected.ok_or_else(|| {
                    CameraError::DeviceNotFound(format!(
                        "Device index {} does not exist",
                        self.device_index
                    ))
                })?;

                let devh = device.open()?;

                // CRITICAL: 在保存 context 之前显式 drop devices
                // 避免 "device still referenced at libusb_exit" 错误
                drop(devices);

                *self.device_handle.lock() = Some(devh);
                *ctx_guard = Some(ctx);
            }
        }

        // 配置流
        let mut devh_guard = self.device_handle.lock();
        let devh = devh_guard
            .as_mut()
            .ok_or_else(|| CameraError::DeviceOpenFailed("Device not open".to_string()))?;

        // 打印设备支持的格式（用于调试）
        Self::log_supported_formats(devh.as_ptr());

        let uvc_format = Self::format_to_uvc(config.format);
        log::info!(
            "Requesting stream: {:?} {}x{} @ {}fps",
            config.format,
            config.width,
            config.height,
            config.fps
        );

        // Negotiation/fallback belongs to the session facade. libuvc's integer
        // selector uses zero for any rate; set the exact descriptor interval before probe.
        let mut ctrl =
            devh.get_stream_ctrl(uvc_format, config.width as i32, config.height as i32, 0)?;
        let interval = (10_000_000u64 * config.fps_denominator as u64 + config.fps as u64 / 2)
            / config.fps as u64;
        ctrl.set_frame_interval(
            u32::try_from(interval)
                .map_err(|_| CameraError::InvalidConfig("UVC frame interval overflow".into()))?,
        );
        if ctrl.frame_interval() == 0 {
            return Err(CameraError::InvalidConfig(
                "UVC frame interval is zero".into(),
            ));
        }
        devh.probe_stream_ctrl(&mut ctrl)?;
        config = config.with_frame_rate(crate::FrameRate::new(10_000_000, ctrl.frame_interval())?);

        let session = self.capture.hub.start();
        let mut context = Box::new(CallbackContext {
            state: self.capture.clone(),
            session,
        });
        let user_ptr = context.as_mut() as *mut CallbackContext as *mut c_void;
        if let Err(error) =
            unsafe { devh.start_streaming(&mut ctrl, Self::frame_callback, user_ptr) }
        {
            self.capture.hub.stop();
            devh.stop_streaming();
            return Err(error);
        }
        *self.callback.lock() = Some(context);
        *self.stream_ctrl.lock() = Some(ctrl);

        log::info!(
            "Stream started: {}x{}@{}fps",
            config.width,
            config.height,
            config.fps
        );
        *self.current_config.lock() = Some(config);

        Ok(())
    }

    pub async fn stop_stream_arc(self: &Arc<Self>) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = camera.lifecycle.lock();
            camera.stop_locked();
        })
        .await
        .map_err(|e| CameraError::Other(e.to_string()))
    }
}

impl StreamingCamera for UvcCamera {
    fn frame_hub(&self) -> Option<&crate::FrameHub> {
        Some(&self.capture.hub)
    }
    async fn start_stream(&self, config: CameraConfig) -> CameraResult<()> {
        self.start_sync(config)
    }
    async fn stop_stream(&self) -> CameraResult<()> {
        let _guard = self.lifecycle.lock();
        self.stop_locked();
        Ok(())
    }
    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>> {
        Ok(self.capture.hub.latest().map(|f| {
            f.rgb_pixels()
                .expect("RGB backend adapter")
                .as_ref()
                .clone()
        }))
    }
    async fn wait_for_frame(&self, timeout: Duration) -> CameraResult<Array3<u8>> {
        Ok((*self
            .capture
            .hub
            .wait_after(None, timeout)
            .await?
            .rgb_pixels()?)
        .as_ref()
        .clone())
    }
    fn is_streaming(&self) -> bool {
        self.capture.hub.is_streaming()
    }
    fn get_config(&self) -> Option<CameraConfig> {
        self.current_config.lock().clone()
    }
    fn get_stats(&self) -> StreamStats {
        self.capture.hub.stats()
    }
    async fn set_buffer_size(&self, _size: usize) -> CameraResult<()> {
        Err(CameraError::InvalidConfig(
            "Frame pool capacity is fixed for the lifetime of the camera".into(),
        ))
    }
}

// ============================================================================
// 摄像头控制参数实现
// ============================================================================

impl CameraControl for UvcCamera {
    fn get_control(&self, control: CameraControlType) -> CameraResult<CameraControlValue> {
        let devh = self.device_handle.lock();
        let devh = devh
            .as_ref()
            .ok_or_else(|| CameraError::DeviceOpenFailed("Device not open".to_string()))?
            .as_ptr();

        use ffi::UvcReqCode;
        let req_code = UvcReqCode::GetCur as u8;

        match control {
            CameraControlType::AutoExposure => {
                let mut mode = 0u8;
                let result = unsafe { ffi::uvc_get_ae_mode(devh, &mut mode, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get auto exposure mode".to_string(),
                    });
                }
                Ok(CameraControlValue::Boolean(matches!(mode, 2 | 4 | 8)))
            }
            CameraControlType::Exposure => {
                let mut time = 0u32;
                let result = unsafe { ffi::uvc_get_exposure_abs(devh, &mut time, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get exposure time".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(time as i32))
            }
            CameraControlType::Focus => {
                let mut focus = 0u16;
                let result = unsafe { ffi::uvc_get_focus_abs(devh, &mut focus, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get focus".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(focus as i32))
            }
            CameraControlType::AutoFocus => {
                let mut state = 0u8;
                let result = unsafe { ffi::uvc_get_focus_auto(devh, &mut state, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get auto focus state".to_string(),
                    });
                }
                Ok(CameraControlValue::Boolean(state != 0))
            }
            CameraControlType::Zoom => {
                let mut zoom = 0u16;
                let result = unsafe { ffi::uvc_get_zoom_abs(devh, &mut zoom, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get zoom".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(zoom as i32))
            }
            CameraControlType::Brightness => {
                let mut brightness = 0i16;
                let result = unsafe { ffi::uvc_get_brightness(devh, &mut brightness, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get brightness".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(brightness as i32))
            }
            CameraControlType::Contrast => {
                let mut contrast = 0u16;
                let result = unsafe { ffi::uvc_get_contrast(devh, &mut contrast, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get contrast".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(contrast as i32))
            }
            CameraControlType::Saturation => {
                let mut saturation = 0u16;
                let result = unsafe { ffi::uvc_get_saturation(devh, &mut saturation, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get saturation".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(saturation as i32))
            }
            CameraControlType::Hue => {
                let mut hue = 0i16;
                let result = unsafe { ffi::uvc_get_hue(devh, &mut hue, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get hue".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(hue as i32))
            }
            CameraControlType::Sharpness => {
                let mut sharpness = 0u16;
                let result = unsafe { ffi::uvc_get_sharpness(devh, &mut sharpness, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get sharpness".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(sharpness as i32))
            }
            CameraControlType::Gamma => {
                let mut gamma = 0u16;
                let result = unsafe { ffi::uvc_get_gamma(devh, &mut gamma, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get gamma".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(gamma as i32))
            }
            CameraControlType::WhiteBalance => {
                let mut temp = 0u16;
                let result =
                    unsafe { ffi::uvc_get_white_balance_temperature(devh, &mut temp, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get white balance temperature".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(temp as i32))
            }
            CameraControlType::AutoWhiteBalance => {
                let mut state = 0u8;
                let result = unsafe {
                    ffi::uvc_get_white_balance_temperature_auto(devh, &mut state, req_code)
                };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get auto white balance state".to_string(),
                    });
                }
                Ok(CameraControlValue::Boolean(state != 0))
            }
            CameraControlType::Gain => {
                let mut gain = 0u16;
                let result = unsafe { ffi::uvc_get_gain(devh, &mut gain, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get gain".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(gain as i32))
            }
            CameraControlType::BacklightCompensation => {
                let mut comp = 0u16;
                let result =
                    unsafe { ffi::uvc_get_backlight_compensation(devh, &mut comp, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::UvcError {
                        code: result,
                        message: "Failed to get backlight compensation".to_string(),
                    });
                }
                Ok(CameraControlValue::manual(comp as i32))
            }
            CameraControlType::Pan | CameraControlType::Tilt | CameraControlType::Iris => {
                // UVC 库不直接支持这些控制，返回错误
                Err(CameraError::ControlNotSupported(format!("{:?}", control)))
            }
        }
    }

    fn set_control(
        &self,
        control: CameraControlType,
        value: CameraControlValue,
    ) -> CameraResult<()> {
        let devh = self.device_handle.lock();
        let devh = devh
            .as_ref()
            .ok_or_else(|| CameraError::DeviceOpenFailed("Device not open".to_string()))?
            .as_ptr();

        let result =
            match control {
                CameraControlType::AutoExposure => {
                    let enabled = value.as_bool().ok_or_else(|| {
                        CameraError::InvalidConfig("Auto exposure expects boolean".into())
                    })?;
                    let mut mask = 0u8;
                    let code = unsafe {
                        ffi::uvc_get_ae_mode(devh, &mut mask, ffi::UvcReqCode::GetRes as u8)
                    };
                    if code != ffi::UVC_SUCCESS {
                        return Err(CameraError::UvcError {
                            code,
                            message: "Query AE supported modes".into(),
                        });
                    }
                    let mode = select_ae_mode(mask, enabled)?;
                    unsafe { ffi::uvc_set_ae_mode(devh, mode) }
                }
                CameraControlType::Exposure => {
                    let time = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid exposure time value".to_string())
                    })? as u32;
                    unsafe { ffi::uvc_set_exposure_abs(devh, time) }
                }
                CameraControlType::Focus => {
                    let focus = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid focus value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_focus_abs(devh, focus) }
                }
                CameraControlType::AutoFocus => {
                    let state = if value.as_bool().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid auto focus value".to_string())
                    })? {
                        1u8
                    } else {
                        0u8
                    };
                    unsafe { ffi::uvc_set_focus_auto(devh, state) }
                }
                CameraControlType::Zoom => {
                    let zoom = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid zoom value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_zoom_abs(devh, zoom) }
                }
                CameraControlType::Brightness => {
                    let brightness = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid brightness value".to_string())
                    })? as i16;
                    unsafe { ffi::uvc_set_brightness(devh, brightness) }
                }
                CameraControlType::Contrast => {
                    let contrast = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid contrast value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_contrast(devh, contrast) }
                }
                CameraControlType::Saturation => {
                    let saturation = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid saturation value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_saturation(devh, saturation) }
                }
                CameraControlType::Hue => {
                    let hue = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid hue value".to_string())
                    })? as i16;
                    unsafe { ffi::uvc_set_hue(devh, hue) }
                }
                CameraControlType::Sharpness => {
                    let sharpness = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid sharpness value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_sharpness(devh, sharpness) }
                }
                CameraControlType::Gamma => {
                    let gamma = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid gamma value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_gamma(devh, gamma) }
                }
                CameraControlType::WhiteBalance => {
                    let temp = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig(
                            "Invalid white balance temperature value".to_string(),
                        )
                    })? as u16;
                    unsafe { ffi::uvc_set_white_balance_temperature(devh, temp) }
                }
                CameraControlType::AutoWhiteBalance => {
                    let state = if value.as_bool().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid auto white balance value".to_string())
                    })? {
                        1u8
                    } else {
                        0u8
                    };
                    unsafe { ffi::uvc_set_white_balance_temperature_auto(devh, state) }
                }
                CameraControlType::Gain => {
                    let gain = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig("Invalid gain value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_gain(devh, gain) }
                }
                CameraControlType::BacklightCompensation => {
                    let comp = value.as_i32().ok_or_else(|| {
                        CameraError::InvalidConfig(
                            "Invalid backlight compensation value".to_string(),
                        )
                    })? as u16;
                    unsafe { ffi::uvc_set_backlight_compensation(devh, comp) }
                }
                CameraControlType::Pan | CameraControlType::Tilt | CameraControlType::Iris => {
                    // UVC 库不直接支持这些控制，返回错误
                    return Err(CameraError::ControlNotSupported(format!("{:?}", control)));
                }
            };

        if result != ffi::UVC_SUCCESS {
            Err(CameraError::UvcError {
                code: result,
                message: format!("Failed to set control parameter: {:?}", control),
            })
        } else {
            Ok(())
        }
    }

    fn get_control_range(&self, control: CameraControlType) -> CameraResult<CameraControlRange> {
        let devh = self.device_handle.lock();
        let devh = devh
            .as_ref()
            .ok_or_else(|| CameraError::DeviceOpenFailed("Device not open".to_string()))?
            .as_ptr();

        use ffi::UvcReqCode;

        if control.is_auto_control() {
            let mut value = 0u8;
            let getter = match control {
                CameraControlType::AutoExposure => ffi::uvc_get_ae_mode,
                CameraControlType::AutoFocus => ffi::uvc_get_focus_auto,
                CameraControlType::AutoWhiteBalance => ffi::uvc_get_white_balance_temperature_auto,
                _ => unreachable!(),
            };
            let code = unsafe { getter(devh, &mut value, UvcReqCode::GetDef as u8) };
            if code != ffi::UVC_SUCCESS {
                return Err(CameraError::UvcError {
                    code,
                    message: format!("Query {control:?} default"),
                });
            }
            let default = if control == CameraControlType::AutoExposure {
                i32::from(matches!(value, 2 | 4 | 8))
            } else {
                i32::from(value != 0)
            };
            return Ok(CameraControlRange::new(0, 1, 1, default, false));
        }
        macro_rules! range {
            ($getter:expr,$ty:ty) => {{
                let mut values = [0i32; 4];
                for (i, request) in [
                    UvcReqCode::GetMin,
                    UvcReqCode::GetMax,
                    UvcReqCode::GetRes,
                    UvcReqCode::GetDef,
                ]
                .into_iter()
                .enumerate()
                {
                    let mut raw = 0 as $ty;
                    let code = unsafe { $getter(devh, &mut raw, request as u8) };
                    if code != ffi::UVC_SUCCESS {
                        return Err(CameraError::UvcError {
                            code,
                            message: format!("Query {control:?} {request:?}"),
                        });
                    }
                    values[i] = i32::try_from(raw).map_err(|_| {
                        CameraError::InvalidFormat("UVC control range exceeds i32".into())
                    })?;
                }
                if values[0] > values[1] || values[2] <= 0 {
                    return Err(CameraError::InvalidFormat(
                        "Invalid native control range".into(),
                    ));
                }
                Ok(CameraControlRange::new(
                    values[0], values[1], values[2], values[3], false,
                ))
            }};
        }
        match control {
            CameraControlType::Exposure => range!(ffi::uvc_get_exposure_abs, u32),
            CameraControlType::Focus => range!(ffi::uvc_get_focus_abs, u16),
            CameraControlType::Zoom => range!(ffi::uvc_get_zoom_abs, u16),
            CameraControlType::Brightness => range!(ffi::uvc_get_brightness, i16),
            CameraControlType::Hue => range!(ffi::uvc_get_hue, i16),
            CameraControlType::Contrast => range!(ffi::uvc_get_contrast, u16),
            CameraControlType::Saturation => range!(ffi::uvc_get_saturation, u16),
            CameraControlType::Sharpness => range!(ffi::uvc_get_sharpness, u16),
            CameraControlType::Gamma => range!(ffi::uvc_get_gamma, u16),
            CameraControlType::WhiteBalance => range!(ffi::uvc_get_white_balance_temperature, u16),
            CameraControlType::Gain => range!(ffi::uvc_get_gain, u16),
            CameraControlType::BacklightCompensation => {
                range!(ffi::uvc_get_backlight_compensation, u16)
            }
            _ => Err(CameraError::ControlNotSupported(format!("{control:?}"))),
        }
    }

    fn supports_control(&self, control: CameraControlType) -> bool {
        // 尝试获取控制值，如果成功则支持
        self.get_control(control).is_ok()
    }

    fn get_supported_controls(&self) -> Vec<CameraControlType> {
        use CameraControlType::*;
        let all_controls = vec![
            Brightness,
            Contrast,
            Saturation,
            Hue,
            Sharpness,
            Gamma,
            WhiteBalance,
            BacklightCompensation,
            Gain,
            Pan,
            Tilt,
            Zoom,
            Exposure,
            Iris,
            Focus,
            AutoExposure,
            AutoFocus,
            AutoWhiteBalance,
        ];

        all_controls
            .into_iter()
            .filter(|c| self.supports_control(*c))
            .collect()
    }
}

fn select_ae_mode(mask: u8, enabled: bool) -> CameraResult<u8> {
    let candidates: &[u8] = if enabled { &[2, 8, 4] } else { &[1] };
    candidates
        .iter()
        .copied()
        .find(|m| mask & m != 0)
        .ok_or_else(|| {
            CameraError::ControlNotSupported("Requested UVC exposure mode is unavailable".into())
        })
}

impl Drop for UvcCamera {
    fn drop(&mut self) {
        log::debug!("UvcCamera::drop() 调用");

        // cleanup 已经会处理停止流，直接调用即可
        self.cleanup();

        log::debug!("UvcCamera::drop() 完成");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camera_creation() {
        let camera = UvcCamera::new(0);
        assert!(camera.is_ok());
    }

    #[test]
    fn test_format_conversion() {
        assert_eq!(
            UvcCamera::format_to_uvc(VideoFormat::MJPEG),
            ffi::UvcFrameFormat::Mjpeg
        );
    }
}

#[cfg(test)]
mod capture_contract_tests {
    use super::*;
    #[test]
    fn callback_owns_capture_state_without_retaining_the_camera() {
        let camera = Arc::new(UvcCamera::new(0).unwrap());
        let weak = Arc::downgrade(&camera);
        let state = Arc::downgrade(&camera.capture);
        let session = camera.capture.hub.start();
        *camera.callback.lock() = Some(Box::new(CallbackContext {
            state: camera.capture.clone(),
            session,
        }));
        drop(camera);
        assert!(weak.upgrade().is_none());
        assert!(state.upgrade().is_none());
    }
    #[test]
    fn callback_respects_stride_rejects_short_buffers_and_ignores_old_sessions() {
        let camera = UvcCamera::new(0).unwrap();
        let session = camera.capture.hub.start();
        let mut context = Box::new(CallbackContext {
            state: camera.capture.clone(),
            session,
        });
        let pointer = context.as_mut() as *mut CallbackContext as *mut c_void;
        let mut bytes = [10u8, 128, 20, 128, 255, 255, 255, 255, 30, 128, 40, 128];
        let mut frame: ffi::UvcFrame = unsafe { std::mem::zeroed() };
        frame.data = bytes.as_mut_ptr() as *mut c_void;
        frame.data_bytes = bytes.len();
        frame.width = 2;
        frame.height = 2;
        frame.step = 8;
        frame.frame_format = ffi::UvcFrameFormat::Yuyv;
        UvcCamera::frame_callback(&mut frame, pointer);
        let snapshot = camera.capture.hub.latest().unwrap();
        assert_eq!(
            snapshot.bytes(),
            [10, 10, 10, 20, 20, 20, 30, 30, 30, 40, 40, 40]
        );
        frame.data_bytes = 2;
        UvcCamera::frame_callback(&mut frame, pointer);
        assert_eq!(camera.capture.hub.latest().unwrap().key, snapshot.key);
        assert_eq!(camera.capture.hub.metrics().conversion_errors, 1);
        camera.capture.hub.stop();
        camera.capture.hub.start();
        frame.data_bytes = bytes.len();
        UvcCamera::frame_callback(&mut frame, pointer);
        assert!(camera.capture.hub.latest().is_none());
    }
    #[test]
    fn descriptors_use_guid_and_all_discrete_intervals() {
        let mut guid = [0, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
        guid[..4].copy_from_slice(b"YUY2");
        assert_eq!(
            UvcCamera::descriptor_format(4, &guid),
            Some(VideoFormat::YUYV)
        );
        guid[..4].copy_from_slice(b"UYVY");
        assert_eq!(
            UvcCamera::descriptor_format(4, &guid),
            Some(VideoFormat::UYVY)
        );
        guid[..4].copy_from_slice(b"NV12");
        assert_eq!(UvcCamera::descriptor_format(4, &guid), None);
        assert_eq!(
            UvcCamera::descriptor_format(6, &guid),
            Some(VideoFormat::MJPEG)
        );
        let mut intervals = [333333, 666666, 1000000];
        let mut frame: ffi::UvcFrameDesc = unsafe { std::mem::zeroed() };
        frame.dw_default_frame_interval = 333333;
        frame.b_frame_interval_type = 3;
        frame.intervals = intervals.as_mut_ptr();
        assert_eq!(
            unsafe { UvcCamera::descriptor_frame_rates(&frame) },
            [
                crate::FrameRate::new(10, 1).unwrap(),
                crate::FrameRate::new(10_000_000, 666666).unwrap(),
                crate::FrameRate::new(10_000_000, 333333).unwrap()
            ]
        );
    }
}

#[cfg(test)]
mod ae_mode_tests {
    use super::select_ae_mode;
    #[test]
    fn automatic_never_selects_manual_and_respects_capabilities() {
        assert_eq!(select_ae_mode(0b1111, true).unwrap(), 2);
        assert_eq!(select_ae_mode(0b1001, true).unwrap(), 8);
        assert_eq!(select_ae_mode(0b0101, true).unwrap(), 4);
        assert_eq!(select_ae_mode(0b1111, false).unwrap(), 1);
        assert!(select_ae_mode(1, true).is_err());
        assert!(select_ae_mode(2, false).is_err());
    }
}
