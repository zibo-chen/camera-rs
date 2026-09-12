//! AVFoundation 捕获会话管理

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::exception;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureSessionPresetInputPriority, AVCaptureVideoDataOutput,
};
#[cfg(target_os = "macos")]
use objc2_av_foundation::{
    AVCaptureSessionPreset1280x720, AVCaptureSessionPreset1920x1080,
    AVCaptureSessionPreset3840x2160, AVCaptureSessionPreset640x480,
};
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

use crate::error::CameraError;
use crate::types::{CameraConfig, CameraResult};

use super::delegate::{CaptureDelegate, FrameBuffer};
use super::device::{find_best_format, fps_matches_range, get_device_by_id};

fn cv_pixel_buffer_pixel_format_type_key() -> &'static NSString {
    unsafe { AsRef::<NSString>::as_ref(kCVPixelBufferPixelFormatTypeKey) }
}

fn nv12_video_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    let format_key = cv_pixel_buffer_pixel_format_type_key();
    let format_value = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
    let keys: [&NSString; 1] = [format_key];
    let values: [&AnyObject; 1] = [(&*format_value) as &AnyObject];

    NSDictionary::from_slices(&keys, &values)
}

fn objc_exception_message(exception: Option<Retained<exception::Exception>>) -> String {
    exception
        .as_deref()
        .map(|exception| format!("{exception:?}"))
        .unwrap_or_else(|| "nil Objective-C exception".to_string())
}

fn objc_exception_error(
    context: &str,
    exception: Option<Retained<exception::Exception>>,
) -> CameraError {
    let message = objc_exception_message(exception);
    log::error!("{context} raised Objective-C exception: {message}");
    CameraError::Other(format!("{context}: {message}"))
}

fn catch_objc<T, F>(context: &str, f: F) -> CameraResult<T>
where
    F: FnOnce() -> T,
{
    exception::catch(AssertUnwindSafe(f))
        .map_err(|exception| objc_exception_error(context, exception))
}

fn catch_objc_result<T, F>(context: &str, f: F) -> CameraResult<T>
where
    F: FnOnce() -> CameraResult<T>,
{
    catch_objc(context, f)?
}

/// Retains the configuration lock through startRunning; macOS otherwise lets
/// the session preset replace activeFormat at startup. RAII also covers errors.
struct DeviceConfigurationLock(Retained<AVCaptureDevice>);
impl Drop for DeviceConfigurationLock {
    fn drop(&mut self) {
        unsafe {
            self.0.unlockForConfiguration();
        }
    }
}

/// 捕获会话
pub struct CaptureSession {
    /// AVCaptureSession
    session: Retained<AVCaptureSession>,
    /// AVCaptureDevice
    #[allow(dead_code)]
    device: Retained<AVCaptureDevice>,
    /// AVCaptureDeviceInput
    #[allow(dead_code)]
    input: Retained<AVCaptureDeviceInput>,
    /// AVCaptureVideoDataOutput
    #[allow(dead_code)]
    output: Retained<AVCaptureVideoDataOutput>,
    /// 帧委托 - 需要保持存活
    #[allow(dead_code)]
    delegate: Retained<CaptureDelegate>,
    /// 帧缓冲区
    frame_buffer: Arc<FrameBuffer>,
    /// 回调队列 - 需要保持存活
    #[allow(dead_code)]
    callback_queue: DispatchRetained<DispatchQueue>,
    /// 当前配置
    config: CameraConfig,
    configuration_lock: Option<DeviceConfigurationLock>,
}

impl CaptureSession {
    /// 创建新的捕获会话
    pub fn new(
        device_id: &str,
        config: CameraConfig,
        frame_buffer: Arc<FrameBuffer>,
        discard_late_frames: bool,
    ) -> CameraResult<Self> {
        catch_objc_result("AVFoundation create capture session", || {
            Self::new_uncaught(device_id, config, frame_buffer, discard_late_frames)
        })
    }

    fn new_uncaught(
        device_id: &str,
        config: CameraConfig,
        frame_buffer: Arc<FrameBuffer>,
        discard_late_frames: bool,
    ) -> CameraResult<Self> {
        log::info!(
            "Creating AVFoundation capture session for device {} with config {:?}",
            device_id,
            config
        );

        // 获取设备
        let device = get_device_by_id(device_id)?;

        unsafe {
            log::debug!("Got device: {}", device.localizedName());
        }

        // 查找最佳格式
        log::debug!("Finding best format...");
        let best_format = find_best_format(&device, &config)?;
        log::debug!("Best format found");

        // 创建会话
        log::debug!("Creating AVCaptureSession...");
        let session = unsafe { AVCaptureSession::new() };
        log::debug!("AVCaptureSession created");

        // 开始配置
        log::debug!("Beginning configuration...");
        unsafe {
            session.beginConfiguration();
        }
        log::debug!("Configuration begun");

        // 创建输入
        let input = unsafe {
            AVCaptureDeviceInput::deviceInputWithDevice_error(&device).map_err(|e| {
                CameraError::DeviceOpenFailed(format!("Failed to create input: {:?}", e))
            })?
        };

        // 添加输入到会话
        let can_add = unsafe { session.canAddInput(&input) };
        if can_add {
            unsafe { session.addInput(&input) };
            log::debug!("Input added to session");
        } else {
            return Err(CameraError::DeviceOpenFailed(
                "Cannot add input to session".into(),
            ));
        }

        // 创建输出
        let output = unsafe { AVCaptureVideoDataOutput::new() };

        // Keep native output untouched. RGB output asks Core Video for NV12 so
        // camera-rs can use its direct two-row SIMD conversion instead of paying
        // for AVFoundation YUV->BGRA followed by another BGRA->RGB pass.
        // 分辨率由 activeFormat 控制，避免向 setVideoSettings 传入容易触发
        // NSInvalidArgumentException 的 width/height 约束。
        unsafe {
            // An empty dictionary requests device-native samples (Apple videoSettings contract).
            let dict = if frame_buffer.wants_native() {
                NSDictionary::new()
            } else {
                nv12_video_settings()
            };
            output.setVideoSettings(Some(&dict));
        }

        // 设置丢帧策略
        unsafe {
            output.setAlwaysDiscardsLateVideoFrames(discard_late_frames);
        }

        // 添加输出到会话
        let output_ref: &AVCaptureOutput = &output;
        let can_add_output = unsafe { session.canAddOutput(output_ref) };
        if can_add_output {
            unsafe { session.addOutput(output_ref) };
            log::debug!("Output added to session");
        } else {
            return Err(CameraError::DeviceOpenFailed(
                "Cannot add output to session".into(),
            ));
        }

        // Preserve the selected native format when committing/starting the
        // session instead of allowing the default high-quality preset to win.
        unsafe {
            #[cfg(target_os = "macos")]
            {
                let size = objc2_core_media::CMVideoFormatDescriptionGetDimensions(
                    &best_format.formatDescription(),
                );
                let preset = match (size.width, size.height) {
                    (640, 480) => Some(AVCaptureSessionPreset640x480),
                    (1280, 720) => Some(AVCaptureSessionPreset1280x720),
                    (1920, 1080) => Some(AVCaptureSessionPreset1920x1080),
                    (3840, 2160) => Some(AVCaptureSessionPreset3840x2160),
                    _ => None,
                };
                if let Some(preset) = preset.filter(|p| session.canSetSessionPreset(p)) {
                    session.setSessionPreset(preset);
                }
            }
            if session.canSetSessionPreset(AVCaptureSessionPresetInputPriority) {
                session.setSessionPreset(AVCaptureSessionPresetInputPriority);
            }
        }

        // 在输入输出都添加后，设置设备格式
        // 这是正确的顺序，因为某些格式可能在添加输出后才能正确应用
        log::debug!("Locking device for format configuration...");
        unsafe {
            device.lockForConfiguration().map_err(|e| {
                CameraError::DeviceOpenFailed(format!("Failed to lock device: {:?}", e))
            })?;
        }
        let configuration_lock = DeviceConfigurationLock(device.clone());
        log::debug!("Device locked");

        // 设置格式
        log::debug!("Setting active format...");
        unsafe {
            device.setActiveFormat(&best_format);
        }
        log::debug!("Active format set");

        // 设置帧率。不要手写 1/fps 的 CMTime，部分设备只接受
        // AVFrameRateRange 暴露的精确 duration。
        let selected_frame_duration = unsafe {
            let frame_rate_ranges = best_format.videoSupportedFrameRateRanges();
            let range_count = frame_rate_ranges.len();
            let mut selected = None;
            let mut best_distance = f64::MAX;
            for j in 0..range_count {
                let range = frame_rate_ranges.objectAtIndex_unchecked(j);
                let min_fps = range.minFrameRate();
                let max_fps = range.maxFrameRate();
                log::debug!("Format supports fps range: {:.6}-{:.6}", min_fps, max_fps);
                if fps_matches_range(config.frame_rate()?.as_f64(), min_fps, max_fps) {
                    let requested = config.frame_rate()?.as_f64();
                    let target = requested.clamp(min_fps, max_fps);
                    let distance = (target - requested).abs();
                    if distance < best_distance {
                        let duration = if (max_fps - min_fps).abs() < 0.01
                            || (max_fps - target).abs() < 0.01
                        {
                            range.minFrameDuration()
                        } else if (target - min_fps).abs() < 0.01 {
                            range.maxFrameDuration()
                        } else {
                            CMTime {
                                value: config.fps_denominator as i64,
                                timescale: i32::try_from(config.fps).map_err(|_| {
                                    CameraError::InvalidConfig(
                                        "Frame rate exceeds CMTime range".into(),
                                    )
                                })?,
                                flags: CMTimeFlags::Valid,
                                epoch: 0,
                            }
                        };
                        selected = Some((duration, min_fps, max_fps));
                        best_distance = distance;
                    }
                }
            }
            selected
        };

        if let Some((frame_duration, min_fps, max_fps)) = selected_frame_duration {
            log::debug!("Setting frame rate to {} fps...", config.fps);
            let frame_duration_value = frame_duration.value;
            let frame_duration_timescale = frame_duration.timescale;
            unsafe {
                device.setActiveVideoMinFrameDuration(frame_duration);
                device.setActiveVideoMaxFrameDuration(frame_duration);
            }
            log::debug!(
                "Frame rate configured to {} fps using supported range {:.6}-{:.6}, duration {}/{}",
                config.fps,
                min_fps,
                max_fps,
                frame_duration_value,
                frame_duration_timescale
            );
        } else {
            log::warn!(
                "Requested fps {} not supported by format, using device default",
                config.fps
            );
        }

        // Hold the lock until startRunning has applied the chosen format.

        let mut config = config;
        unsafe {
            let desc = best_format.formatDescription();
            let size = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&desc);
            config.width = size.width as u32;
            config.height = size.height as u32;
            config.format =
                super::device::fourcc_to_format(desc.media_sub_type()).ok_or_else(|| {
                    CameraError::UnsupportedFormat("Selected AVFoundation subtype".into())
                })?;
            let duration = device.activeVideoMinFrameDuration();
            if duration.value > 0 {
                config = config.with_frame_rate(crate::FrameRate::new(
                    duration.timescale as u32,
                    u32::try_from(duration.value).map_err(|_| {
                        CameraError::InvalidFormat("CMTime duration overflow".into())
                    })?,
                )?);
            }
        }

        // 创建帧缓冲区
        let delegate = CaptureDelegate::new(frame_buffer.clone());

        // 创建回调队列
        let callback_queue =
            DispatchQueue::new("com.medivh.camera.callback", DispatchQueueAttr::SERIAL);

        // 设置委托
        unsafe {
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&*callback_queue),
            );
        }

        // 提交配置
        unsafe {
            session.commitConfiguration();
            log::debug!(
                "Native configured preset={} format={:?} output={:?}",
                session.sessionPreset(),
                device.activeFormat(),
                output.videoSettings()
            );
        }

        log::info!("Capture session created successfully");

        Ok(Self {
            session,
            device,
            input,
            output,
            delegate,
            frame_buffer,
            callback_queue,
            config,
            configuration_lock: Some(configuration_lock),
        })
    }

    /// 启动捕获
    pub fn start(&mut self) -> CameraResult<()> {
        catch_objc_result("AVFoundation start capture session", || {
            self.start_uncaught()
        })
    }

    fn start_uncaught(&mut self) -> CameraResult<()> {
        unsafe {
            if self.session.isRunning() {
                log::warn!("Session is already running");
                return Ok(());
            }
        }

        unsafe {
            self.session.startRunning();
            // macOS may choose a different active format when starting a preset.
            // Report what is actually running, including a preset fallback.
            let desc = self.device.activeFormat().formatDescription();
            let size = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&desc);
            self.config.width = size.width as u32;
            self.config.height = size.height as u32;
            self.config.format = super::device::fourcc_to_format(desc.media_sub_type())
                .ok_or_else(|| {
                    CameraError::UnsupportedFormat("Native AVFoundation subtype".into())
                })?;
            let duration = self.device.activeVideoMinFrameDuration();
            if duration.value > 0 {
                self.config = self.config.clone().with_frame_rate(crate::FrameRate::new(
                    duration.timescale as u32,
                    u32::try_from(duration.value).map_err(|_| {
                        CameraError::InvalidFormat("CMTime duration overflow".into())
                    })?,
                )?);
            }
            log::debug!(
                "Native running preset={} config={:?}",
                self.session.sessionPreset(),
                self.config
            );

            if self.session.isRunning() {
                self.configuration_lock.take();
                log::info!("Capture session started");
                Ok(())
            } else {
                self.frame_buffer.stop();
                Err(CameraError::StreamError("Failed to start session".into()))
            }
        }
    }

    /// 停止捕获
    pub fn stop(&self) -> CameraResult<()> {
        catch_objc_result("AVFoundation stop capture session", || self.stop_uncaught())
    }

    fn stop_uncaught(&self) -> CameraResult<()> {
        self.frame_buffer.stop();

        unsafe {
            if self.session.isRunning() {
                self.session.stopRunning();
                log::info!("Capture session stopped");
            }
        }

        Ok(())
    }

    /// 检查是否正在运行
    pub fn is_running(&self) -> bool {
        catch_objc("AVFoundation check capture session running", || unsafe {
            self.session.isRunning()
        })
        .unwrap_or(false)
    }

    /// 获取帧缓冲区
    pub fn frame_buffer(&self) -> &Arc<FrameBuffer> {
        &self.frame_buffer
    }

    /// 获取当前配置
    pub fn config(&self) -> &CameraConfig {
        &self.config
    }

    /// 获取设备
    pub fn device(&self) -> &AVCaptureDevice {
        &self.device
    }

    fn drop_uncaught(&mut self) {
        // 停止会话
        self.frame_buffer.stop();

        unsafe {
            if self.session.isRunning() {
                self.session.stopRunning();
            }

            // Stop scheduling delegate work and drain the serial queue before
            // releasing the delegate's ivars. Lifecycle methods run off this queue.
            self.output.setSampleBufferDelegate_queue(None, None);
            self.callback_queue.exec_sync(|| {});

            // 移除输入和输出
            self.session.removeInput(&self.input);
            let output_ref: &AVCaptureOutput = &self.output;
            self.session.removeOutput(output_ref);
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        log::debug!("Dropping CaptureSession");

        if let Err(error) = catch_objc("AVFoundation drop capture session", || {
            self.drop_uncaught();
        }) {
            log::warn!("Ignoring error while dropping AVFoundation capture session: {error}");
        }

        log::debug!("CaptureSession dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_video_settings_uses_core_video_pixel_format_key_only() {
        let settings = nv12_video_settings();
        let format_key = cv_pixel_buffer_pixel_format_type_key();
        let width_key = NSString::from_str("Width");
        let height_key = NSString::from_str("Height");

        assert_eq!(settings.count(), 1);
        assert!(settings.objectForKey(format_key).is_some());
        assert!(settings.objectForKey(width_key.as_ref()).is_none());
        assert!(settings.objectForKey(height_key.as_ref()).is_none());
    }

    #[test]
    fn catch_objc_maps_ns_exception_to_camera_error() {
        use objc2_foundation::NSException;

        let result: CameraResult<()> = catch_objc("test Objective-C boundary", || {
            let name = NSString::from_str("CameraTestException");
            let reason = NSString::from_str("boom");
            let exception = NSException::new(name.as_ref(), Some(reason.as_ref()), None)
                .expect("NSException should be created");
            exception.raise();
        });

        let error = result.expect_err("Objective-C exception should be mapped to CameraError");
        let message = error.to_string();
        assert!(message.contains("test Objective-C boundary"));
        assert!(message.contains("CameraTestException") || message.contains("boom"));
    }

    #[test]
    fn fps_matches_near_integer_avfoundation_ranges() {
        assert!(fps_matches_range(30.0, 30.00003, 30.00003));
        assert!(fps_matches_range(60.0, 59.94, 60.0));
        assert!(!fps_matches_range(30.0, 50.0, 60.0));
    }
}
