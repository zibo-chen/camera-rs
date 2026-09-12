use super::{
    convert::{from_fourcc, to_fourcc, Converter, Plane},
    native,
};
use crate::pixels::Pixels as Array3;
use crate::{
    CameraConfig, CameraControl, CameraControlRange, CameraControlType, CameraControlValue,
    CameraDeviceInfo, CameraError, CameraManager, CameraResult, FrameHub, StreamStats,
    StreamingCamera, VideoFormat,
};
use parking_lot::Mutex;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, net::UnixStream},
    },
    path::Path,
    sync::{mpsc, Arc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// A Linux kernel camera. `new(N)` opens `/dev/videoN`, even when indices have
/// gaps. `from_path` also accepts stable `/dev/v4l/by-id/...` symlinks.
/// Blocking driver calls run on a dedicated worker. Dropping the camera stops
/// capture and joins that worker before closing the device.
pub struct V4l2Camera {
    inner: Arc<Inner>,
}
struct Inner {
    file: Arc<File>,
    hub: Arc<FrameHub>,
    state: Mutex<State>,
}
struct State {
    worker: Option<JoinHandle<CameraResult<()>>>,
    wake: Option<UnixStream>,
    config: Option<CameraConfig>,
    buffers: u32,
}

fn open(path: &Path) -> CameraResult<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                CameraError::device_not_found(path.display().to_string())
            }
            std::io::ErrorKind::PermissionDenied => {
                CameraError::permission_denied(format!("{}: {e}", path.display()))
            }
            _ => CameraError::device_open_failed(format!("{}: {e}", path.display())),
        })?;
    native::probe(file.as_raw_fd()).map_err(|e| {
        CameraError::device_open_failed(format!(
            "{} is not a supported V4L2 capture device: {e}",
            path.display()
        ))
    })?;
    Ok(file)
}
fn device_path(index: u32) -> String {
    format!("/dev/video{index}")
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.split(|&b| b == 0).next().unwrap_or_default()).into_owned()
}

impl V4l2Camera {
    pub fn new(device_index: u32) -> CameraResult<Self> {
        Self::from_path(device_path(device_index))
    }
    pub(crate) fn new_with_hub(index: u32, hub: FrameHub) -> CameraResult<Self> {
        Self::from_path_with_hub(device_path(index), hub)
    }
    pub fn from_path(path: impl AsRef<Path>) -> CameraResult<Self> {
        Self::from_path_with_hub(path, FrameHub::default())
    }
    pub(crate) fn from_path_with_hub(path: impl AsRef<Path>, hub: FrameHub) -> CameraResult<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                file: Arc::new(open(path.as_ref())?),
                hub: Arc::new(hub),
                state: Mutex::new(State {
                    worker: None,
                    wake: None,
                    config: None,
                    buffers: 4,
                }),
            }),
        })
    }
    pub async fn start_stream_arc(self: &Arc<Self>, config: CameraConfig) -> CameraResult<()> {
        self.start_stream(config).await
    }
    pub async fn stop_stream_arc(self: &Arc<Self>) -> CameraResult<()> {
        self.stop_stream().await
    }
    pub(crate) fn control_writable(&self, control: CameraControlType) -> CameraResult<bool> {
        let _guard = self.inner.state.lock();
        Ok(self.query(control)?.read_only == 0)
    }
    pub fn cleanup(&self) {
        if let Err(error) = self.inner.stop() {
            log::debug!("V4L2 cleanup: {error}");
        }
    }

    pub(crate) fn device_capabilities(
        device_index: u32,
    ) -> CameraResult<crate::DeviceCapabilities> {
        let configurations = <Self as CameraManager>::get_supported_configs(device_index)?;
        let file = open(Path::new(&device_path(device_index)))?;
        let fd = file.as_raw_fd();
        let mut ranges = Vec::new();
        for format_index in 0..1024 {
            let Some(code) = native::formats(fd, format_index)? else {
                break;
            };
            let Some(format) = from_fourcc(code) else {
                continue;
            };
            for size_index in 0..4096 {
                let Some(size) = native::sizes(fd, code, size_index)? else {
                    break;
                };
                let mut frame_intervals = Vec::new();
                for (width, height) in dimensions(&size) {
                    for interval_index in 0..4096 {
                        let Some(interval) =
                            native::intervals(fd, code, width, height, interval_index)?
                        else {
                            break;
                        };
                        if interval.kind != 1
                            && interval.min_n > 0
                            && interval.min_d > 0
                            && interval.max_n > 0
                            && interval.max_d > 0
                        {
                            frame_intervals.push(crate::FrameIntervalRange {
                                kind: range_kind(interval.kind),
                                minimum: crate::FrameInterval {
                                    numerator: interval.min_n,
                                    denominator: interval.min_d,
                                },
                                maximum: crate::FrameInterval {
                                    numerator: interval.max_n,
                                    denominator: interval.max_d,
                                },
                                step: (interval.kind == 3
                                    && interval.step_n > 0
                                    && interval.step_d > 0)
                                    .then_some(crate::FrameInterval {
                                        numerator: interval.step_n,
                                        denominator: interval.step_d,
                                    }),
                                at_resolution: (width, height),
                            });
                        }
                        if interval.kind != 1 {
                            break;
                        }
                    }
                }
                if size.kind != 1 || !frame_intervals.is_empty() {
                    ranges.push(crate::CaptureModeRange {
                        format: format.into(),
                        kind: range_kind(size.kind),
                        width: crate::DimensionRange {
                            minimum: size.min_w,
                            maximum: size.max_w,
                            step: size.step_w.max(1),
                        },
                        height: crate::DimensionRange {
                            minimum: size.min_h,
                            maximum: size.max_h,
                            step: size.step_h.max(1),
                        },
                        frame_intervals,
                    });
                }
                if size.kind != 1 {
                    break;
                }
            }
        }
        Ok(crate::DeviceCapabilities::from_configurations(configurations).with_ranges(ranges))
    }

    pub(crate) fn requested_configs(
        device_index: u32,
        request: &crate::CaptureRequest,
        advertised: &[CameraConfig],
    ) -> CameraResult<Vec<CameraConfig>> {
        let mut formats = if request.preferred_formats.is_empty() {
            advertised
                .iter()
                .map(|config| config.format)
                .collect::<Vec<_>>()
        } else {
            request
                .preferred_formats
                .iter()
                .copied()
                .map(Into::into)
                .collect::<Vec<_>>()
        };
        formats.sort_by_key(|format| *format as u8);
        formats.dedup();
        let mut requested = Vec::new();
        for format in formats {
            let config = CameraConfig::new(
                format,
                request.width,
                request.height,
                request.preferred_fps.numerator(),
            )
            .with_frame_rate(request.preferred_fps);
            if <Self as CameraManager>::is_config_supported(device_index, &config)? {
                requested.push(config);
            }
        }
        Ok(requested)
    }
}

fn range_kind(kind: u32) -> crate::CapabilityRangeKind {
    match kind {
        1 => crate::CapabilityRangeKind::Discrete,
        2 => crate::CapabilityRangeKind::Continuous,
        _ => crate::CapabilityRangeKind::Stepwise,
    }
}
impl Drop for V4l2Camera {
    fn drop(&mut self) {
        self.cleanup();
    }
}
impl Inner {
    fn stop_locked(&self, state: &mut State) -> CameraResult<()> {
        self.hub.stop();
        if let Some(mut wake) = state.wake.take() {
            let _ = wake.write_all(&[1]);
        }
        state.config = None;
        if let Some(worker) = state.worker.take() {
            worker
                .join()
                .map_err(|_| CameraError::stream_error("V4L2 capture worker panicked".into()))??;
        }
        Ok(())
    }
    fn stop(&self) -> CameraResult<()> {
        self.stop_locked(&mut self.state.lock())
    }
    fn start(&self, config: CameraConfig) -> CameraResult<()> {
        config.validate()?;
        to_fourcc(config.format)?;
        let mut state = self.state.lock();
        if self.hub.is_streaming() {
            return if state.config.as_ref() == Some(&config) {
                Ok(())
            } else {
                Err(CameraError::stream_error(
                    "Stop the V4L2 stream before changing configuration".into(),
                ))
            };
        }
        if let Err(error) = self.stop_locked(&mut state) {
            log::debug!("Previous V4L2 stream: {error}");
        }
        let (wake, cancel) = UnixStream::pair()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let file = self.file.clone();
        let hub = self.hub.clone();
        let buffers = state.buffers;
        let session = hub.start();
        let worker = std::thread::Builder::new()
            .name("camera-v4l2".into())
            .spawn(move || {
                // Also clears state/wakes readers on errors or a Rust panic.
                struct StopHub(Arc<FrameHub>);
                impl Drop for StopHub {
                    fn drop(&mut self) {
                        self.0.stop();
                    }
                }
                let _stop = StopHub(hub.clone());
                let setup = (|| {
                    let mut format = requested_format(file.as_raw_fd(), &config)?;
                    native::configure(file.as_raw_fd(), &mut format, true)?;
                    let actual = config_from_format(&format)?;
                    let mut decoder = Converter::new()?;
                    if matches!(
                        actual.format,
                        VideoFormat::YUYV | VideoFormat::UYVY | VideoFormat::NV12
                    ) {
                        decoder.set_colorimetry(format.ycbcr, format.full_range != 0)?;
                    }
                    let stream = native::Stream::start(&file, buffers)?;
                    Ok::<_, CameraError>((format, actual, decoder, stream))
                })();
                let (format, actual, mut decoder, mut stream) = match setup {
                    Ok(setup) => setup,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return Ok(());
                    }
                };
                if sender.send(Ok(actual.clone())).is_err() {
                    return Ok(());
                }
                let mut last_frame = Instant::now();
                let native_output = hub.wants_native();
                let mut compressed_scratch = Vec::new();
                loop {
                    let mut deferred_mjpeg = None;
                    let result = stream.next(
                        cancel.as_raw_fd(),
                        format.strides,
                        |planes, timestamp, clock, source_sequence, damaged| {
                            if damaged {
                                let error = CameraError::invalid_frame(
                                    "V4L2 driver marked frame damaged".into(),
                                );
                                hub.record_input_error(session);
                                hub.log_frame_error("V4L2", &error);
                                return Ok(());
                            }
                            let timestamp = Some(crate::SourceTimestamp {
                                nanoseconds: timestamp,
                                clock,
                            });
                            if !native_output && actual.format == VideoFormat::MJPEG {
                                let Some(plane) = planes.first() else {
                                    return Err(CameraError::buffer_exhausted());
                                };
                                compressed_scratch.clear();
                                compressed_scratch.extend_from_slice(plane.data);
                                deferred_mjpeg = Some((timestamp, source_sequence, plane.stride));
                                return Ok(());
                            }
                            let converted = if native_output {
                                native_layout(&actual, &format, planes).and_then(|layout| {
                                    let parts = [
                                        planes[0].data,
                                        planes.get(1).map_or(&[][..], |plane| plane.data),
                                    ];
                                    hub.publish_native(
                                        session,
                                        layout,
                                        timestamp,
                                        Some(source_sequence),
                                        &parts[..planes.len()],
                                    )
                                })
                            } else {
                                hub.publish_rgb_metadata(
                                    session,
                                    actual.width,
                                    actual.height,
                                    timestamp,
                                    Some(source_sequence),
                                    |rgb| decoder.convert(&actual, planes, rgb),
                                )
                            };
                            if let Err(error) = converted {
                                hub.log_frame_error("V4L2", &error);
                            }
                            Ok(())
                        },
                    );
                    if result.is_ok() {
                        if let Some((timestamp, source_sequence, stride)) = deferred_mjpeg.take() {
                            let plane = [Plane {
                                data: &compressed_scratch,
                                stride,
                            }];
                            if let Err(error) = hub.publish_rgb_metadata(
                                session,
                                actual.width,
                                actual.height,
                                timestamp,
                                Some(source_sequence),
                                |rgb| decoder.convert(&actual, &plane, rgb),
                            ) {
                                hub.log_frame_error("V4L2 MJPEG", &error);
                            }
                        }
                    }
                    match result {
                        Ok(()) => last_frame = Instant::now(),
                        Err(error)
                            if error.io_error().and_then(std::io::Error::raw_os_error)
                                == Some(libc::ECANCELED) =>
                        {
                            break
                        }
                        Err(error)
                            if matches!(
                                error.io_error().and_then(std::io::Error::raw_os_error),
                                Some(libc::EAGAIN | libc::ETIMEDOUT)
                            ) =>
                        {
                            if last_frame.elapsed() >= Duration::from_secs(5) {
                                return Err(CameraError::stream_error(
                                    "V4L2 delivered no buffers for 5 seconds".into(),
                                ));
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
                stream.stop()?;
                Ok(())
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.hub.stop();
                return Err(error.into());
            }
        };
        state.worker = Some(worker);
        state.wake = Some(wake);
        match receiver.recv() {
            Ok(Ok(actual)) => {
                state.config = Some(actual);
                Ok(())
            }
            response => {
                let _ = self.stop_locked(&mut state);
                Err(match response {
                    Ok(Err(error)) => error,
                    _ => CameraError::stream_error("V4L2 worker exited during startup".into()),
                })
            }
        }
    }
}

fn requested_format(fd: i32, config: &CameraConfig) -> CameraResult<native::Format> {
    let preferred = to_fourcc(config.format)?;
    let mut selected = None;
    for i in 0..1024 {
        let Some(code) = native::formats(fd, i)? else {
            break;
        };
        if from_fourcc(code) == Some(config.format) {
            selected = Some(code);
            if code == preferred {
                break;
            }
        }
    }
    Ok(native::Format {
        fourcc: selected.ok_or_else(|| {
            CameraError::unsupported_format(format!(
                "V4L2 device does not advertise {:?}",
                config.format
            ))
        })?,
        width: config.width,
        height: config.height,
        fps: config.fps,
        fps_denominator: config.fps_denominator,
        ..Default::default()
    })
}
fn config_from_format(format: &native::Format) -> CameraResult<CameraConfig> {
    let pixel_format = from_fourcc(format.fourcc).ok_or_else(|| {
        CameraError::unsupported_format(format!("V4L2 negotiated fourcc 0x{:08x}", format.fourcc))
    })?;
    if format.planes != 1 && !(pixel_format == VideoFormat::NV12 && format.planes == 2) {
        return Err(CameraError::unsupported_format(
            "Unsupported V4L2 plane layout".into(),
        ));
    }
    if format.fps == 0 {
        return Err(CameraError::unsupported_format(
            "V4L2 driver does not report a frame interval".into(),
        ));
    }
    let config = CameraConfig::new(pixel_format, format.width, format.height, format.fps)
        .with_frame_rate(crate::FrameRate::new(format.fps, format.fps_denominator)?);
    config.validate()?;
    Ok(config)
}

fn native_layout(
    config: &CameraConfig,
    native: &native::Format,
    planes: &[super::convert::Plane<'_>],
) -> CameraResult<crate::FrameLayout> {
    use crate::{ColorInfo, ColorMatrix, ColorRange, FrameLayout, PixelFormat, PlaneLayout};
    let format = match config.format {
        VideoFormat::MJPEG => PixelFormat::Mjpeg,
        VideoFormat::YUYV => PixelFormat::Yuyv,
        VideoFormat::UYVY => PixelFormat::Uyvy,
        VideoFormat::NV12 => PixelFormat::Nv12,
        VideoFormat::RGB => PixelFormat::Rgb8,
        VideoFormat::Gray => PixelFormat::Gray8,
        _ => return Err(CameraError::unsupported_format("V4L2 native layout".into())),
    };
    let mut layout = FrameLayout::packed(
        config.width,
        config.height,
        format,
        planes[0].stride,
        planes[0].data.len(),
    );
    layout.color = ColorInfo {
        matrix: match native.ycbcr {
            1 | 3 | 5 => ColorMatrix::Bt601,
            2 | 4 => ColorMatrix::Bt709,
            6 => ColorMatrix::Bt2020,
            8 => ColorMatrix::Smpte240M,
            _ => ColorMatrix::Unknown,
        },
        range: if native.full_range != 0 {
            ColorRange::Full
        } else {
            ColorRange::Limited
        },
        ..Default::default()
    };
    if format == PixelFormat::Nv12 {
        let (offset, length, stride) = if planes.len() == 2 {
            (planes[0].data.len(), planes[1].data.len(), planes[1].stride)
        } else {
            let y_len = planes[0]
                .stride
                .checked_mul(config.height as usize)
                .filter(|&n| n < planes[0].data.len())
                .ok_or_else(|| CameraError::invalid_frame("Truncated V4L2 NV12".into()))?;
            layout.planes[0].length = y_len;
            (y_len, planes[0].data.len() - y_len, planes[0].stride)
        };
        layout.planes.push(PlaneLayout {
            offset,
            length,
            row_stride: stride,
            pixel_stride: 2,
        });
    }
    Ok(layout)
}

impl CameraManager for V4l2Camera {
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
        let mut devices = Vec::new();
        let mut denied = Vec::new();
        for entry in std::fs::read_dir("/dev")? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(index) = name
                .to_str()
                .and_then(|s| s.strip_prefix("video"))
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let file = match open(&entry.path()) {
                Ok(file) => file,
                Err(error) => {
                    if error.kind() == crate::CameraErrorKind::PermissionDenied {
                        denied.push(entry.path().display().to_string());
                    }
                    log::debug!("Skipping {}: {error}", entry.path().display());
                    continue;
                }
            };
            let info = native::probe(file.as_raw_fd())?;
            let mut device = CameraDeviceInfo::new(
                index,
                text(&info.card),
                format!("{} ({})", text(&info.driver), text(&info.bus)),
            );
            let stable = ["/dev/v4l/by-id", "/dev/v4l/by-path"]
                .into_iter()
                .find_map(|dir| {
                    let mut links: Vec<_> = std::fs::read_dir(dir)
                        .ok()?
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| std::fs::canonicalize(p).ok().as_ref() == Some(&entry.path()))
                        .collect();
                    links.sort();
                    links.into_iter().next()
                });
            device.device_path = Some(
                stable
                    .unwrap_or_else(|| entry.path())
                    .to_string_lossy()
                    .into_owned(),
            );
            devices.push(device);
        }
        devices.sort_by_key(|d| d.index);
        if devices.is_empty() && !denied.is_empty() {
            return Err(CameraError::permission_denied(format!(
                "V4L2 device access denied: {}. Android applications normally need Camera2 or an authorized USB descriptor.",
                denied.join(", ")
            )));
        }
        Ok(devices)
    }
    fn get_supported_configs(device_index: u32) -> CameraResult<Vec<CameraConfig>> {
        let file = open(Path::new(&device_path(device_index)))?;
        let fd = file.as_raw_fd();
        let mut configs = Vec::new();
        for i in 0..1024 {
            let Some(code) = native::formats(fd, i)? else {
                break;
            };
            let Some(format) = from_fourcc(code) else {
                continue;
            };
            for j in 0..4096 {
                let Some(size) = native::sizes(fd, code, j)? else {
                    break;
                };
                for (w, h) in dimensions(&size) {
                    for fps in frame_rates(fd, code, w, h)? {
                        let config =
                            CameraConfig::new(format, w, h, fps.numerator()).with_frame_rate(fps);
                        if config.validate().is_ok() && !configs.contains(&config) {
                            configs.push(config);
                        }
                    }
                }
                if size.kind != 1 {
                    break;
                }
            }
        }
        Ok(configs)
    }
    fn is_config_supported(device_index: u32, config: &CameraConfig) -> CameraResult<bool> {
        if config.validate().is_err() || to_fourcc(config.format).is_err() {
            return Ok(false);
        }
        let file = open(Path::new(&device_path(device_index)))?;
        let fd = file.as_raw_fd();
        let code = to_fourcc(config.format)?;
        if !size_supported(fd, code, config.width, config.height)? {
            return Ok(false);
        }
        let mut format = match requested_format(fd, config) {
            Ok(f) => f,
            Err(ref error) if error.kind() == crate::CameraErrorKind::UnsupportedFormat => {
                return Ok(false)
            }
            Err(e) => return Err(e),
        };
        match native::configure(fd, &mut format, false) {
            Ok(()) => (),
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) => {
                return Ok(false)
            }
            Err(e) => return Err(e.into()),
        }
        Ok(format.width == config.width
            && format.height == config.height
            && from_fourcc(format.fourcc) == Some(config.format)
            && frame_rate_supported(
                fd,
                format.fourcc,
                config.width,
                config.height,
                config.frame_rate()?,
            )?)
    }
}

/// Ranged drivers can advertise millions of sizes. Return endpoints and common
/// aligned resolutions; is_config_supported probes arbitrary sizes with TRY_FMT.
fn dimensions(size: &native::Size) -> Vec<(u32, u32)> {
    let mut result = Vec::new();
    for (w, h) in [
        (size.min_w, size.min_h),
        (size.max_w, size.max_h),
        (320, 240),
        (640, 480),
        (800, 600),
        (1024, 768),
        (1280, 720),
        (1280, 960),
        (1920, 1080),
        (2560, 1440),
        (3840, 2160),
    ] {
        if w >= size.min_w
            && w <= size.max_w
            && h >= size.min_h
            && h <= size.max_h
            && (w - size.min_w).is_multiple_of(size.step_w.max(1))
            && (h - size.min_h).is_multiple_of(size.step_h.max(1))
            && !result.contains(&(w, h))
        {
            result.push((w, h));
        }
    }
    result
}

fn size_supports(size: &native::Size, width: u32, height: u32) -> bool {
    width >= size.min_w
        && width <= size.max_w
        && height >= size.min_h
        && height <= size.max_h
        && (size.kind == 2
            || ((width - size.min_w).is_multiple_of(size.step_w.max(1))
                && (height - size.min_h).is_multiple_of(size.step_h.max(1))))
}

fn size_supported(fd: i32, code: u32, width: u32, height: u32) -> CameraResult<bool> {
    for index in 0..4096 {
        let Some(size) = native::sizes(fd, code, index)? else {
            break;
        };
        if size_supports(&size, width, height) {
            return Ok(true);
        }
        if size.kind != 1 {
            break;
        }
    }
    Ok(false)
}

fn interval_supports(interval: &native::Interval, rate: crate::FrameRate) -> bool {
    if interval.min_n == 0 || interval.min_d == 0 || interval.max_n == 0 || interval.max_d == 0 {
        return false;
    }
    let target_n = i128::from(rate.denominator());
    let target_d = i128::from(rate.numerator());
    let min_n = i128::from(interval.min_n);
    let min_d = i128::from(interval.min_d);
    let max_n = i128::from(interval.max_n);
    let max_d = i128::from(interval.max_d);
    if target_n * min_d < min_n * target_d || target_n * max_d > max_n * target_d {
        return false;
    }
    if interval.kind == 1 {
        return target_n * min_d == min_n * target_d;
    }
    if interval.kind == 2 {
        return true;
    }
    if interval.step_n == 0 || interval.step_d == 0 {
        return false;
    }
    let delta_n = target_n * min_d - min_n * target_d;
    let delta_d = target_d * min_d;
    let scaled_delta = delta_n * i128::from(interval.step_d);
    let scaled_step = delta_d * i128::from(interval.step_n);
    scaled_step != 0 && scaled_delta % scaled_step == 0
}

fn frame_rate_supported(
    fd: i32,
    code: u32,
    width: u32,
    height: u32,
    rate: crate::FrameRate,
) -> CameraResult<bool> {
    for index in 0..4096 {
        let Some(interval) = native::intervals(fd, code, width, height, index)? else {
            break;
        };
        if interval_supports(&interval, rate) {
            return Ok(true);
        }
        if interval.kind != 1 {
            break;
        }
    }
    Ok(false)
}
fn interval_rates(interval: &native::Interval) -> Vec<crate::FrameRate> {
    let Ok(fastest) = crate::FrameRate::new(interval.min_d, interval.min_n) else {
        return vec![];
    };
    let Ok(slowest) = crate::FrameRate::new(interval.max_d, interval.max_n) else {
        return vec![];
    };
    if interval.kind == 1 {
        return vec![fastest];
    }
    let mut rates = vec![slowest, fastest];
    for fps in [1, 5, 10, 15, 24, 25, 30, 50, 60, 90, 120, 240, 480] {
        let duration = 1.0 / fps as f64;
        let min = 1.0 / fastest.as_f64();
        let max = 1.0 / slowest.as_f64();
        if duration < min || duration > max {
            continue;
        }
        let aligned = interval.kind == 2
            || (interval.step_n > 0 && interval.step_d > 0 && {
                let step = interval.step_n as f64 / interval.step_d as f64;
                let steps = (duration - min) / step;
                (steps - steps.round()).abs() < 1e-8
            });
        if aligned {
            let rate = crate::FrameRate::new(fps, 1).unwrap();
            if !rates.contains(&rate) {
                rates.push(rate);
            }
        }
    }
    rates.sort_unstable();
    rates
}
fn frame_rates(fd: i32, code: u32, w: u32, h: u32) -> CameraResult<Vec<crate::FrameRate>> {
    let mut rates = Vec::new();
    for i in 0..4096 {
        let Some(interval) = native::intervals(fd, code, w, h, i)? else {
            break;
        };
        for rate in interval_rates(&interval) {
            if !rates.contains(&rate) {
                rates.push(rate);
            }
        }
        if interval.kind != 1 {
            break;
        }
    }
    rates.sort_unstable();
    rates.reverse();
    Ok(rates)
}

impl StreamingCamera for V4l2Camera {
    fn frame_hub(&self) -> Option<&FrameHub> {
        Some(&self.inner.hub)
    }
    async fn start_stream(&self, config: CameraConfig) -> CameraResult<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.start(config))
            .await
            .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }
    async fn stop_stream(&self) -> CameraResult<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.stop())
            .await
            .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }
    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>> {
        Ok(self.inner.hub.latest().map(|f| {
            f.rgb_pixels()
                .expect("RGB backend adapter")
                .as_ref()
                .clone()
        }))
    }
    async fn wait_for_frame(&self, timeout: Duration) -> CameraResult<Array3<u8>> {
        Ok(self
            .inner
            .hub
            .wait_after(None, timeout)
            .await?
            .rgb_pixels()?
            .as_ref()
            .clone())
    }
    fn is_streaming(&self) -> bool {
        self.inner.hub.is_streaming()
    }
    fn get_config(&self) -> Option<CameraConfig> {
        let state = self.inner.state.lock();
        if self.is_streaming() {
            state.config.clone()
        } else {
            None
        }
    }
    fn get_stats(&self) -> StreamStats {
        self.inner.hub.stats()
    }
    /// Set the kernel MMAP queue depth (2..=32) while stopped. The shared RGB
    /// snapshot pool remains bounded independently at its default capacity.
    async fn set_buffer_size(&self, size: usize) -> CameraResult<()> {
        if !(2..=32).contains(&size) {
            return Err(CameraError::invalid_config(
                "V4L2 buffer count must be between 2 and 32".into(),
            ));
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = inner.state.lock();
            if inner.hub.is_streaming() {
                return Err(CameraError::stream_error(
                    "Stop V4L2 before changing buffer count".into(),
                ));
            }
            state.buffers = size as u32;
            Ok(())
        })
        .await
        .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }
}

// V4L2_CID_BASE and V4L2_CID_CAMERA_CLASS_BASE from linux/v4l2-controls.h.
const BASE: u32 = 0x0098_0900;
const CAMERA: u32 = 0x009a_0900;
const CONTROLS: [CameraControlType; 18] = [
    CameraControlType::Brightness,
    CameraControlType::Contrast,
    CameraControlType::Hue,
    CameraControlType::Saturation,
    CameraControlType::Sharpness,
    CameraControlType::Gamma,
    CameraControlType::WhiteBalance,
    CameraControlType::BacklightCompensation,
    CameraControlType::Gain,
    CameraControlType::Pan,
    CameraControlType::Tilt,
    CameraControlType::Zoom,
    CameraControlType::Exposure,
    CameraControlType::Iris,
    CameraControlType::Focus,
    CameraControlType::AutoExposure,
    CameraControlType::AutoFocus,
    CameraControlType::AutoWhiteBalance,
];
fn control_id(control: CameraControlType) -> u32 {
    use CameraControlType::*;
    match control {
        Brightness => BASE,
        Contrast => BASE + 1,
        Saturation => BASE + 2,
        Hue => BASE + 3,
        AutoWhiteBalance => BASE + 12,
        Gamma => BASE + 16,
        Gain => BASE + 19,
        WhiteBalance => BASE + 26,
        Sharpness => BASE + 27,
        BacklightCompensation => BASE + 28,
        AutoExposure => CAMERA + 1,
        Exposure => CAMERA + 2,
        Pan => CAMERA + 8,
        Tilt => CAMERA + 9,
        Focus => CAMERA + 10,
        AutoFocus => CAMERA + 12,
        Zoom => CAMERA + 13,
        Iris => CAMERA + 17,
    }
}
fn auto_control(control: CameraControlType) -> Option<CameraControlType> {
    use CameraControlType::*;
    match control {
        Exposure => Some(AutoExposure),
        Focus => Some(AutoFocus),
        WhiteBalance => Some(AutoWhiteBalance),
        _ => None,
    }
}
fn control_error(control: CameraControlType, error: std::io::Error) -> CameraError {
    if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) {
        CameraError::control_not_supported(control.display_name().to_string())
    } else {
        error.into()
    }
}
impl V4l2Camera {
    fn query(&self, control: CameraControlType) -> CameraResult<native::Control> {
        native::query_control(self.inner.file.as_raw_fd(), control_id(control))
            .map_err(|e| control_error(control, e))
    }
    fn read_auto(&self, control: CameraControlType) -> CameraResult<bool> {
        let value = native::get_control(self.inner.file.as_raw_fd(), control_id(control))?;
        Ok(if control == CameraControlType::AutoExposure {
            value != 1
        } else {
            value != 0
        })
    }
    fn write_auto(&self, control: CameraControlType, enabled: bool) -> CameraResult<()> {
        let fd = self.inner.file.as_raw_fd();
        if control == CameraControlType::AutoExposure {
            native::auto_exposure(fd, enabled)?;
        } else {
            native::set_control(fd, control_id(control), i32::from(enabled))?;
        }
        Ok(())
    }
}
impl CameraControl for V4l2Camera {
    fn get_control(&self, control: CameraControlType) -> CameraResult<CameraControlValue> {
        let _guard = self.inner.state.lock();
        self.query(control)?;
        if control.is_auto_control() {
            return Ok(CameraControlValue::Boolean(self.read_auto(control)?));
        }
        let value = native::get_control(self.inner.file.as_raw_fd(), control_id(control))?;
        let is_auto = match auto_control(control).filter(|&c| self.query(c).is_ok()) {
            Some(c) => self.read_auto(c)?,
            None => false,
        };
        Ok(CameraControlValue::Integer { value, is_auto })
    }
    fn set_control(
        &self,
        control: CameraControlType,
        value: CameraControlValue,
    ) -> CameraResult<()> {
        let _guard = self.inner.state.lock();
        let range = self.query(control)?;
        if range.read_only != 0 {
            return Err(CameraError::control_not_supported(format!(
                "{control:?} is read-only"
            )));
        }
        let (value, auto) = match value {
            CameraControlValue::Integer { value, is_auto } => (value, is_auto),
            CameraControlValue::Boolean(v) => (i32::from(v), false),
            CameraControlValue::Float(_) => {
                return Err(CameraError::invalid_config(
                    "V4L2 controls require integer or boolean values".into(),
                ))
            }
        };
        if control.is_auto_control() {
            if value != 0 && value != 1 {
                return Err(CameraError::invalid_config(
                    "V4L2 auto controls require 0 or 1".into(),
                ));
            }
            return self.write_auto(control, value != 0);
        }
        let automatic = auto_control(control).filter(|&c| self.query(c).is_ok());
        if auto {
            return self.write_auto(
                automatic.ok_or_else(|| {
                    CameraError::control_not_supported(format!("{control:?} has no automatic mode"))
                })?,
                true,
            );
        }
        if value < range.min
            || value > range.max
            || (i64::from(value) - i64::from(range.min)) % i64::from(range.step.max(1)) != 0
        {
            return Err(CameraError::invalid_config(format!(
                "{control:?} is outside its V4L2 range/step"
            )));
        }
        if let Some(c) = automatic {
            self.write_auto(c, false)?;
        }
        native::set_control(self.inner.file.as_raw_fd(), control_id(control), value)?;
        Ok(())
    }
    fn get_control_range(&self, control: CameraControlType) -> CameraResult<CameraControlRange> {
        let _guard = self.inner.state.lock();
        let range = self.query(control)?;
        if control.is_auto_control() {
            let default = if control == CameraControlType::AutoExposure {
                i32::from(range.def != 1)
            } else {
                range.def
            };
            return Ok(CameraControlRange::new(0, 1, 1, default, false));
        }
        Ok(CameraControlRange::new(
            range.min,
            range.max,
            range.step.max(1),
            range.def,
            auto_control(control).is_some_and(|c| self.query(c).is_ok()),
        ))
    }
    fn supports_control(&self, control: CameraControlType) -> bool {
        self.get_control_range(control).is_ok()
    }
    fn get_supported_controls(&self) -> Vec<CameraControlType> {
        CONTROLS
            .into_iter()
            .filter(|&c| self.supports_control(c))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn startup_failure_stops_readers_and_is_safe_to_retry() {
        // A real non-camera fd exercises the native ioctl failure and worker
        // rollback without requiring camera hardware or changing host state.
        let camera = V4l2Camera {
            inner: Arc::new(Inner {
                file: Arc::new(File::open("/dev/null").unwrap()),
                hub: Arc::new(FrameHub::default()),
                state: Mutex::new(State {
                    worker: None,
                    wake: None,
                    config: None,
                    buffers: 4,
                }),
            }),
        };
        assert!(camera.set_buffer_size(1).await.is_err());
        assert!(camera.set_buffer_size(33).await.is_err());
        camera.set_buffer_size(8).await.unwrap();
        for _ in 0..2 {
            assert!(camera
                .start_stream(CameraConfig::default_yuyv())
                .await
                .is_err());
            assert!(!camera.is_streaming());
            assert!(camera.get_config().is_none());
            assert!(camera.get_latest_frame().unwrap().is_none());
            assert!(camera.wait_for_frame(Duration::from_secs(1)).await.is_err());
            camera.stop_stream().await.unwrap();
            assert!(camera.inner.state.lock().worker.is_none());
        }
        assert!(camera
            .start_stream(CameraConfig::new(VideoFormat::H264, 640, 480, 30))
            .await
            .is_err());
        assert!(!camera.supports_control(CameraControlType::Exposure));
        assert!(camera.get_supported_controls().is_empty());
    }

    #[test]
    fn ranged_resolutions_respect_steps_and_bounds() {
        let size = native::Size {
            kind: 3,
            min_w: 320,
            max_w: 1920,
            step_w: 16,
            min_h: 240,
            max_h: 1080,
            step_h: 16,
        };
        let dims = dimensions(&size);
        assert!(dims.contains(&(320, 240)));
        assert!(dims.contains(&(640, 480)));
        assert!(!dims.contains(&(1920, 1080)));
        assert!(!dims.contains(&(3840, 2160)));
        assert!(size_supports(&size, 1600, 880));
        assert!(!size_supports(&size, 1600, 900));
        assert!(!size_supports(&size, 1936, 1080));
    }
    #[test]
    fn interval_enumeration_handles_fractional_and_ranged_rates() {
        assert_eq!(
            interval_rates(&native::Interval {
                kind: 1,
                min_n: 1001,
                min_d: 30000,
                max_n: 1001,
                max_d: 30000,
                ..Default::default()
            }),
            vec![crate::FrameRate::new(30000, 1001).unwrap()]
        );
        assert!(interval_rates(&native::Interval::default()).is_empty());
        let rates = interval_rates(&native::Interval {
            kind: 3,
            min_n: 1,
            min_d: 30,
            max_n: 1,
            max_d: 10,
            step_n: 1,
            step_d: 30,
        });
        assert_eq!(
            rates,
            vec![10, 15, 30]
                .into_iter()
                .map(|r| crate::FrameRate::new(r, 1).unwrap())
                .collect::<Vec<_>>()
        );
        let range = native::Interval {
            kind: 3,
            min_n: 1001,
            min_d: 60000,
            max_n: 1001,
            max_d: 15000,
            step_n: 1001,
            step_d: 60000,
        };
        assert!(interval_supports(
            &range,
            crate::FrameRate::new(30000, 1001).unwrap()
        ));
        assert!(!interval_supports(
            &range,
            crate::FrameRate::new(24, 1).unwrap()
        ));
        assert!(!interval_supports(
            &range,
            crate::FrameRate::new(120, 1).unwrap()
        ));
    }
}
