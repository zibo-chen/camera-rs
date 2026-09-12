//! UVC camera implementation.

use super::context::{StreamCtrl, UvcContext, UvcDeviceHandle};
use super::ffi;
use crate::error::{CameraError, Result};
use crate::pixels::Pixels as Array3;
use crate::traits::{CameraControl, CameraManager, StreamStats, StreamingCamera};
use crate::types::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraResult, VideoFormat,
};
#[cfg(feature = "convert-rgb")]
use crate::utils::color_convert::ColorConverter;
use parking_lot::Mutex;
use std::ffi::c_void;
#[cfg(feature = "decode-mjpeg")]
use std::sync::mpsc;
use std::sync::Arc;
#[cfg(feature = "decode-mjpeg")]
use std::thread::JoinHandle;
use std::time::Duration;
#[cfg(feature = "decode-mjpeg")]
use turbojpeg::{Decompressor, Image, PixelFormat};

struct CaptureState {
    hub: crate::FrameHub,
    #[cfg(feature = "decode-mjpeg")]
    decoder: Mutex<Option<Decompressor>>,
    #[cfg(feature = "convert-rgb")]
    converter: ColorConverter,
}

// Owned by the camera until uvc_stop_streaming has joined the callback thread.
// This owns only capture state, never a reference back to the camera.
struct CallbackContext {
    state: Arc<CaptureState>,
    session: u64,
    #[cfg(feature = "decode-mjpeg")]
    mjpeg: Option<MjpegDispatch>,
}

#[cfg(feature = "decode-mjpeg")]
struct MjpegDispatch {
    sender: mpsc::SyncSender<MjpegJob>,
    buffers: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[cfg(feature = "decode-mjpeg")]
struct MjpegJob {
    data: Vec<u8>,
    width: u32,
    height: u32,
    sequence: u64,
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
    #[cfg(feature = "decode-mjpeg")]
    mjpeg_worker: Mutex<Option<JoinHandle<()>>>,
    lifecycle: Mutex<()>,
}

impl UvcCamera {
    /// Creates a UVC camera instance.
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
                #[cfg(feature = "decode-mjpeg")]
                decoder: Mutex::new(None),
                #[cfg(feature = "convert-rgb")]
                converter: ColorConverter::new(),
            }),
            callback: Mutex::new(None),
            #[cfg(feature = "decode-mjpeg")]
            mjpeg_worker: Mutex::new(None),
            lifecycle: Mutex::new(()),
        })
    }

    pub(crate) fn exposure_modes(&self) -> CameraResult<Vec<crate::ControlMode>> {
        let handle = self.device_handle.lock();
        let handle = handle.as_ref().ok_or(CameraError::stream_stopped())?;
        let mut mask = 0;
        let code = unsafe {
            ffi::uvc_get_ae_mode(handle.as_ptr(), &mut mask, ffi::UvcReqCode::GetRes as u8)
        };
        if code != ffi::UVC_SUCCESS {
            return Err(CameraError::uvc(code, "Query exposure modes".into()));
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
    /// Creates a camera from an Android file descriptor.
    ///
    /// Android applications cannot access USB device nodes directly. Use
    /// `UsbManager.openDevice()` to obtain a `UsbDeviceConnection`, then pass
    /// the value returned by `getFileDescriptor()` to this method.
    #[cfg(unix)]
    pub fn from_fd(fd: i32) -> Result<Self> {
        log::info!("Creating UvcCamera from file descriptor: {}", fd);

        let camera = Self::new(0)?;
        let mut ctx = UvcContext::new()?;
        let devh = ctx.wrap_device(fd)?;

        // Query and log the formats supported by the device.
        Self::log_supported_formats(devh.as_ptr());

        *camera.context.lock() = Some(ctx);
        *camera.device_handle.lock() = Some(devh);

        log::info!("UvcCamera created successfully from fd");
        Ok(camera)
    }

    /// Logs all formats supported by the device.
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

                // Visit each frame descriptor.
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

    fn supported_configs_from_handle(devh: *mut ffi::UvcDeviceHandle) -> Vec<CameraConfig> {
        let mut configs = Vec::new();
        unsafe {
            let mut current_format = ffi::uvc_get_format_descs(devh);
            while !current_format.is_null() {
                let format = &*current_format;
                if let Some(video_format) =
                    Self::descriptor_format(format.b_descriptor_subtype as u8, &format.guid_format)
                {
                    let mut current_frame = format.frame_descs;
                    while !current_frame.is_null() {
                        let frame = &*current_frame;
                        let width = frame.w_width as u32;
                        let height = frame.w_height as u32;
                        for frame_rate in Self::descriptor_frame_rates(frame) {
                            if width > 0 && height > 0 {
                                let candidate = CameraConfig::new(
                                    video_format,
                                    width,
                                    height,
                                    frame_rate.numerator(),
                                )
                                .with_frame_rate(frame_rate);
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
        configs
    }

    pub(crate) fn opened_device_capabilities(&self) -> CameraResult<crate::DeviceCapabilities> {
        let handle = self.device_handle.lock();
        let handle = handle.as_ref().ok_or_else(|| {
            CameraError::invalid_state(
                "UVC device must be open before querying capabilities".into(),
            )
        })?;
        Ok(crate::DeviceCapabilities::from_configurations(
            Self::supported_configs_from_handle(handle.as_ptr()),
        ))
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
        #[cfg(feature = "decode-mjpeg")]
        if let Some(worker) = self.mjpeg_worker.lock().take() {
            if worker.join().is_err() {
                log::warn!("UVC MJPEG worker panicked while stopping");
            }
        }
        *self.stream_ctrl.lock() = None;
        #[cfg(feature = "decode-mjpeg")]
        {
            *self.capture.decoder.lock() = None;
        }
    }

    #[cfg(feature = "decode-mjpeg")]
    fn start_mjpeg_worker(
        state: Arc<CaptureState>,
        session: u64,
    ) -> CameraResult<(MjpegDispatch, JoinHandle<()>)> {
        let (sender, receiver) = mpsc::sync_channel::<MjpegJob>(1);
        let buffers = Arc::new(Mutex::new(vec![Vec::new(), Vec::new()]));
        let worker_buffers = buffers.clone();
        let worker = std::thread::Builder::new()
            .name("camera-uvc-mjpeg".into())
            .spawn(move || {
                let mut decoder = None;
                while let Ok(mut job) = receiver.recv() {
                    let result = state.hub.publish_rgb_metadata(
                        session,
                        job.width,
                        job.height,
                        None,
                        Some(job.sequence),
                        |rgb| {
                            crate::mjpeg::decode_with(&mut decoder, &job.data, |decoder, data| {
                                let header = decoder.read_header(data).map_err(|error| {
                                    CameraError::invalid_frame(error.to_string())
                                })?;
                                if header.width != job.width as usize
                                    || header.height != job.height as usize
                                {
                                    return Err(CameraError::invalid_frame(
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
                                    .map_err(|error| CameraError::invalid_frame(error.to_string()))
                            })
                        },
                    );
                    if let Err(error) = result {
                        state.hub.log_frame_error("UVC MJPEG", &error);
                    }
                    job.data.clear();
                    worker_buffers.lock().push(job.data);
                }
            })
            .map_err(|error| {
                CameraError::worker_failure(format!("Start UVC MJPEG worker: {error}"))
                    .with_backend(crate::BackendId::UVC)
                    .with_source(error)
            })?;
        Ok((MjpegDispatch { sender, buffers }, worker))
    }

    extern "C" fn frame_callback(frame: *mut ffi::UvcFrame, user_ptr: *mut c_void) {
        if frame.is_null() || user_ptr.is_null() {
            return;
        }
        // SAFETY: libuvc retains the callback context until streaming is
        // synchronously stopped and this callback has returned.
        let hub = unsafe { (&*(user_ptr as *const CallbackContext)).state.hub.clone() };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            let context = &*(user_ptr as *const CallbackContext);
            let frame = &*frame;
            if frame.data.is_null() || frame.data_bytes == 0 {
                return Ok(false);
            }
            let data = std::slice::from_raw_parts(frame.data as *const u8, frame.data_bytes);
            #[cfg(feature = "decode-mjpeg")]
            if frame.frame_format == ffi::UvcFrameFormat::Mjpeg {
                if let Err(error) = crate::mjpeg::validate(data) {
                    context.state.hub.record_input_error(context.session);
                    context.state.hub.log_frame_error("UVC MJPEG", &error);
                    return Ok(false);
                }
            }
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
                        return Err(CameraError::unsupported_format(
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
            #[cfg(not(feature = "convert-rgb"))]
            return Err(CameraError::unsupported_format(
                "RGB delivery requires the convert-rgb feature".into(),
            ));
            #[cfg(feature = "convert-rgb")]
            {
                #[cfg(feature = "decode-mjpeg")]
                if frame.frame_format == ffi::UvcFrameFormat::Mjpeg {
                    if let Some(dispatch) = &context.mjpeg {
                        let Some(mut buffer) = dispatch.buffers.lock().pop() else {
                            context.state.hub.record_input_drop(context.session);
                            return Ok(false);
                        };
                        buffer.clear();
                        buffer.extend_from_slice(data);
                        let job = MjpegJob {
                            data: buffer,
                            width: frame.width,
                            height: frame.height,
                            sequence: frame.sequence as u64,
                        };
                        return match dispatch.sender.try_send(job) {
                            Ok(()) => Ok(true),
                            Err(mpsc::TrySendError::Full(job)) => {
                                dispatch.buffers.lock().push(job.data);
                                context.state.hub.record_input_drop(context.session);
                                Ok(false)
                            }
                            Err(mpsc::TrySendError::Disconnected(job)) => {
                                dispatch.buffers.lock().push(job.data);
                                Err(CameraError::stream_stopped())
                            }
                        };
                    }
                }
                context.state.hub.publish_rgb_metadata(
                    context.session,
                    frame.width,
                    frame.height,
                    None,
                    Some(frame.sequence as u64),
                    |rgb| match frame.frame_format {
                        ffi::UvcFrameFormat::Mjpeg => {
                            #[cfg(not(feature = "decode-mjpeg"))]
                            return Err(CameraError::unsupported_format(
                                "MJPEG decoding requires the decode-mjpeg feature".into(),
                            ));
                            #[cfg(feature = "decode-mjpeg")]
                            {
                                let mut guard = context.state.decoder.lock();
                                crate::mjpeg::decode_with(&mut guard, data, |decoder, data| {
                                    let header = decoder
                                        .read_header(data)
                                        .map_err(|e| CameraError::invalid_frame(e.to_string()))?;
                                    if header.width != frame.width as usize
                                        || header.height != frame.height as usize
                                    {
                                        return Err(CameraError::invalid_frame(
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
                                        .map_err(|e| CameraError::invalid_frame(e.to_string()))
                                })
                            }
                        }
                        ffi::UvcFrameFormat::Yuyv | ffi::UvcFrameFormat::Uyvy => {
                            let width = frame.width as usize;
                            let packed = width.checked_mul(2).ok_or_else(|| {
                                CameraError::invalid_frame("UVC row size overflow".into())
                            })?;
                            let stride = if frame.step == 0 { packed } else { frame.step };
                            if stride < packed {
                                return Err(CameraError::invalid_frame(
                                    "UVC row stride is too short".into(),
                                ));
                            }
                            if stride == packed {
                                return if frame.frame_format == ffi::UvcFrameFormat::Yuyv {
                                    context.state.converter.yuyv_to_rgb_into(
                                        data,
                                        frame.width,
                                        frame.height,
                                        rgb,
                                    )
                                } else {
                                    context.state.converter.uyvy_to_rgb_into(
                                        data,
                                        frame.width,
                                        frame.height,
                                        rgb,
                                    )
                                };
                            }
                            for (y, row) in rgb.chunks_exact_mut(width * 3).enumerate() {
                                let offset = y.checked_mul(stride).ok_or_else(|| {
                                    CameraError::invalid_frame("UVC stride overflow".into())
                                })?;
                                let src =
                                    data.get(offset..offset.saturating_add(packed)).ok_or_else(
                                        || CameraError::invalid_frame("Truncated UVC row".into()),
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
                                return Err(CameraError::invalid_frame(
                                    "UVC stride too short".into(),
                                ));
                            }
                            for (y, row) in
                                rgb.chunks_exact_mut(frame.width as usize * 3).enumerate()
                            {
                                let offset = y.checked_mul(stride).ok_or_else(|| {
                                    CameraError::invalid_frame("UVC row overflow".into())
                                })?;
                                let src =
                                    data.get(offset..offset.saturating_add(packed)).ok_or_else(
                                        || CameraError::invalid_frame("Truncated UVC row".into()),
                                    )?;
                                if channels == 1 {
                                    for (out, &gray) in
                                        row.as_chunks_mut::<3>().0.iter_mut().zip(src)
                                    {
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
                        _ => Err(CameraError::unsupported_format(format!(
                            "{:?}",
                            frame.frame_format
                        ))),
                    },
                )
            }
        }));
        match result {
            Ok(Err(error)) => hub.log_frame_error("UVC", &error),
            Err(_) => log::error!("UVC callback panicked; frame discarded"),
            _ => {}
        }
    }

    /// Decode MJPEG for the optional legacy pixel adapter.
    #[cfg(feature = "decode-mjpeg")]
    #[allow(dead_code)]
    fn decode_mjpeg(jpeg_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        let mut decompressor = Decompressor::new()
            .map_err(|e| CameraError::invalid_frame(e.to_string()).with_source(e))?;

        // Read the JPEG header.
        let header = decompressor
            .read_header(jpeg_data)
            .map_err(|e| CameraError::invalid_frame(e.to_string()).with_source(e))?;

        // Validate the dimensions.
        if header.width != width as usize || header.height != height as usize {
            log::warn!(
                "JPEG size mismatch: expected {}x{}, actual {}x{}",
                width,
                height,
                header.width,
                header.height
            );
        }

        // Prepare the output buffer.
        let mut image = Image {
            pixels: vec![0; 3 * header.width * header.height],
            width: header.width,
            pitch: 3 * header.width,
            height: header.height,
            format: PixelFormat::RGB,
        };

        // Decode the image.
        decompressor
            .decompress(jpeg_data, image.as_deref_mut())
            .map_err(|e| CameraError::invalid_frame(e.to_string()).with_source(e))?;

        Ok(image.pixels)
    }

    /// Converts YUYV to RGB for the compatibility API.
    #[cfg(feature = "convert-rgb")]
    #[allow(dead_code)]
    fn yuyv_to_rgb(yuyv_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        // Use the optimized color converter.
        ColorConverter::new().yuyv_to_rgb(yuyv_data, width, height)
    }

    /// Converts UYVY to RGB for the compatibility API.
    #[cfg(feature = "convert-rgb")]
    #[allow(dead_code)]
    fn uyvy_to_rgb(uyvy_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        // Use the optimized color converter.
        ColorConverter::new().uyvy_to_rgb(uyvy_data, width, height)
    }

    /// Converts the public format enum into a libuvc format.
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
            Err(CameraError::permission_denied(
                "Enumerate USB devices with camera-android and pass an authorized BorrowedFd"
                    .into(),
            ))
        }

        // Non-Android platforms use libuvc discovery.
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

            // Drop devices before the context to avoid
            // "device still referenced at libusb_exit".
            drop(devices);
            drop(ctx);

            Ok(device_infos)
        }
    }

    fn get_supported_configs(device_index: u32) -> CameraResult<Vec<CameraConfig>> {
        // Open the device and query its supported formats.
        let mut ctx = UvcContext::new()?;
        let devices = ctx.get_device_list()?;

        let device = devices.get(device_index as usize).ok_or_else(|| {
            CameraError::device_not_found(format!("Device index {} not found", device_index))
        })?;

        let devh = device.open()?;

        let configs = Self::supported_configs_from_handle(devh.as_ptr());

        // Release temporary discovery resources.
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

    /// Finds a device index by unique ID.
    ///
    /// IDs use `vendor_id:product_id:serial_number` or `vendor_id:product_id:index`.
    ///
    /// # Parameters
    /// - `unique_id`: Unique device identifier.
    ///
    /// # Returns
    /// The matching device index and device information.
    pub fn find_device_by_unique_id(unique_id: &str) -> CameraResult<(u32, CameraDeviceInfo)> {
        let devices = Self::list_devices()?;

        // Try an exact match first.
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

        // If an exact match fails, parse the ID and try a partial match using
        // `vendor_id:product_id:serial_or_index`.
        let parts: Vec<&str> = unique_id.split(':').collect();
        if parts.len() >= 2 {
            let target_vid = u16::from_str_radix(parts[0], 16).ok();
            let target_pid = u16::from_str_radix(parts[1], 16).ok();

            if let (Some(vid), Some(pid)) = (target_vid, target_pid) {
                // Find devices with the same VID and PID.
                let matching_devices: Vec<_> = devices
                    .iter()
                    .filter(|d| d.vendor_id == Some(vid) && d.product_id == Some(pid))
                    .collect();

                if matching_devices.len() == 1 {
                    // A single matching device is unambiguous.
                    let device = matching_devices[0];
                    log::info!(
                        "Found single device with VID:PID {:04x}:{:04x}: device {}",
                        vid,
                        pid,
                        device.index
                    );
                    return Ok((device.index, device.clone()));
                } else if !matching_devices.is_empty() {
                    // Disambiguate multiple matches by serial number.
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

                    // The device cannot be identified unambiguously.
                    log::warn!(
                        "Found {} devices with VID:PID {:04x}:{:04x}, but cannot match unique_id {}",
                        matching_devices.len(), vid, pid, unique_id
                    );
                }
            }
        }

        Err(CameraError::device_not_found(format!(
            "No device found with unique_id: {}",
            unique_id
        )))
    }

    /// Opens a camera by unique ID or index.
    ///
    /// # Parameters
    /// - `unique_id`: Optional unique ID. The index is used when this is `None`.
    /// - `device_index`: Device index used when `unique_id` is `None`.
    ///
    /// # Returns
    /// The camera instance and its device information.
    pub fn open_by_unique_id_or_index(
        unique_id: Option<&str>,
        device_index: u32,
    ) -> CameraResult<(Self, CameraDeviceInfo)> {
        let (actual_index, device_info) = if let Some(id) = unique_id {
            // Find the device by unique ID.
            log::info!("Opening camera by unique_id: {}", id);
            Self::find_device_by_unique_id(id)?
        } else {
            // Find the device by index.
            log::info!("Opening camera by index: {}", device_index);
            let devices = Self::list_devices()?;
            let device = devices
                .into_iter()
                .find(|d| d.index == device_index)
                .ok_or_else(|| {
                    CameraError::device_not_found(format!(
                        "Device index {} not found",
                        device_index
                    ))
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
            .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }

    fn start_sync(&self, mut config: CameraConfig) -> CameraResult<()> {
        let _lifecycle = self.lifecycle.lock();
        config.validate()?;

        if self.capture.hub.is_streaming() {
            return Err(CameraError::stream_error(
                "Stream already running".to_string(),
            ));
        }

        // Initialize the context and device.
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
                        .ok_or_else(|| CameraError::device_not_found(expected.clone()))?;
                    if matches.next().is_some() {
                        return Err(CameraError::ambiguous_device(expected.clone()));
                    }
                    Some(found)
                } else {
                    devices.get(self.device_index as usize)
                };
                let device = selected.ok_or_else(|| {
                    CameraError::device_not_found(format!(
                        "Device index {} does not exist",
                        self.device_index
                    ))
                })?;

                let devh = device.open()?;

                // Explicitly drop devices before storing the context to avoid
                // "device still referenced at libusb_exit".
                drop(devices);

                *self.device_handle.lock() = Some(devh);
                *ctx_guard = Some(ctx);
            }
        }

        // Configure the stream.
        let mut devh_guard = self.device_handle.lock();
        let devh = devh_guard
            .as_mut()
            .ok_or_else(|| CameraError::device_open_failed("Device not open".to_string()))?;

        // Log supported formats for diagnostics.
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
                .map_err(|_| CameraError::invalid_config("UVC frame interval overflow".into()))?,
        );
        if ctrl.frame_interval() == 0 {
            return Err(CameraError::invalid_config(
                "UVC frame interval is zero".into(),
            ));
        }
        devh.probe_stream_ctrl(&mut ctrl)?;
        config = config.with_frame_rate(crate::FrameRate::new(10_000_000, ctrl.frame_interval())?);

        let session = self.capture.hub.start();
        #[cfg(not(feature = "decode-mjpeg"))]
        if config.format == VideoFormat::MJPEG && !self.capture.hub.wants_native() {
            return Err(CameraError::unsupported_format(
                "MJPEG decoding requires the decode-mjpeg feature".into(),
            ));
        }
        #[cfg(feature = "decode-mjpeg")]
        let (mjpeg, mjpeg_worker) =
            if config.format == VideoFormat::MJPEG && !self.capture.hub.wants_native() {
                let (dispatch, worker) = Self::start_mjpeg_worker(self.capture.clone(), session)?;
                (Some(dispatch), Some(worker))
            } else {
                (None, None)
            };
        let mut context = Box::new(CallbackContext {
            state: self.capture.clone(),
            session,
            #[cfg(feature = "decode-mjpeg")]
            mjpeg,
        });
        let user_ptr = context.as_mut() as *mut CallbackContext as *mut c_void;
        if let Err(error) =
            unsafe { devh.start_streaming(&mut ctrl, Self::frame_callback, user_ptr) }
        {
            self.capture.hub.stop();
            devh.stop_streaming();
            drop(context);
            #[cfg(feature = "decode-mjpeg")]
            if let Some(worker) = mjpeg_worker {
                let _ = worker.join();
            }
            return Err(error);
        }
        *self.callback.lock() = Some(context);
        #[cfg(feature = "decode-mjpeg")]
        {
            *self.mjpeg_worker.lock() = mjpeg_worker;
        }
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
        .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))
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
        Err(CameraError::invalid_config(
            "Frame pool capacity is fixed for the lifetime of the camera".into(),
        ))
    }
}

// ============================================================================
// Camera control implementation.
// ============================================================================

impl CameraControl for UvcCamera {
    fn get_control(&self, control: CameraControlType) -> CameraResult<CameraControlValue> {
        let devh = self.device_handle.lock();
        let devh = devh
            .as_ref()
            .ok_or_else(|| CameraError::device_open_failed("Device not open".to_string()))?
            .as_ptr();

        use ffi::UvcReqCode;
        let req_code = UvcReqCode::GetCur as u8;

        match control {
            CameraControlType::AutoExposure => {
                let mut mode = 0u8;
                let result = unsafe { ffi::uvc_get_ae_mode(devh, &mut mode, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get auto exposure mode".to_string(),
                    ));
                }
                Ok(CameraControlValue::Boolean(matches!(mode, 2 | 4 | 8)))
            }
            CameraControlType::Exposure => {
                let mut time = 0u32;
                let result = unsafe { ffi::uvc_get_exposure_abs(devh, &mut time, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get exposure time".to_string(),
                    ));
                }
                Ok(CameraControlValue::manual(time as i32))
            }
            CameraControlType::Focus => {
                let mut focus = 0u16;
                let result = unsafe { ffi::uvc_get_focus_abs(devh, &mut focus, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(result, "Failed to get focus".to_string()));
                }
                Ok(CameraControlValue::manual(focus as i32))
            }
            CameraControlType::AutoFocus => {
                let mut state = 0u8;
                let result = unsafe { ffi::uvc_get_focus_auto(devh, &mut state, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get auto focus state".to_string(),
                    ));
                }
                Ok(CameraControlValue::Boolean(state != 0))
            }
            CameraControlType::Zoom => {
                let mut zoom = 0u16;
                let result = unsafe { ffi::uvc_get_zoom_abs(devh, &mut zoom, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(result, "Failed to get zoom".to_string()));
                }
                Ok(CameraControlValue::manual(zoom as i32))
            }
            CameraControlType::Brightness => {
                let mut brightness = 0i16;
                let result = unsafe { ffi::uvc_get_brightness(devh, &mut brightness, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get brightness".to_string(),
                    ));
                }
                Ok(CameraControlValue::manual(brightness as i32))
            }
            CameraControlType::Contrast => {
                let mut contrast = 0u16;
                let result = unsafe { ffi::uvc_get_contrast(devh, &mut contrast, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get contrast".to_string(),
                    ));
                }
                Ok(CameraControlValue::manual(contrast as i32))
            }
            CameraControlType::Saturation => {
                let mut saturation = 0u16;
                let result = unsafe { ffi::uvc_get_saturation(devh, &mut saturation, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get saturation".to_string(),
                    ));
                }
                Ok(CameraControlValue::manual(saturation as i32))
            }
            CameraControlType::Hue => {
                let mut hue = 0i16;
                let result = unsafe { ffi::uvc_get_hue(devh, &mut hue, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(result, "Failed to get hue".to_string()));
                }
                Ok(CameraControlValue::manual(hue as i32))
            }
            CameraControlType::Sharpness => {
                let mut sharpness = 0u16;
                let result = unsafe { ffi::uvc_get_sharpness(devh, &mut sharpness, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get sharpness".to_string(),
                    ));
                }
                Ok(CameraControlValue::manual(sharpness as i32))
            }
            CameraControlType::Gamma => {
                let mut gamma = 0u16;
                let result = unsafe { ffi::uvc_get_gamma(devh, &mut gamma, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(result, "Failed to get gamma".to_string()));
                }
                Ok(CameraControlValue::manual(gamma as i32))
            }
            CameraControlType::WhiteBalance => {
                let mut temp = 0u16;
                let result =
                    unsafe { ffi::uvc_get_white_balance_temperature(devh, &mut temp, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get white balance temperature".to_string(),
                    ));
                }
                Ok(CameraControlValue::manual(temp as i32))
            }
            CameraControlType::AutoWhiteBalance => {
                let mut state = 0u8;
                let result = unsafe {
                    ffi::uvc_get_white_balance_temperature_auto(devh, &mut state, req_code)
                };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get auto white balance state".to_string(),
                    ));
                }
                Ok(CameraControlValue::Boolean(state != 0))
            }
            CameraControlType::Gain => {
                let mut gain = 0u16;
                let result = unsafe { ffi::uvc_get_gain(devh, &mut gain, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(result, "Failed to get gain".to_string()));
                }
                Ok(CameraControlValue::manual(gain as i32))
            }
            CameraControlType::BacklightCompensation => {
                let mut comp = 0u16;
                let result =
                    unsafe { ffi::uvc_get_backlight_compensation(devh, &mut comp, req_code) };
                if result != ffi::UVC_SUCCESS {
                    return Err(CameraError::uvc(
                        result,
                        "Failed to get backlight compensation".to_string(),
                    ));
                }
                Ok(CameraControlValue::manual(comp as i32))
            }
            CameraControlType::Pan | CameraControlType::Tilt | CameraControlType::Iris => {
                // libuvc does not expose these controls directly.
                Err(CameraError::control_not_supported(format!("{:?}", control)))
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
            .ok_or_else(|| CameraError::device_open_failed("Device not open".to_string()))?
            .as_ptr();

        let result =
            match control {
                CameraControlType::AutoExposure => {
                    let enabled = value.as_bool().ok_or_else(|| {
                        CameraError::invalid_config("Auto exposure expects boolean".into())
                    })?;
                    let mut mask = 0u8;
                    let code = unsafe {
                        ffi::uvc_get_ae_mode(devh, &mut mask, ffi::UvcReqCode::GetRes as u8)
                    };
                    if code != ffi::UVC_SUCCESS {
                        return Err(CameraError::uvc(code, "Query AE supported modes".into()));
                    }
                    let mode = select_ae_mode(mask, enabled)?;
                    unsafe { ffi::uvc_set_ae_mode(devh, mode) }
                }
                CameraControlType::Exposure => {
                    let time = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid exposure time value".to_string())
                    })? as u32;
                    unsafe { ffi::uvc_set_exposure_abs(devh, time) }
                }
                CameraControlType::Focus => {
                    let focus = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid focus value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_focus_abs(devh, focus) }
                }
                CameraControlType::AutoFocus => {
                    let state = if value.as_bool().ok_or_else(|| {
                        CameraError::invalid_config("Invalid auto focus value".to_string())
                    })? {
                        1u8
                    } else {
                        0u8
                    };
                    unsafe { ffi::uvc_set_focus_auto(devh, state) }
                }
                CameraControlType::Zoom => {
                    let zoom = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid zoom value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_zoom_abs(devh, zoom) }
                }
                CameraControlType::Brightness => {
                    let brightness = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid brightness value".to_string())
                    })? as i16;
                    unsafe { ffi::uvc_set_brightness(devh, brightness) }
                }
                CameraControlType::Contrast => {
                    let contrast = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid contrast value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_contrast(devh, contrast) }
                }
                CameraControlType::Saturation => {
                    let saturation = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid saturation value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_saturation(devh, saturation) }
                }
                CameraControlType::Hue => {
                    let hue = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid hue value".to_string())
                    })? as i16;
                    unsafe { ffi::uvc_set_hue(devh, hue) }
                }
                CameraControlType::Sharpness => {
                    let sharpness = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid sharpness value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_sharpness(devh, sharpness) }
                }
                CameraControlType::Gamma => {
                    let gamma = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid gamma value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_gamma(devh, gamma) }
                }
                CameraControlType::WhiteBalance => {
                    let temp = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config(
                            "Invalid white balance temperature value".to_string(),
                        )
                    })? as u16;
                    unsafe { ffi::uvc_set_white_balance_temperature(devh, temp) }
                }
                CameraControlType::AutoWhiteBalance => {
                    let state = if value.as_bool().ok_or_else(|| {
                        CameraError::invalid_config("Invalid auto white balance value".to_string())
                    })? {
                        1u8
                    } else {
                        0u8
                    };
                    unsafe { ffi::uvc_set_white_balance_temperature_auto(devh, state) }
                }
                CameraControlType::Gain => {
                    let gain = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Invalid gain value".to_string())
                    })? as u16;
                    unsafe { ffi::uvc_set_gain(devh, gain) }
                }
                CameraControlType::BacklightCompensation => {
                    let comp = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config(
                            "Invalid backlight compensation value".to_string(),
                        )
                    })? as u16;
                    unsafe { ffi::uvc_set_backlight_compensation(devh, comp) }
                }
                CameraControlType::Pan | CameraControlType::Tilt | CameraControlType::Iris => {
                    // libuvc does not expose these controls directly.
                    return Err(CameraError::control_not_supported(format!("{:?}", control)));
                }
            };

        if result != ffi::UVC_SUCCESS {
            Err(CameraError::uvc(
                result,
                format!("Failed to set control parameter: {:?}", control),
            ))
        } else {
            Ok(())
        }
    }

    fn get_control_range(&self, control: CameraControlType) -> CameraResult<CameraControlRange> {
        let devh = self.device_handle.lock();
        let devh = devh
            .as_ref()
            .ok_or_else(|| CameraError::device_open_failed("Device not open".to_string()))?
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
                return Err(CameraError::uvc(code, format!("Query {control:?} default")));
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
                        return Err(CameraError::uvc(
                            code,
                            format!("Query {control:?} {request:?}"),
                        ));
                    }
                    values[i] = i32::try_from(raw).map_err(|_| {
                        CameraError::invalid_frame("UVC control range exceeds i32".into())
                    })?;
                }
                if values[0] > values[1] || values[2] <= 0 {
                    return Err(CameraError::invalid_frame(
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
            _ => Err(CameraError::control_not_supported(format!("{control:?}"))),
        }
    }

    fn supports_control(&self, control: CameraControlType) -> bool {
        // A successfully queried value indicates control support.
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
            CameraError::control_not_supported("Requested UVC exposure mode is unavailable".into())
        })
}

impl Drop for UvcCamera {
    fn drop(&mut self) {
        log::debug!("UvcCamera::drop() 调用");

        // cleanup also stops an active stream.
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

#[cfg(all(test, feature = "decode-mjpeg"))]
mod capture_contract_tests {
    use super::*;

    #[test]
    fn mjpeg_worker_publishes_after_the_callback_queue_releases_its_buffer() {
        let camera = UvcCamera::new(0).unwrap();
        let session = camera.capture.hub.start();
        let pixels = vec![128u8; 16 * 16 * 3];
        let jpeg = turbojpeg::compress(
            turbojpeg::Image {
                pixels: pixels.as_slice(),
                width: 16,
                height: 16,
                pitch: 16 * 3,
                format: turbojpeg::PixelFormat::RGB,
            },
            90,
            turbojpeg::Subsamp::None,
        )
        .unwrap();
        let (dispatch, worker) =
            UvcCamera::start_mjpeg_worker(camera.capture.clone(), session).unwrap();
        dispatch
            .sender
            .send(MjpegJob {
                data: jpeg.to_vec(),
                width: 16,
                height: 16,
                sequence: 7,
            })
            .unwrap();
        drop(dispatch);
        worker.join().unwrap();

        let frame = camera.capture.hub.latest().unwrap();
        assert_eq!(frame.key.session, session);
        assert_eq!(frame.source_sequence, Some(7));
        assert_eq!(frame.bytes().len(), 16 * 16 * 3);
    }

    #[test]
    fn mjpeg_worker_skips_a_truncated_frame_and_decodes_the_next_complete_frame() {
        let camera = UvcCamera::new(0).unwrap();
        let session = camera.capture.hub.start();
        let pixels = vec![64u8; 16 * 16 * 3];
        let jpeg = turbojpeg::compress(
            turbojpeg::Image {
                pixels: pixels.as_slice(),
                width: 16,
                height: 16,
                pitch: 16 * 3,
                format: turbojpeg::PixelFormat::RGB,
            },
            90,
            turbojpeg::Subsamp::None,
        )
        .unwrap();
        let (dispatch, worker) =
            UvcCamera::start_mjpeg_worker(camera.capture.clone(), session).unwrap();
        dispatch
            .sender
            .send(MjpegJob {
                data: jpeg[..jpeg.len() / 4].to_vec(),
                width: 16,
                height: 16,
                sequence: 7,
            })
            .unwrap();
        dispatch
            .sender
            .send(MjpegJob {
                data: jpeg.to_vec(),
                width: 16,
                height: 16,
                sequence: 8,
            })
            .unwrap();
        drop(dispatch);
        worker.join().unwrap();

        let frame = camera.capture.hub.latest().unwrap();
        assert_eq!(frame.source_sequence, Some(8));
        let metrics = camera.capture.hub.metrics();
        assert_eq!(metrics.received, 2);
        assert_eq!(metrics.published, 1);
        assert_eq!(metrics.conversion_errors, 1);
        assert_eq!(metrics.consecutive_conversion_errors, 0);
    }

    #[test]
    fn callback_owns_capture_state_without_retaining_the_camera() {
        let camera = Arc::new(UvcCamera::new(0).unwrap());
        let weak = Arc::downgrade(&camera);
        let state = Arc::downgrade(&camera.capture);
        let session = camera.capture.hub.start();
        *camera.callback.lock() = Some(Box::new(CallbackContext {
            state: camera.capture.clone(),
            session,
            mjpeg: None,
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
            mjpeg: None,
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
