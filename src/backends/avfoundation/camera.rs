//! AVFoundation camera implementation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::pixels::Pixels as Array3;

use crate::error::CameraError;
use crate::traits::{CameraControl, CameraManager, StreamStats, StreamingCamera};
use crate::types::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraResult,
};

use super::capture::CaptureSession;
use super::device::{get_supported_configs, query_devices};

/// AVFoundation camera implementation.
///
/// Provides native camera access on macOS and iOS.
pub struct AVFoundationCamera {
    /// Device index.
    device_id: String,
    hub: Arc<crate::FrameHub>,
    lifecycle: Mutex<()>,
    /// Capture session.
    session: Mutex<Option<CaptureSession>>,
    /// Whether streaming is active.
    is_streaming: Arc<AtomicBool>,
    /// Current configuration.
    current_config: Mutex<Option<CameraConfig>>,
    /// Start time.
    start_time: Mutex<Option<Instant>>,
    discard_late_frames: AtomicBool,
}

impl AVFoundationCamera {
    /// Creates an AVFoundation camera instance.
    pub fn new(device_index: u32) -> CameraResult<Self> {
        Self::new_with_hub(device_index, crate::FrameHub::default())
    }
    pub(crate) fn new_with_hub(device_index: u32, hub: crate::FrameHub) -> CameraResult<Self> {
        Self::new_selected(device_index, hub, None)
    }
    pub(crate) fn new_selected(
        device_index: u32,
        hub: crate::FrameHub,
        expected: Option<&str>,
    ) -> CameraResult<Self> {
        log::info!("Creating AVFoundationCamera for device {}", device_index);

        // Verify that the device exists.
        let devices = query_devices()?;
        if device_index as usize >= devices.len() {
            return Err(CameraError::device_not_found(format!(
                "Device index {} not found, only {} devices available",
                device_index,
                devices.len()
            )));
        }

        let device_id = expected
            .map(str::to_owned)
            .unwrap_or_else(|| devices[device_index as usize].unique_id());
        super::device::get_device_by_id(&device_id)?;
        Ok(Self {
            device_id,
            hub: Arc::new(hub),
            lifecycle: Mutex::new(()),
            session: Mutex::new(None),
            is_streaming: Arc::new(AtomicBool::new(false)),
            current_config: Mutex::new(None),
            start_time: Mutex::new(None),
            discard_late_frames: AtomicBool::new(true),
        })
    }

    pub(crate) fn set_options(&self, options: crate::AvFoundationOptions) -> CameraResult<()> {
        if self.is_streaming.load(Ordering::Acquire) {
            return Err(CameraError::invalid_state(
                "AVFoundation options must be set before streaming".into(),
            ));
        }
        self.discard_late_frames.store(
            options.late_frames == crate::LateFramePolicy::Drop,
            Ordering::Release,
        );
        Ok(())
    }

    pub(crate) fn supported_modes(&self, control: CameraControlType) -> CameraResult<Vec<i32>> {
        let session = self.session.lock().unwrap();
        let device = session
            .as_ref()
            .ok_or(CameraError::stream_stopped())?
            .device();
        Ok((0..=2)
            .filter(|&mode| unsafe {
                let mode = mode as isize;
                match control {
                    CameraControlType::Focus => objc2::msg_send![device, isFocusModeSupported:mode],
                    CameraControlType::Exposure => {
                        objc2::msg_send![device, isExposureModeSupported:mode]
                    }
                    CameraControlType::WhiteBalance => {
                        objc2::msg_send![device, isWhiteBalanceModeSupported:mode]
                    }
                    _ => false,
                }
            })
            .collect())
    }
    /// Requests camera permission.
    pub async fn request_permission() -> CameraResult<bool> {
        Ok(super::device::request_authorization().await)
    }

    /// Returns the current permission status.
    pub fn authorization_status() -> super::device::AVAuthorizationStatus {
        super::device::authorization_status()
    }

    pub(crate) fn permission_status() -> crate::PermissionStatus {
        match Self::authorization_status() {
            super::device::AVAuthorizationStatus::NotDetermined => {
                crate::PermissionStatus::NotDetermined
            }
            super::device::AVAuthorizationStatus::Restricted => crate::PermissionStatus::Restricted,
            super::device::AVAuthorizationStatus::Denied => crate::PermissionStatus::Denied,
            super::device::AVAuthorizationStatus::Authorized => crate::PermissionStatus::Authorized,
        }
    }

    /// Releases resources.
    pub fn cleanup(&self) {
        let _guard = self.lifecycle.lock().unwrap();
        self.hub.stop();

        self.is_streaming.store(false, Ordering::SeqCst);

        if let Ok(mut session) = self.session.lock() {
            if let Some(s) = session.take() {
                let _ = s.stop();
            }
        }

        *self.current_config.lock().unwrap() = None;
        *self.start_time.lock().unwrap() = None;
    }
}

impl CameraManager for AVFoundationCamera {
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
        query_devices()
    }

    fn get_supported_configs(device_index: u32) -> CameraResult<Vec<CameraConfig>> {
        use super::device::get_device_by_index;

        let device = get_device_by_index(device_index)?;
        get_supported_configs(&device)
    }

    fn is_config_supported(device_index: u32, config: &CameraConfig) -> CameraResult<bool> {
        let configs = Self::get_supported_configs(device_index)?;

        Ok(configs.iter().any(|c| {
            c.width == config.width
                && c.height == config.height
                && c.frame_rate().ok() == config.frame_rate().ok()
        }))
    }
}

impl StreamingCamera for AVFoundationCamera {
    fn frame_hub(&self) -> Option<&crate::FrameHub> {
        Some(&self.hub)
    }
    async fn start_stream(&self, config: CameraConfig) -> CameraResult<()> {
        self.start_stream_sync(config)
    }

    async fn stop_stream(&self) -> CameraResult<()> {
        self.stop_stream_sync()
    }

    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>> {
        Ok(self.hub.latest().map(|f| {
            f.rgb_pixels()
                .expect("RGB backend adapter")
                .as_ref()
                .clone()
        }))
    }
    async fn wait_for_frame(&self, timeout: Duration) -> CameraResult<Array3<u8>> {
        Ok((*self.hub.wait_after(None, timeout).await?.rgb_pixels()?)
            .as_ref()
            .clone())
    }

    fn is_streaming(&self) -> bool {
        self.is_streaming.load(Ordering::Relaxed)
    }

    fn get_config(&self) -> Option<CameraConfig> {
        self.current_config.lock().unwrap().clone()
    }

    fn get_stats(&self) -> StreamStats {
        self.hub.stats()
    }

    async fn set_buffer_size(&self, _size: usize) -> CameraResult<()> {
        Err(CameraError::invalid_config(
            "Frame pool capacity is fixed for the lifetime of the camera".into(),
        ))
    }
}

impl CameraControl for AVFoundationCamera {
    fn get_control(&self, control: CameraControlType) -> CameraResult<CameraControlValue> {
        let session = self.session.lock().unwrap();

        if let Some(ref s) = *session {
            let device = s.device();

            match control {
                CameraControlType::Brightness => {
                    // AVFoundation represents brightness as exposureTargetBias.
                    let bias: f32 = unsafe { objc2::msg_send![device, exposureTargetBias] };
                    Ok(CameraControlValue::manual((bias * 1000.0).round() as i32))
                }
                CameraControlType::Focus => {
                    // focusMode returns NSInteger (isize).
                    let mode: isize = unsafe { objc2::msg_send![device, focusMode] };
                    Ok(CameraControlValue::manual(mode as i32))
                }
                CameraControlType::Exposure => {
                    let mode: isize = unsafe { objc2::msg_send![device, exposureMode] };
                    Ok(CameraControlValue::manual(mode as i32))
                }
                CameraControlType::WhiteBalance => {
                    let mode: isize = unsafe { objc2::msg_send![device, whiteBalanceMode] };
                    Ok(CameraControlValue::manual(mode as i32))
                }
                CameraControlType::Zoom => {
                    let factor: f64 = unsafe { objc2::msg_send![device, videoZoomFactor] };
                    Ok(CameraControlValue::manual((factor * 100.0) as i32))
                }
                _ => Err(CameraError::control_not_supported(format!("{:?}", control))),
            }
        } else {
            Err(CameraError::stream_error("No active session".into()))
        }
    }

    fn set_control(
        &self,
        control: CameraControlType,
        value: CameraControlValue,
    ) -> CameraResult<()> {
        let session = self.session.lock().unwrap();

        if let Some(ref s) = *session {
            let device = s.device();

            // Lock the device before changing its configuration.
            unsafe {
                device.lockForConfiguration().map_err(|e| {
                    CameraError::backend_failure(
                        crate::BackendId::AV_FOUNDATION,
                        crate::OperationStage::BackendCommand,
                        format!("Failed to lock device: {e:?}"),
                    )
                })?;
            }

            struct Unlock<'a>(&'a objc2_av_foundation::AVCaptureDevice);
            impl Drop for Unlock<'_> {
                fn drop(&mut self) {
                    unsafe {
                        self.0.unlockForConfiguration();
                    }
                }
            }
            let _lock = Unlock(device);
            let result = match control {
                CameraControlType::Brightness => {
                    let bias = value.as_i32().ok_or_else(|| {
                        CameraError::invalid_config("Exposure bias expects milliev".into())
                    })? as f32
                        / 1000.0;
                    unsafe {
                        let _: () = objc2::msg_send![device, setExposureTargetBias:bias, completionHandler:std::ptr::null::<std::ffi::c_void>()];
                    }
                    Ok(())
                }
                CameraControlType::Focus => {
                    let mode = value.as_i32().unwrap_or(0) as isize;
                    let supported: bool =
                        unsafe { objc2::msg_send![device,isFocusModeSupported:mode] };
                    if !supported {
                        return Err(CameraError::control_not_supported(
                            "Native mode unavailable".into(),
                        ));
                    }
                    unsafe {
                        let _: () = objc2::msg_send![device, setFocusMode: mode];
                    }
                    Ok(())
                }
                CameraControlType::Exposure => {
                    let mode = value.as_i32().unwrap_or(0) as isize;
                    let supported: bool =
                        unsafe { objc2::msg_send![device,isExposureModeSupported:mode] };
                    if !supported {
                        return Err(CameraError::control_not_supported(
                            "Native mode unavailable".into(),
                        ));
                    }
                    unsafe {
                        let _: () = objc2::msg_send![device, setExposureMode: mode];
                    }
                    Ok(())
                }
                CameraControlType::WhiteBalance => {
                    let mode = value.as_i32().unwrap_or(0) as isize;
                    let supported: bool =
                        unsafe { objc2::msg_send![device,isWhiteBalanceModeSupported:mode] };
                    if !supported {
                        return Err(CameraError::control_not_supported(
                            "Native mode unavailable".into(),
                        ));
                    }
                    unsafe {
                        let _: () = objc2::msg_send![device, setWhiteBalanceMode: mode];
                    }
                    Ok(())
                }
                CameraControlType::Zoom => {
                    // videoZoomFactor is CGFloat (f64 on 64-bit targets).
                    let factor = value.as_i32().unwrap_or(100) as f64 / 100.0;
                    unsafe {
                        let _: () = objc2::msg_send![device, setVideoZoomFactor: factor];
                    }
                    Ok(())
                }
                _ => Err(CameraError::control_not_supported(format!("{:?}", control))),
            };

            result
        } else {
            Err(CameraError::stream_error("No active session".into()))
        }
    }

    fn get_control_range(&self, control: CameraControlType) -> CameraResult<CameraControlRange> {
        let session = self.session.lock().unwrap();

        if let Some(ref s) = *session {
            let device = s.device();

            match control {
                CameraControlType::Brightness => {
                    let min: f32 = unsafe { objc2::msg_send![device, minExposureTargetBias] };
                    let max: f32 = unsafe { objc2::msg_send![device, maxExposureTargetBias] };
                    Ok(CameraControlRange {
                        min: (min * 1000.0).ceil() as i32,
                        max: (max * 1000.0).floor() as i32,
                        step: 1,
                        default: 0,
                        supports_auto: true,
                    })
                }
                CameraControlType::Zoom => {
                    // videoZoomFactor returns CGFloat (f64 on 64-bit targets).
                    let min: f64 = unsafe { objc2::msg_send![device, minAvailableVideoZoomFactor] };
                    let max: f64 = unsafe { objc2::msg_send![device, maxAvailableVideoZoomFactor] };
                    Ok(CameraControlRange {
                        min: (min * 100.0) as i32,
                        max: (max * 100.0) as i32,
                        step: 1,
                        default: 100,
                        supports_auto: false,
                    })
                }
                _ => Err(CameraError::control_not_supported(format!("{:?}", control))),
            }
        } else {
            Err(CameraError::stream_error("No active session".into()))
        }
    }

    fn supports_control(&self, control: CameraControlType) -> bool {
        matches!(
            control,
            CameraControlType::Focus
                | CameraControlType::Exposure
                | CameraControlType::WhiteBalance
                | CameraControlType::Zoom
                | CameraControlType::Brightness
        )
    }

    fn get_supported_controls(&self) -> Vec<CameraControlType> {
        vec![
            CameraControlType::Focus,
            CameraControlType::Exposure,
            CameraControlType::WhiteBalance,
            CameraControlType::Zoom,
            CameraControlType::Brightness,
        ]
    }
}

// SAFETY: AVFoundationCamera uses thread-safe synchronization primitives.
unsafe impl Send for AVFoundationCamera {}
unsafe impl Sync for AVFoundationCamera {}

impl Drop for AVFoundationCamera {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl AVFoundationCamera {
    fn start_stream_sync(&self, config: CameraConfig) -> CameraResult<()> {
        let _guard = self.lifecycle.lock().unwrap();
        log::info!("Starting stream with config: {:?}", config);

        // Reject duplicate starts.
        if self.is_streaming.load(Ordering::Relaxed) {
            log::warn!("Stream is already running");
            return Ok(());
        }

        // Validate the configuration.
        config.validate()?;

        // Create the capture session.
        self.hub.start();
        let mut session = match CaptureSession::new(
            &self.device_id,
            config.clone(),
            self.hub.clone(),
            self.discard_late_frames.load(Ordering::Acquire),
        ) {
            Ok(session) => session,
            Err(error) => {
                self.hub.stop();
                return Err(error);
            }
        };

        // Start the session.
        if let Err(error) = session.start() {
            self.hub.stop();
            return Err(error);
        }

        // Update lifecycle state.
        self.is_streaming.store(true, Ordering::SeqCst);
        *self.current_config.lock().unwrap() = Some(session.config().clone());
        *self.session.lock().unwrap() = Some(session);
        *self.start_time.lock().unwrap() = Some(Instant::now());

        log::info!("Stream started successfully");
        Ok(())
    }
    pub async fn start_stream_arc(self: &Arc<Self>, config: CameraConfig) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || camera.start_stream_sync(config))
            .await
            .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }
    fn stop_stream_sync(&self) -> CameraResult<()> {
        let _guard = self.lifecycle.lock().unwrap();
        self.hub.stop();

        self.is_streaming.store(false, Ordering::SeqCst);

        if let Ok(mut session) = self.session.lock() {
            if let Some(s) = session.take() {
                s.stop()?;
            }
        }

        *self.current_config.lock().unwrap() = None;

        log::info!("Stream stopped");
        Ok(())
    }
    pub async fn stop_stream_arc(self: &Arc<Self>) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || camera.stop_stream_sync())
            .await
            .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        let result = AVFoundationCamera::list_devices();
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_camera() {
        // This can fail when no camera is available, but must not panic.
        let result = AVFoundationCamera::new(0);
        // Do not require success because the host may have no camera.
        let _ = result;
    }

    #[test]
    fn test_authorization_status() {
        let status = AVFoundationCamera::authorization_status();
        // Verify that the method can be called safely.
        println!("Authorization status: {:?}", status);
    }
}
