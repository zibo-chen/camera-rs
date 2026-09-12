//! MFCamera stub for non-Windows platforms.
//!
//! All methods return a not-implemented error outside Windows.

use crate::error::CameraError;
use crate::pixels::Pixels as Array3;
use crate::traits::{CameraControl, CameraManager, StreamStats, StreamingCamera};
use crate::types::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraResult,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Media Foundation camera stub.
///
/// Used on non-Windows platforms, where every operation returns an appropriate error.
pub struct MFCamera {
    device_index: u32,
    is_streaming: Arc<AtomicBool>,
}

impl MFCamera {
    /// Creates a stub instance.
    pub fn new(device_index: u32) -> CameraResult<Self> {
        Ok(Self {
            device_index,
            is_streaming: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Returns device information.
    pub fn device_info(&self) -> CameraDeviceInfo {
        CameraDeviceInfo {
            index: self.device_index,
            name: "Media Foundation (Not Available)".to_string(),
            description: "This backend is only available on Windows".to_string(),
            vendor_id: None,
            product_id: None,
            serial_number: None,
            device_path: None,
        }
    }

    fn not_implemented<T>() -> CameraResult<T> {
        Err(CameraError::unsupported_target(
            crate::BackendId::MEDIA_FOUNDATION,
            std::env::consts::OS,
        ))
    }
}

impl CameraManager for MFCamera {
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
        Self::not_implemented()
    }

    fn get_supported_configs(_device_index: u32) -> CameraResult<Vec<CameraConfig>> {
        Self::not_implemented()
    }

    fn is_config_supported(_device_index: u32, _config: &CameraConfig) -> CameraResult<bool> {
        Self::not_implemented()
    }
}

impl StreamingCamera for MFCamera {
    async fn start_stream(&self, _config: CameraConfig) -> CameraResult<()> {
        Self::not_implemented()
    }

    async fn stop_stream(&self) -> CameraResult<()> {
        self.is_streaming.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>> {
        Self::not_implemented()
    }

    async fn wait_for_frame(&self, _timeout: Duration) -> CameraResult<Array3<u8>> {
        Self::not_implemented()
    }

    fn is_streaming(&self) -> bool {
        self.is_streaming.load(Ordering::Relaxed)
    }

    fn get_config(&self) -> Option<CameraConfig> {
        None
    }

    fn get_stats(&self) -> StreamStats {
        StreamStats::default()
    }

    async fn set_buffer_size(&self, _size: usize) -> CameraResult<()> {
        Self::not_implemented()
    }
}

impl CameraControl for MFCamera {
    fn get_control(&self, _control: CameraControlType) -> CameraResult<CameraControlValue> {
        Self::not_implemented()
    }

    fn set_control(
        &self,
        _control: CameraControlType,
        _value: CameraControlValue,
    ) -> CameraResult<()> {
        Self::not_implemented()
    }

    fn get_control_range(&self, _control: CameraControlType) -> CameraResult<CameraControlRange> {
        Self::not_implemented()
    }

    fn supports_control(&self, _control: CameraControlType) -> bool {
        false
    }

    fn get_supported_controls(&self) -> Vec<CameraControlType> {
        vec![]
    }
}

unsafe impl Send for MFCamera {}
unsafe impl Sync for MFCamera {}
