//! Android Camera2 NDK camera implementation
//!
//! Full implementation using NDK Camera2 C++ bridge via FFI.

use std::ffi::CStr;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::pixels::Pixels as Array3;

use crate::error::CameraError;
use crate::traits::{CameraControl, CameraManager, StreamStats, StreamingCamera};
use crate::types::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraResult, VideoFormat,
};
use crate::utils::color_convert::{
    rgba8888_to_rgb_into, yuv420_to_rgb_into, yuv420sp_to_rgb_with_coefficients_into, Yuv420Sp,
    YuvPlane, BT601_FULL_COEFFICIENTS,
};

use super::ffi;

/// Camera facing direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraFacing {
    /// Rear-facing camera (ACAMERA_LENS_FACING_BACK = 1)
    Back,
    /// Front-facing camera (ACAMERA_LENS_FACING_FRONT = 0)
    Front,
    /// External camera (ACAMERA_LENS_FACING_EXTERNAL = 2)
    External,
}

impl CameraFacing {
    fn from_ndk(value: i32) -> Self {
        match value {
            0 => CameraFacing::Front,
            1 => CameraFacing::Back,
            2 => CameraFacing::External,
            _ => CameraFacing::External,
        }
    }
}

#[derive(Default)]
struct SharedState {
    hub: crate::FrameHub,
    limiter: Mutex<crate::utils::frame_rate::FrameRateLimiter>,
    callback_min_interval_ns: AtomicU64,
    session: AtomicU64,
}

/// Native camera handle wrapper (ensures proper cleanup)
struct NativeHandle {
    ptr: *mut ffi::NdkCamera2,
}

impl NativeHandle {
    fn new() -> CameraResult<Self> {
        let ptr = unsafe { ffi::ndk_camera2_create() };
        if ptr.is_null() {
            return Err(CameraError::Other(
                "Failed to create NDK Camera2 instance".into(),
            ));
        }
        Ok(Self { ptr })
    }
}

impl Drop for NativeHandle {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                ffi::ndk_camera2_stop_stream(self.ptr);
                ffi::ndk_camera2_destroy(self.ptr);
            }
            self.ptr = std::ptr::null_mut();
        }
    }
}

// SAFETY: The C++ NdkCamera2 instance protects shared state with mutex/atomic.
// All FFI calls go through the single handle pointer.
unsafe impl Send for NativeHandle {}
unsafe impl Sync for NativeHandle {}

/// Camera2 backend implementation using Android NDK Camera2 API.
pub struct Camera2Camera {
    /// Device index
    device_index: u32,
    lifecycle: Mutex<()>,
    /// Native handle (C++ bridge)
    native: Arc<Mutex<NativeHandle>>,
    /// Shared state for frame exchange
    shared: Arc<SharedState>,
    /// Current config
    current_config: Mutex<Option<CameraConfig>>,
    /// Start time for uptime stats
    start_time: Mutex<Option<Instant>>,
}

impl Camera2Camera {
    /// Create a new Camera2 camera instance.
    ///
    /// # Arguments
    /// * `device_index` - Camera device index (0 = first camera)
    pub fn new(device_index: u32) -> CameraResult<Self> {
        Self::new_with_hub(device_index, crate::FrameHub::default())
    }
    pub(crate) fn new_with_hub(device_index: u32, hub: crate::FrameHub) -> CameraResult<Self> {
        Self::new_selected(device_index, hub, None)
    }
    pub(crate) fn new_selected(
        mut device_index: u32,
        hub: crate::FrameHub,
        expected: Option<&str>,
    ) -> CameraResult<Self> {
        log::info!("Creating Camera2Camera for device {}", device_index);

        let native = NativeHandle::new()?;

        // Verify device exists
        let mut count: i32 = 0;
        let status = unsafe { ffi::ndk_camera2_get_device_count(native.ptr, &mut count) };
        if !status.is_ok() {
            return Err(status.error("enumerate cameras"));
        }

        if let Some(expected) = expected {
            let mut selected = None;
            for i in 0..count {
                let mut info = ffi::NdkCameraDeviceInfo {
                    id: std::ptr::null(),
                    facing: 0,
                    orientation: 0,
                    available: false,
                };
                let status = unsafe { ffi::ndk_camera2_get_device_info(native.ptr, i, &mut info) };
                if status.is_ok()
                    && !info.id.is_null()
                    && unsafe { CStr::from_ptr(info.id) }.to_string_lossy() == expected
                {
                    if selected.is_some() {
                        return Err(CameraError::AmbiguousDevice(expected.into()));
                    }
                    selected = Some(i as u32);
                }
            }
            device_index = selected.ok_or_else(|| CameraError::DeviceNotFound(expected.into()))?;
        }
        if device_index as i32 >= count {
            return Err(CameraError::DeviceNotFound(format!(
                "Device index {} not found, only {} cameras available",
                device_index, count
            )));
        }

        Ok(Self {
            device_index,
            lifecycle: Mutex::new(()),
            native: Arc::new(Mutex::new(native)),
            shared: Arc::new(SharedState {
                hub,
                ..Default::default()
            }),
            current_config: Mutex::new(None),
            start_time: Mutex::new(None),
        })
    }

    pub(crate) fn set_options(&self, options: crate::Camera2Options) -> CameraResult<()> {
        let native = self.native.lock().unwrap();
        let max_images = options.max_images.unwrap_or(4) as i32;
        let template = options.request_template.unwrap_or_default().native_value();
        let status = unsafe { ffi::ndk_camera2_set_options(native.ptr, max_images, template) };
        if status.is_ok() {
            Ok(())
        } else {
            Err(status.error("configure Camera2 options"))
        }
    }

    /// Select camera by facing direction.
    pub fn with_facing(facing: CameraFacing) -> CameraResult<Self> {
        log::info!("Selecting camera with facing: {:?}", facing);

        let native = NativeHandle::new()?;
        let mut count: i32 = 0;
        let status = unsafe { ffi::ndk_camera2_get_device_count(native.ptr, &mut count) };
        if !status.is_ok() || count == 0 {
            return Err(CameraError::DeviceNotFound("No cameras available".into()));
        }

        let target_facing = match facing {
            CameraFacing::Front => 0,
            CameraFacing::Back => 1,
            CameraFacing::External => 2,
        };

        for i in 0..count {
            let mut info = ffi::NdkCameraDeviceInfo {
                id: std::ptr::null(),
                facing: 0,
                orientation: 0,
                available: false,
            };
            let status = unsafe { ffi::ndk_camera2_get_device_info(native.ptr, i, &mut info) };
            if status.is_ok() && info.facing == target_facing {
                log::info!("Found {:?} camera at index {}", facing, i);
                return Self::new(i as u32);
            }
        }

        Err(CameraError::DeviceNotFound(format!(
            "No camera with facing {:?} found",
            facing
        )))
    }

    /// Clean up resources.
    pub fn cleanup(&self) {
        let _guard = self.lifecycle.lock().unwrap();
        self.shared.hub.stop();

        {
            let native = self.native.lock().unwrap();
            unsafe {
                ffi::ndk_camera2_stop_stream(native.ptr);
            }
        }

        *self.current_config.lock().unwrap() = None;
        *self.start_time.lock().unwrap() = None;
    }

    pub fn get_latest_frame_raw(&self) -> CameraResult<Option<(Vec<u8>, u32, u32, u8)>> {
        Ok(self
            .shared
            .hub
            .latest()
            .map(|f| (f.bytes().to_vec(), f.layout().width, f.layout().height, 3)))
    }
}

/// The native bridge retains SharedState through stop; null frame signals a
/// device disconnect/error and wakes all consumers immediately.
unsafe extern "C" fn frame_callback(context: *mut c_void, frame: *const ffi::NdkFrameData) {
    if context.is_null() {
        return;
    }
    let shared = &*(context as *const SharedState);
    if frame.is_null() {
        shared.hub.stop();
        return;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let frame = &*frame;
        if !shared.hub.is_streaming() || frame.width <= 0 || frame.height <= 0 {
            return Ok(false);
        }
        if !shared.limiter.lock().unwrap().accept(
            frame.timestamp_ns.max(0) as u64,
            shared.callback_min_interval_ns.load(Ordering::Relaxed),
        ) {
            return Ok(false);
        }
        let width = frame.width as usize;
        let height = frame.height as usize;
        let timestamp = Some(crate::SourceTimestamp {
            nanoseconds: frame.timestamp_ns,
            clock: crate::ClockDomain::DeviceMonotonic,
        });
        if shared.hub.wants_native() {
            let (layout, parts) = native_frame_layout(frame)?;
            return shared.hub.publish_native(
                shared.session.load(Ordering::Acquire),
                layout,
                timestamp,
                None,
                &parts,
            );
        }
        shared.hub.publish_rgb_metadata(
            shared.session.load(Ordering::Acquire),
            frame.width as u32,
            frame.height as u32,
            timestamp,
            None,
            |rgb| {
                if frame.pixel_format == ffi::NdkCameraPixelFormat::Rgba8888 as i32
                    && !frame.rgb_data.is_null()
                    && frame.rgb_len > 0
                    && frame.row_stride_rgb > 0
                {
                    return rgba8888_to_rgb_into(
                        std::slice::from_raw_parts(frame.rgb_data, frame.rgb_len as usize),
                        width,
                        height,
                        frame.row_stride_rgb as usize,
                        rgb,
                    );
                }
                if frame.y_data.is_null()
                    || frame.uv_data.is_null()
                    || frame.v_data.is_null()
                    || frame.y_len <= 0
                    || frame.uv_len <= 0
                    || frame.v_len <= 0
                    || frame.row_stride_y <= 0
                    || frame.row_stride_uv <= 0
                    || frame.row_stride_v <= 0
                    || frame.pixel_stride_uv <= 0
                    || frame.pixel_stride_v <= 0
                {
                    return Err(CameraError::InvalidFormat(
                        "Invalid Camera2 plane metadata".into(),
                    ));
                }
                convert_camera2_yuv(frame, width, height, rgb)
            },
        )
    }));
    match result {
        Ok(Err(error)) => shared.hub.log_frame_error("Camera2", &error),
        Err(_) => log::error!("Camera2 callback panicked; frame discarded"),
        _ => {}
    }
}

unsafe fn convert_camera2_yuv(
    frame: &ffi::NdkFrameData,
    width: usize,
    height: usize,
    rgb: &mut [u8],
) -> CameraResult<()> {
    let y = std::slice::from_raw_parts(frame.y_data, frame.y_len as usize);
    let u_span = crate::format::ChromaPlaneSpan {
        start: frame.uv_data as usize,
        length: frame.uv_len as usize,
        row_stride: frame.row_stride_uv as usize,
        pixel_stride: frame.pixel_stride_uv as usize,
    };
    let v_span = crate::format::ChromaPlaneSpan {
        start: frame.v_data as usize,
        length: frame.v_len as usize,
        row_stride: frame.row_stride_v as usize,
        pixel_stride: frame.pixel_stride_v as usize,
    };
    if let Some(chroma) = crate::format::interleaved_chroma_layout(width, height, u_span, v_span) {
        let uv = std::slice::from_raw_parts(chroma.start as *const u8, chroma.length);
        return yuv420sp_to_rgb_with_coefficients_into(
            Yuv420Sp {
                y,
                uv,
                y_stride: frame.row_stride_y as usize,
                uv_stride: chroma.row_stride,
                vu_order: chroma.format == crate::PixelFormat::Nv21,
            },
            width,
            height,
            BT601_FULL_COEFFICIENTS,
            rgb,
        );
    }
    yuv420_to_rgb_into(
        YuvPlane {
            data: y,
            row_stride: frame.row_stride_y as usize,
            pixel_stride: 1,
        },
        YuvPlane {
            data: std::slice::from_raw_parts(frame.uv_data, frame.uv_len as usize),
            row_stride: frame.row_stride_uv as usize,
            pixel_stride: frame.pixel_stride_uv as usize,
        },
        YuvPlane {
            data: std::slice::from_raw_parts(frame.v_data, frame.v_len as usize),
            row_stride: frame.row_stride_v as usize,
            pixel_stride: frame.pixel_stride_v as usize,
        },
        width,
        height,
        rgb,
    )
}

unsafe fn native_frame_layout(
    frame: &ffi::NdkFrameData,
) -> CameraResult<(crate::FrameLayout, Vec<&[u8]>)> {
    use crate::{FrameLayout, PixelFormat, PlaneLayout};
    if frame.pixel_format == ffi::NdkCameraPixelFormat::Rgba8888 as i32 {
        if frame.rgb_data.is_null() || frame.rgb_len <= 0 || frame.row_stride_rgb <= 0 {
            return Err(CameraError::InvalidFormat(
                "Invalid Camera2 RGBA frame".into(),
            ));
        }
        let data = std::slice::from_raw_parts(frame.rgb_data, frame.rgb_len as usize);
        return Ok((
            FrameLayout::packed(
                frame.width as u32,
                frame.height as u32,
                PixelFormat::Rgba8,
                frame.row_stride_rgb as usize,
                data.len(),
            ),
            vec![data],
        ));
    }
    if frame.width <= 0
        || frame.height <= 0
        || frame.y_data.is_null()
        || frame.y_len <= 0
        || frame.row_stride_y <= 0
        || frame.uv_data.is_null()
        || frame.uv_len <= 0
        || frame.row_stride_uv <= 0
        || frame.pixel_stride_uv <= 0
        || frame.v_data.is_null()
        || frame.v_len <= 0
        || frame.row_stride_v <= 0
        || frame.pixel_stride_v <= 0
    {
        return Err(CameraError::InvalidFormat(
            "Invalid Camera2 YUV plane".into(),
        ));
    }
    let y = std::slice::from_raw_parts(frame.y_data, frame.y_len as usize);
    let u_span = crate::format::ChromaPlaneSpan {
        start: frame.uv_data as usize,
        length: frame.uv_len as usize,
        row_stride: frame.row_stride_uv as usize,
        pixel_stride: frame.pixel_stride_uv as usize,
    };
    let v_span = crate::format::ChromaPlaneSpan {
        start: frame.v_data as usize,
        length: frame.v_len as usize,
        row_stride: frame.row_stride_v as usize,
        pixel_stride: frame.pixel_stride_v as usize,
    };
    if let Some(chroma) = crate::format::interleaved_chroma_layout(
        frame.width as usize,
        frame.height as usize,
        u_span,
        v_span,
    ) {
        let (chroma_ptr, chroma_length) = if chroma.format == PixelFormat::Nv12 {
            (frame.uv_data, frame.uv_len as usize)
        } else {
            (frame.v_data, frame.v_len as usize)
        };
        let chroma_data = std::slice::from_raw_parts(chroma_ptr, chroma_length);
        let mut layout =
            FrameLayout::packed(frame.width as u32, frame.height as u32, chroma.format, 0, 0);
        layout.planes = vec![
            PlaneLayout {
                offset: 0,
                length: y.len(),
                row_stride: frame.row_stride_y as usize,
                pixel_stride: 1,
            },
            PlaneLayout {
                offset: y.len(),
                length: chroma.length,
                row_stride: chroma.row_stride,
                pixel_stride: 2,
            },
        ];
        return Ok((layout, vec![y, chroma_data]));
    }
    let mut layout = FrameLayout::packed(
        frame.width as u32,
        frame.height as u32,
        PixelFormat::Yuv420p,
        0,
        0,
    );
    layout.planes.clear();
    let mut parts = Vec::new();
    let mut offset = 0;
    for (ptr, len, row, pixel) in [
        (frame.y_data, frame.y_len, frame.row_stride_y, 1),
        (
            frame.uv_data,
            frame.uv_len,
            frame.row_stride_uv,
            frame.pixel_stride_uv,
        ),
        (
            frame.v_data,
            frame.v_len,
            frame.row_stride_v,
            frame.pixel_stride_v,
        ),
    ] {
        parts.push(std::slice::from_raw_parts(ptr, len as usize));
        layout.planes.push(PlaneLayout {
            offset,
            length: len as usize,
            row_stride: row as usize,
            pixel_stride: pixel as usize,
        });
        offset += len as usize;
    }
    Ok((layout, parts))
}

impl CameraManager for Camera2Camera {
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
        let native = NativeHandle::new()?;
        let mut count: i32 = 0;
        let status = unsafe { ffi::ndk_camera2_get_device_count(native.ptr, &mut count) };
        if !status.is_ok() {
            return Err(status.error("enumerate cameras"));
        }

        let mut devices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut info = ffi::NdkCameraDeviceInfo {
                id: std::ptr::null(),
                facing: 0,
                orientation: 0,
                available: false,
            };
            let status = unsafe { ffi::ndk_camera2_get_device_info(native.ptr, i, &mut info) };
            if !status.is_ok() {
                continue;
            }

            let id_str = if !info.id.is_null() {
                unsafe { CStr::from_ptr(info.id) }
                    .to_string_lossy()
                    .into_owned()
            } else {
                format!("{}", i)
            };

            let facing = CameraFacing::from_ndk(info.facing);
            let facing_str = match facing {
                CameraFacing::Back => "Back Camera",
                CameraFacing::Front => "Front Camera",
                CameraFacing::External => "External Camera",
            };

            devices.push(CameraDeviceInfo {
                index: i as u32,
                name: format!("Camera {} ({})", id_str, facing_str),
                description: format!(
                    "Android Camera2 - {} - orientation {}",
                    facing_str, info.orientation
                ),
                vendor_id: None,
                product_id: None,
                serial_number: None,
                device_path: Some(id_str),
            });
        }

        Ok(devices)
    }

    fn get_supported_configs(device_index: u32) -> CameraResult<Vec<CameraConfig>> {
        let native = NativeHandle::new()?;
        let mut count: i32 = 0;
        let status = unsafe {
            ffi::ndk_camera2_get_config_count(native.ptr, device_index as i32, &mut count)
        };
        if !status.is_ok() {
            return Err(status.error("enumerate configurations"));
        }

        let mut configs = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut cfg = ffi::NdkCameraConfig {
                width: 0,
                height: 0,
                format: 0,
                fps: 30,
            };
            let status = unsafe {
                ffi::ndk_camera2_get_config(native.ptr, device_index as i32, i, &mut cfg)
            };
            if status.is_ok() {
                configs.push(CameraConfig::new(
                    VideoFormat::NV12,
                    cfg.width as u32,
                    cfg.height as u32,
                    cfg.fps as u32,
                ));
            }
        }

        Ok(configs)
    }

    fn is_config_supported(device_index: u32, config: &CameraConfig) -> CameraResult<bool> {
        let configs = Self::get_supported_configs(device_index)?;
        Ok(configs
            .iter()
            .any(|c| c.width == config.width && c.height == config.height))
    }
}

impl StreamingCamera for Camera2Camera {
    fn frame_hub(&self) -> Option<&crate::FrameHub> {
        Some(&self.shared.hub)
    }
    async fn start_stream(&self, config: CameraConfig) -> CameraResult<()> {
        self.start_stream_sync(config)
    }

    async fn stop_stream(&self) -> CameraResult<()> {
        self.stop_stream_sync()
    }

    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>> {
        Ok(self.shared.hub.latest().map(|f| {
            f.rgb_pixels()
                .expect("RGB backend adapter")
                .as_ref()
                .clone()
        }))
    }
    async fn wait_for_frame(&self, timeout: Duration) -> CameraResult<Array3<u8>> {
        Ok((*self
            .shared
            .hub
            .wait_after(None, timeout)
            .await?
            .rgb_pixels()?)
        .as_ref()
        .clone())
    }
    fn is_streaming(&self) -> bool {
        self.shared.hub.is_streaming()
    }

    fn get_config(&self) -> Option<CameraConfig> {
        self.current_config.lock().unwrap().clone()
    }

    fn get_stats(&self) -> StreamStats {
        self.shared.hub.stats()
    }

    async fn set_buffer_size(&self, _size: usize) -> CameraResult<()> {
        Err(CameraError::InvalidConfig(
            "Frame pool capacity is fixed for the lifetime of the camera".into(),
        ))
    }
}

impl CameraControl for Camera2Camera {
    fn get_control(&self, control: CameraControlType) -> CameraResult<CameraControlValue> {
        Err(CameraError::NotReadable(format!(
            "Camera2 {control:?}: capture-result readback is unavailable"
        )))
    }

    fn set_control(
        &self,
        control: CameraControlType,
        value: CameraControlValue,
    ) -> CameraResult<()> {
        let native = self.native.lock().unwrap();
        let status = match control {
            CameraControlType::Brightness | CameraControlType::Exposure => {
                let v = value.as_i32().unwrap_or(0);
                unsafe { ffi::ndk_camera2_set_exposure_compensation(native.ptr, v) }
            }
            CameraControlType::AutoExposure => {
                let mode = value
                    .as_i32()
                    .ok_or_else(|| CameraError::InvalidConfig("Expected native AE mode".into()))?;
                unsafe { ffi::ndk_camera2_set_ae_mode(native.ptr, mode) }
            }
            CameraControlType::Focus | CameraControlType::AutoFocus => {
                let mode = value
                    .as_i32()
                    .ok_or_else(|| CameraError::InvalidConfig("Expected native AF mode".into()))?;
                unsafe { ffi::ndk_camera2_set_af_mode(native.ptr, mode) }
            }
            CameraControlType::WhiteBalance | CameraControlType::AutoWhiteBalance => {
                let mode = value
                    .as_i32()
                    .ok_or_else(|| CameraError::InvalidConfig("Expected native AWB mode".into()))?;
                unsafe { ffi::ndk_camera2_set_awb_mode(native.ptr, mode) }
            }
            CameraControlType::Zoom => {
                let v = value.as_i32().unwrap_or(100);
                unsafe { ffi::ndk_camera2_set_zoom(native.ptr, v) }
            }
            CameraControlType::Gain => {
                let v = value.as_i32().unwrap_or(100);
                unsafe { ffi::ndk_camera2_set_sensitivity(native.ptr, v) }
            }
            _ => {
                return Err(CameraError::ControlNotSupported(format!(
                    "{:?} is not supported via NDK Camera2",
                    control
                )));
            }
        };

        if status.is_ok() {
            Ok(())
        } else {
            Err(status.error(format!("set {control:?}")))
        }
    }

    fn get_control_range(&self, control: CameraControlType) -> CameraResult<CameraControlRange> {
        let native = self.native.lock().unwrap();
        let mut min: i32 = 0;
        let mut max: i32 = 0;

        let status = match control {
            CameraControlType::Brightness | CameraControlType::Exposure => unsafe {
                ffi::ndk_camera2_get_exposure_compensation_range(native.ptr, &mut min, &mut max)
            },
            CameraControlType::Zoom => unsafe {
                ffi::ndk_camera2_get_zoom_range(native.ptr, &mut min, &mut max)
            },
            CameraControlType::Gain => unsafe {
                ffi::ndk_camera2_get_sensitivity_range(native.ptr, &mut min, &mut max)
            },
            _ => {
                return Err(CameraError::ControlNotSupported(format!(
                    "Range query for {:?} not supported via NDK Camera2",
                    control
                )));
            }
        };

        if status.is_ok() {
            Ok(CameraControlRange {
                min,
                max,
                step: 1,
                // Camera2 reports limits, not a default. The public descriptor
                // deliberately exposes None for this internal placeholder.
                default: 0,
                supports_auto: matches!(
                    control,
                    CameraControlType::Brightness | CameraControlType::Exposure
                ),
            })
        } else {
            Err(status.error(format!("query {control:?} range")))
        }
    }

    fn supports_control(&self, control: CameraControlType) -> bool {
        if control.is_auto_control() {
            self.supported_modes(control).is_ok_and(|m| !m.is_empty())
        } else {
            self.get_control_range(control).is_ok()
        }
    }

    fn get_supported_controls(&self) -> Vec<CameraControlType> {
        vec![
            CameraControlType::Brightness,
            CameraControlType::Exposure,
            CameraControlType::AutoExposure,
            CameraControlType::Focus,
            CameraControlType::AutoFocus,
            CameraControlType::WhiteBalance,
            CameraControlType::AutoWhiteBalance,
            CameraControlType::Zoom,
            CameraControlType::Gain,
        ]
    }
}

impl Camera2Camera {
    pub(crate) fn supported_modes(&self, control: CameraControlType) -> CameraResult<Vec<i32>> {
        let kind = match control {
            CameraControlType::AutoExposure => 0,
            CameraControlType::AutoFocus => 1,
            CameraControlType::AutoWhiteBalance => 2,
            _ => {
                return Err(CameraError::ControlNotSupported(format!(
                    "{control:?} modes"
                )))
            }
        };
        let native = self.native.lock().unwrap();
        let mut mask = 0;
        let status = unsafe { ffi::ndk_camera2_get_control_modes(native.ptr, kind, &mut mask) };
        if !status.is_ok() {
            return Err(status.error("query control modes"));
        }
        Ok((0..32).filter(|i| mask & (1 << i) != 0).collect())
    }
}

// SAFETY: Camera2Camera uses thread-safe primitives (Arc, Mutex, Atomic).
// The native handle is protected by Mutex. Frame callback context uses Arc.
unsafe impl Send for Camera2Camera {}
unsafe impl Sync for Camera2Camera {}

impl Drop for Camera2Camera {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camera_facing() {
        assert_eq!(CameraFacing::from_ndk(0), CameraFacing::Front);
        assert_eq!(CameraFacing::from_ndk(1), CameraFacing::Back);
        assert_eq!(CameraFacing::from_ndk(2), CameraFacing::External);
        assert_eq!(CameraFacing::from_ndk(99), CameraFacing::External);
    }

    #[test]
    fn test_yuv_to_rgb_basic() {
        // Simple 2x2 black frame (Y=0, U=128, V=128 => black RGB)
        let y_data: [u8; 4] = [0, 0, 0, 0];
        let uv_data: [u8; 4] = [128, 128, 128, 128];

        let mut rgb = vec![0; 2 * 2 * 3];
        crate::utils::color_convert::yuv420sp_to_rgb_into(&y_data, &uv_data, 2, 2, 2, 4, &mut rgb)
            .expect("YUV420 conversion");

        assert_eq!(rgb.len(), 2 * 2 * 3);
        // All should be close to 0 (black)
        for &v in &rgb {
            assert!(v < 10, "Expected near-black, got {}", v);
        }
    }
}

impl Camera2Camera {
    fn start_stream_sync(&self, mut config: CameraConfig) -> CameraResult<()> {
        let _guard = self.lifecycle.lock().unwrap();

        if self.shared.hub.is_streaming() {
            log::warn!("Stream is already running");
            return Ok(());
        }

        config.validate()?;

        if config.fps_denominator != 1 {
            return Err(CameraError::UnsupportedFormat(
                "Camera2 AE target FPS ranges use integer rates".into(),
            ));
        }
        let mut ndk_config = ffi::NdkCameraConfig {
            width: config.width as i32,
            height: config.height as i32,
            format: 0x23, // AIMAGE_FORMAT_YUV_420_888
            fps: i32::try_from(config.fps / config.fps_denominator)
                .map_err(|_| CameraError::InvalidConfig("Camera2 FPS overflow".into()))?,
        };

        let session = self.shared.hub.start();
        self.shared.session.store(session, Ordering::Release);
        *self.shared.limiter.lock().unwrap() = Default::default();
        self.shared.callback_min_interval_ns.store(
            config
                .frame_rate()?
                .interval()
                .as_nanos()
                .min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );

        // Create a raw pointer to shared state for the callback.
        // We use Arc::as_ptr to get a non-owning pointer. The Arc is kept alive
        // by self.shared, so this is safe as long as stop_stream is called before drop.
        let shared_ptr = Arc::as_ptr(&self.shared) as *mut c_void;

        let status = {
            let native = self.native.lock().unwrap();
            unsafe {
                let status = ffi::ndk_camera2_start_stream(
                    native.ptr,
                    self.device_index as i32,
                    &ndk_config,
                    frame_callback,
                    shared_ptr,
                );
                if status.is_ok() {
                    ffi::ndk_camera2_get_active_config(native.ptr, &mut ndk_config)
                } else {
                    status
                }
            }
        };

        if !status.is_ok() {
            self.shared.hub.stop();
            return Err(status.error("start capture"));
        }

        config.fps = ndk_config.fps as u32;
        self.shared.callback_min_interval_ns.store(
            config
                .frame_rate()?
                .interval()
                .as_nanos()
                .min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        *self.current_config.lock().unwrap() = Some(config);
        *self.start_time.lock().unwrap() = Some(Instant::now());

        log::info!("Camera2 stream started successfully");
        Ok(())
    }
    pub async fn start_stream_arc(self: &Arc<Self>, config: CameraConfig) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || camera.start_stream_sync(config))
            .await
            .map_err(|e| CameraError::Other(e.to_string()))?
    }
    fn stop_stream_sync(&self) -> CameraResult<()> {
        let _guard = self.lifecycle.lock().unwrap();

        self.shared.hub.stop();

        let status = {
            let native = self.native.lock().unwrap();
            unsafe { ffi::ndk_camera2_stop_stream(native.ptr) }
        };

        *self.current_config.lock().unwrap() = None;
        *self.start_time.lock().unwrap() = None;
        if !status.is_ok() && status != ffi::NdkCameraStatus::ErrorNotStreaming {
            return Err(status.error("stop capture"));
        }
        log::info!("Camera2 stream stopped");
        Ok(())
    }
    pub async fn stop_stream_arc(self: &Arc<Self>) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || camera.stop_stream_sync())
            .await
            .map_err(|e| CameraError::Other(e.to_string()))?
    }
}
