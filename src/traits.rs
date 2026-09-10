//! 简化的摄像头接口定义
//!
//! 专注于两个核心功能：
//! 1. 获取相机配置支持
//! 2. 异步视频流处理

use crate::pixels::Pixels as Array3;
use crate::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraResult,
};
use std::time::Duration;

/// 简化的摄像头管理接口
pub trait CameraManager {
    /// 列出所有可用的摄像头设备
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>>;

    /// 获取设备支持的配置选项
    fn get_supported_configs(device_index: u32) -> CameraResult<Vec<CameraConfig>>;

    /// 检查特定配置是否被设备支持
    fn is_config_supported(device_index: u32, config: &CameraConfig) -> CameraResult<bool>;
}

/// 基于 ringbuf 的流式摄像头接口
#[allow(async_fn_in_trait)]
pub trait StreamingCamera: Send + Sync {
    /// Shared snapshots supplied by native callback/worker backends.
    fn frame_hub(&self) -> Option<&crate::FrameHub> {
        None
    }

    fn get_shared_frame(&self) -> CameraResult<Option<std::sync::Arc<crate::CapturedFrame>>> {
        self.frame_hub().map(|hub| hub.latest()).ok_or_else(|| {
            crate::CameraError::StreamError("Shared frames unavailable for this backend".into())
        })
    }

    async fn wait_for_shared_frame(
        &self,
        after: Option<crate::FrameKey>,
        timeout: Duration,
    ) -> CameraResult<std::sync::Arc<crate::CapturedFrame>> {
        self.frame_hub()
            .ok_or_else(|| {
                crate::CameraError::StreamError("Shared frames unavailable for this backend".into())
            })?
            .wait_after(after, timeout)
            .await
    }
    /// 启动异步视频流
    ///
    /// 启动后，视频帧会在后台持续捕获并存储在 ring buffer 中
    async fn start_stream(&self, config: CameraConfig) -> CameraResult<()>;

    /// 停止视频流
    async fn stop_stream(&self) -> CameraResult<()>;

    /// 立即获取最新的一帧（非阻塞）
    ///
    /// 返回 None 如果缓冲区为空
    /// 返回 Array3<u8> 形状为 (height, width, 3) 的 RGB 数据
    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>>;

    /// 等待并获取下一帧（带超时）
    /// 返回 Array3<u8> 形状为 (height, width, 3) 的 RGB 数据
    async fn wait_for_frame(&self, timeout: Duration) -> CameraResult<Array3<u8>>;

    /// 检查流是否正在运行
    fn is_streaming(&self) -> bool;

    /// 获取当前配置
    fn get_config(&self) -> Option<CameraConfig>;

    /// 获取流状态统计
    fn get_stats(&self) -> StreamStats;

    /// 设置缓冲区大小（重新配置 ring buffer）
    async fn set_buffer_size(&self, size: usize) -> CameraResult<()>;
}

/// 流统计信息
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// 总捕获帧数
    pub total_frames: u64,
    /// 丢弃的帧数（缓冲区满时）
    pub dropped_frames: u64,
    /// 当前 FPS
    pub current_fps: f64,
    /// 缓冲区当前大小
    pub buffer_size: usize,
    /// 缓冲区中当前帧数
    pub buffered_frames: usize,
    /// 运行时长（秒）
    pub uptime_seconds: u64,
}

impl StreamStats {
    /// 计算丢帧率
    pub fn drop_rate(&self) -> f64 {
        if self.total_frames == 0 {
            0.0
        } else {
            self.dropped_frames as f64 / self.total_frames as f64
        }
    }

    /// 计算缓冲区使用率
    pub fn buffer_usage(&self) -> f64 {
        if self.buffer_size == 0 {
            0.0
        } else {
            self.buffered_frames as f64 / self.buffer_size as f64
        }
    }
}

// ============================================================================
// 摄像头控制接口
// ============================================================================

/// 摄像头控制参数接口
#[allow(async_fn_in_trait)]
pub trait CameraControl {
    /// 获取指定控制参数的当前值
    fn get_control(&self, control: CameraControlType) -> CameraResult<CameraControlValue>;

    /// 设置指定控制参数的值
    fn set_control(
        &self,
        control: CameraControlType,
        value: CameraControlValue,
    ) -> CameraResult<()>;

    /// 获取指定控制参数的取值范围
    fn get_control_range(&self, control: CameraControlType) -> CameraResult<CameraControlRange>;

    /// 检查设备是否支持指定的控制参数
    fn supports_control(&self, control: CameraControlType) -> bool;

    /// 获取设备支持的所有控制参数
    fn get_supported_controls(&self) -> Vec<CameraControlType>;

    /// 将控制参数重置为默认值
    fn reset_control(&self, control: CameraControlType) -> CameraResult<()> {
        let range = self.get_control_range(control)?;
        self.set_control(control, CameraControlValue::manual(range.default))
    }

    /// 将所有控制参数重置为默认值
    fn reset_all_controls(&self) -> CameraResult<()> {
        let mut failures = Vec::new();
        for control in self.get_supported_controls() {
            if let Err(error) = self.reset_control(control) {
                failures.push(format!("{control:?}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(crate::CameraError::ControlBatch(failures))
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

        // 没有帧时应该返回0
        assert_eq!(stats.drop_rate(), 0.0);

        // 有帧但没有丢帧
        stats.total_frames = 100;
        stats.dropped_frames = 0;
        assert_eq!(stats.drop_rate(), 0.0);

        // 有丢帧
        stats.dropped_frames = 10;
        assert_eq!(stats.drop_rate(), 0.1);

        // 全部丢帧
        stats.dropped_frames = 100;
        assert_eq!(stats.drop_rate(), 1.0);
    }

    #[test]
    fn test_stream_stats_buffer_usage() {
        let mut stats = StreamStats::default();

        // 没有缓冲区时应该返回0
        assert_eq!(stats.buffer_usage(), 0.0);

        // 空缓冲区
        stats.buffer_size = 10;
        stats.buffered_frames = 0;
        assert_eq!(stats.buffer_usage(), 0.0);

        // 半满缓冲区
        stats.buffered_frames = 5;
        assert_eq!(stats.buffer_usage(), 0.5);

        // 满缓冲区
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
