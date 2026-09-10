//! AVFoundation 摄像头实现

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

/// AVFoundation 摄像头实现
///
/// 提供 macOS/iOS 平台的原生摄像头访问。
pub struct AVFoundationCamera {
    /// 设备索引
    device_id: String,
    hub: Arc<crate::FrameHub>,
    lifecycle: Mutex<()>,
    /// 捕获会话
    session: Mutex<Option<CaptureSession>>,
    /// 是否正在流
    is_streaming: Arc<AtomicBool>,
    /// 当前配置
    current_config: Mutex<Option<CameraConfig>>,
    /// 启动时间
    start_time: Mutex<Option<Instant>>,
}

impl AVFoundationCamera {
    /// 创建新的 AVFoundation 摄像头实例
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

        // 验证设备存在
        let devices = query_devices()?;
        if device_index as usize >= devices.len() {
            return Err(CameraError::DeviceNotFound(format!(
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
        })
    }

    pub(crate) fn supported_modes(&self, control: CameraControlType) -> CameraResult<Vec<i32>> {
        let session = self.session.lock().unwrap();
        let device = session.as_ref().ok_or(CameraError::StreamStopped)?.device();
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
    /// 请求相机权限
    pub async fn request_permission() -> CameraResult<bool> {
        Ok(super::device::request_authorization().await)
    }

    /// 检查权限状态
    pub fn authorization_status() -> super::device::AVAuthorizationStatus {
        super::device::authorization_status()
    }

    /// 清理资源
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
        Err(CameraError::InvalidConfig(
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
                    // AVFoundation 使用 exposureTargetBias 作为亮度
                    let bias: f32 = unsafe { objc2::msg_send![device, exposureTargetBias] };
                    Ok(CameraControlValue::manual((bias * 1000.0).round() as i32))
                }
                CameraControlType::Focus => {
                    // focusMode 返回 NSInteger (isize)
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
                _ => Err(CameraError::ControlNotSupported(format!("{:?}", control))),
            }
        } else {
            Err(CameraError::StreamError("No active session".into()))
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

            // 锁定设备进行配置
            unsafe {
                device
                    .lockForConfiguration()
                    .map_err(|e| CameraError::Other(format!("Failed to lock device: {:?}", e)))?;
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
                        CameraError::InvalidConfig("Exposure bias expects milliev".into())
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
                        return Err(CameraError::ControlNotSupported(
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
                        return Err(CameraError::ControlNotSupported(
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
                        return Err(CameraError::ControlNotSupported(
                            "Native mode unavailable".into(),
                        ));
                    }
                    unsafe {
                        let _: () = objc2::msg_send![device, setWhiteBalanceMode: mode];
                    }
                    Ok(())
                }
                CameraControlType::Zoom => {
                    // videoZoomFactor 是 CGFloat (f64 on 64-bit)
                    let factor = value.as_i32().unwrap_or(100) as f64 / 100.0;
                    unsafe {
                        let _: () = objc2::msg_send![device, setVideoZoomFactor: factor];
                    }
                    Ok(())
                }
                _ => Err(CameraError::ControlNotSupported(format!("{:?}", control))),
            };

            result
        } else {
            Err(CameraError::StreamError("No active session".into()))
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
                    // videoZoomFactor 返回 CGFloat (f64 on 64-bit)
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
                _ => Err(CameraError::ControlNotSupported(format!("{:?}", control))),
            }
        } else {
            Err(CameraError::StreamError("No active session".into()))
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

// SAFETY: AVFoundationCamera 使用线程安全的同步原语
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

        // 检查是否已在流
        if self.is_streaming.load(Ordering::Relaxed) {
            log::warn!("Stream is already running");
            return Ok(());
        }

        // 验证配置
        config.validate()?;

        // 创建捕获会话
        self.hub.start();
        let mut session =
            match CaptureSession::new(&self.device_id, config.clone(), self.hub.clone()) {
                Ok(session) => session,
                Err(error) => {
                    self.hub.stop();
                    return Err(error);
                }
            };

        // 启动会话
        if let Err(error) = session.start() {
            self.hub.stop();
            return Err(error);
        }

        // 更新状态
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
            .map_err(|e| CameraError::Other(e.to_string()))?
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
            .map_err(|e| CameraError::Other(e.to_string()))?
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
        // 如果没有摄像头，这会失败，但不会 panic
        let result = AVFoundationCamera::new(0);
        // 不断言成功，因为可能没有摄像头
        let _ = result;
    }

    #[test]
    fn test_authorization_status() {
        let status = AVFoundationCamera::authorization_status();
        // 只要能调用就行
        println!("Authorization status: {:?}", status);
    }
}
