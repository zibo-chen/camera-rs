//! Simplified camera interface definitions.
//!
//! The interfaces focus on two core capabilities:
//! 1. Querying supported camera configurations.
//! 2. Processing asynchronous video streams.

use crate::pixels::Pixels as Array3;
use crate::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraResult,
};
use std::time::Duration;

/// Simplified camera management interface.
pub trait CameraManager {
    /// Lists all available camera devices.
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>>;

    /// Returns the configurations supported by a device.
    fn get_supported_configs(device_index: u32) -> CameraResult<Vec<CameraConfig>>;

    /// Checks whether a device supports a specific configuration.
    fn is_config_supported(device_index: u32, config: &CameraConfig) -> CameraResult<bool>;
}

/// Streaming camera interface backed by a ring buffer.
#[allow(async_fn_in_trait)]
pub trait StreamingCamera: Send + Sync {
    /// Shared snapshots supplied by native callback/worker backends.
    fn frame_hub(&self) -> Option<&crate::FrameHub> {
        None
    }

    fn get_shared_frame(&self) -> CameraResult<Option<std::sync::Arc<crate::CapturedFrame>>> {
        self.frame_hub().map(|hub| hub.latest()).ok_or_else(|| {
            crate::CameraError::stream_error("Shared frames unavailable for this backend".into())
        })
    }

    async fn wait_for_shared_frame(
        &self,
        after: Option<crate::FrameKey>,
        timeout: Duration,
    ) -> CameraResult<std::sync::Arc<crate::CapturedFrame>> {
        self.frame_hub()
            .ok_or_else(|| {
                crate::CameraError::stream_error(
                    "Shared frames unavailable for this backend".into(),
                )
            })?
            .wait_after(after, timeout)
            .await
    }
    /// Starts the asynchronous video stream.
    ///
    /// Once started, frames are captured in the background and stored in the ring buffer.
    async fn start_stream(&self, config: CameraConfig) -> CameraResult<()>;

    /// Stops the video stream.
    async fn stop_stream(&self) -> CameraResult<()>;

    /// Returns the latest frame immediately without blocking.
    ///
    /// Returns `None` when the buffer is empty.
    /// The returned `Array3<u8>` contains RGB data shaped as `(height, width, 3)`.
    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>>;

    /// Waits for the next frame until the timeout expires.
    /// The returned `Array3<u8>` contains RGB data shaped as `(height, width, 3)`.
    async fn wait_for_frame(&self, timeout: Duration) -> CameraResult<Array3<u8>>;

    /// Returns whether the stream is running.
    fn is_streaming(&self) -> bool;

    /// Returns the current configuration.
    fn get_config(&self) -> Option<CameraConfig>;

    /// Returns stream statistics.
    fn get_stats(&self) -> StreamStats;

    /// Sets the buffer size by reconfiguring the ring buffer.
    async fn set_buffer_size(&self, size: usize) -> CameraResult<()>;
}

/// Stream statistics.
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Total number of captured frames.
    pub total_frames: u64,
    /// Number of frames dropped because the buffer was full.
    pub dropped_frames: u64,
    /// Current frame rate.
    pub current_fps: f64,
    /// Current buffer capacity.
    pub buffer_size: usize,
    /// Current number of frames in the buffer.
    pub buffered_frames: usize,
    /// Running time in seconds.
    pub uptime_seconds: u64,
}

impl StreamStats {
    /// Calculates the frame drop ratio.
    pub fn drop_rate(&self) -> f64 {
        if self.total_frames == 0 {
            0.0
        } else {
            self.dropped_frames as f64 / self.total_frames as f64
        }
    }

    /// Calculates buffer utilization.
    pub fn buffer_usage(&self) -> f64 {
        if self.buffer_size == 0 {
            0.0
        } else {
            self.buffered_frames as f64 / self.buffer_size as f64
        }
    }
}

// ============================================================================
// Camera control interface.
// ============================================================================

/// Camera control parameter interface.
#[allow(async_fn_in_trait)]
pub trait CameraControl {
    /// Returns the current value of the specified control.
    fn get_control(&self, control: CameraControlType) -> CameraResult<CameraControlValue>;

    /// Sets the specified control value.
    fn set_control(
        &self,
        control: CameraControlType,
        value: CameraControlValue,
    ) -> CameraResult<()>;

    /// Returns the valid range for the specified control.
    fn get_control_range(&self, control: CameraControlType) -> CameraResult<CameraControlRange>;

    /// Checks whether the device supports the specified control.
    fn supports_control(&self, control: CameraControlType) -> bool;

    /// Returns all controls supported by the device.
    fn get_supported_controls(&self) -> Vec<CameraControlType>;

    /// Resets the specified control to its default value.
    fn reset_control(&self, control: CameraControlType) -> CameraResult<()> {
        let range = self.get_control_range(control)?;
        self.set_control(control, CameraControlValue::manual(range.default))
    }

    /// Resets all controls to their default values.
    fn reset_all_controls(&self) -> CameraResult<()> {
        let mut failures = Vec::new();
        for control in self.get_supported_controls() {
            if let Err(error) = self.reset_control(control) {
                failures.push(std::sync::Arc::new(
                    error.with_operation(format!("{control:?}")),
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(crate::CameraError::control_batch(failures))
        }
    }
}

#[cfg(test)]
mod camera_control_tests {
    use super::*;

    #[test]
    fn test_stream_stats_default() {
        let stats = StreamStats::default();
        assert_eq!(stats.total_frames, 0);
        assert_eq!(stats.dropped_frames, 0);
        assert_eq!(stats.current_fps, 0.0);
        assert_eq!(stats.buffer_size, 0);
        assert_eq!(stats.buffered_frames, 0);
        assert_eq!(stats.uptime_seconds, 0);
    }

    #[test]
    fn test_stream_stats_drop_rate() {
        let mut stats = StreamStats::default();

        // No captured frames means a zero drop ratio.
        assert_eq!(stats.drop_rate(), 0.0);

        // Captured frames with no drops.
        stats.total_frames = 100;
        stats.dropped_frames = 0;
        assert_eq!(stats.drop_rate(), 0.0);

        // Some captured frames were dropped.
        stats.dropped_frames = 10;
        assert_eq!(stats.drop_rate(), 0.1);

        // Every captured frame was dropped.
        stats.dropped_frames = 100;
        assert_eq!(stats.drop_rate(), 1.0);
    }

    #[test]
    fn test_stream_stats_buffer_usage() {
        let mut stats = StreamStats::default();

        // No buffer means zero utilization.
        assert_eq!(stats.buffer_usage(), 0.0);

        // Empty buffer.
        stats.buffer_size = 10;
        stats.buffered_frames = 0;
        assert_eq!(stats.buffer_usage(), 0.0);

        // Half-full buffer.
        stats.buffered_frames = 5;
        assert_eq!(stats.buffer_usage(), 0.5);

        // Full buffer.
        stats.buffered_frames = 10;
        assert_eq!(stats.buffer_usage(), 1.0);
    }

    #[test]
    fn test_stream_stats_calculations() {
        let stats = StreamStats {
            total_frames: 1000,
            dropped_frames: 50,
            current_fps: 29.5,
            buffer_size: 20,
            buffered_frames: 15,
            uptime_seconds: 60,
        };

        assert_eq!(stats.drop_rate(), 0.05);
        assert_eq!(stats.buffer_usage(), 0.75);
    }
}
