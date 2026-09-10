//! Device selection and owned capture sessions.
use crate::{
    backends::{self, BackendCamera, BackendType},
    format::*,
    CameraConfig, CameraDeviceInfo, CameraError, CameraResult, Frame, FrameHub, FrameMetrics,
    FrameReceiver, StreamStats, VideoFormat,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{watch, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceId {
    backend: Option<BackendType>,
    native: String,
}
impl DeviceId {
    pub fn backend(&self) -> Option<BackendType> {
        self.backend
    }
    pub fn native_id(&self) -> &str {
        &self.native
    }
}
impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            self.backend.map_or("synthetic".into(), |b| b.to_string()),
            self.native
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityStability {
    Native,
    EnumerationOnly,
}
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub description: String,
    pub stability: IdentityStability,
    pub index: u32,
}
#[derive(Clone, Debug)]
pub struct DeviceCapabilities {
    /// Advertised discrete modes and representative samples from continuous ranges.
    /// An unlisted rate may still be tried with Exact and validated by the driver.
    pub configurations: Vec<CameraConfig>,
    pub native_output: bool,
    pub rgb_output: bool,
}
/// Enumeration is isolated from the async executor. Explicit backend requests
/// never fall back to another device namespace.
#[derive(Clone, Debug, Default)]
pub struct CameraSystem {
    backend: Option<BackendType>,
    synthetic: bool,
}
impl CameraSystem {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_backend(backend: BackendType) -> Self {
        Self {
            backend: Some(backend),
            synthetic: false,
        }
    }
    /// Deterministic, timer-driven RGB source for examples and hardware-free tests.
    pub fn synthetic() -> Self {
        Self {
            backend: None,
            synthetic: true,
        }
    }
    pub fn available_backends() -> Vec<BackendType> {
        backends::available_backends()
    }
    /// Blocking enumeration for native worker threads and synchronous applications.
    pub fn devices_blocking(&self) -> CameraResult<Vec<DeviceInfo>> {
        if self.synthetic {
            return Ok(vec![device_info_synthetic()]);
        }
        let backend = resolve_backend(self.backend.unwrap_or(BackendType::Auto))?;
        backends::list_devices(backend)
            .map(|v| v.into_iter().map(|d| device_info(backend, d)).collect())
    }
    pub fn capabilities_blocking(&self, id: &DeviceId) -> CameraResult<DeviceCapabilities> {
        if id.backend.is_none() && !self.synthetic {
            return Err(CameraError::DeviceNotFound(id.to_string()));
        }
        let system = if let Some(b) = id.backend {
            Self::with_backend(b)
        } else {
            Self::synthetic()
        };
        let devices = system.devices_blocking()?;
        let matching: Vec<_> = devices.iter().filter(|d| d.id == *id).collect();
        if matching.len() > 1 {
            return Err(CameraError::AmbiguousDevice(id.to_string()));
        }
        let device = matching
            .first()
            .ok_or_else(|| CameraError::DeviceNotFound(id.to_string()))?;
        let configurations = if let Some(b) = id.backend {
            backends::get_supported_configs(b, device.index)?
        } else {
            vec![CameraConfig::new(VideoFormat::RGB, 640, 480, 30)]
        };
        Ok(DeviceCapabilities {
            configurations,
            native_output: true,
            rgb_output: true,
        })
    }
    pub async fn devices(&self) -> CameraResult<Vec<DeviceInfo>> {
        if self.synthetic {
            return Ok(vec![DeviceInfo {
                id: DeviceId {
                    backend: None,
                    native: "test-pattern".into(),
                },
                name: "Synthetic RGB camera".into(),
                description: "Deterministic capture source".into(),
                stability: IdentityStability::Native,
                index: 0,
            }]);
        }
        let backend = self.backend.unwrap_or(BackendType::Auto);
        let backend = resolve_backend(backend)?;
        tokio::task::spawn_blocking(move || {
            backends::list_devices(backend)
                .map(|v| v.into_iter().map(|d| device_info(backend, d)).collect())
        })
        .await
        .map_err(join_error)?
    }
    pub async fn capabilities(&self, id: &DeviceId) -> CameraResult<DeviceCapabilities> {
        if id.backend.is_none() {
            self.resolve(id).await?;
            return Ok(DeviceCapabilities {
                configurations: vec![CameraConfig::new(VideoFormat::RGB, 640, 480, 30)],
                native_output: true,
                rgb_output: true,
            });
        }
        let index = self.resolve(id).await?.index;
        let backend = id.backend.unwrap();
        let configurations =
            tokio::task::spawn_blocking(move || backends::get_supported_configs(backend, index))
                .await
                .map_err(join_error)??;
        Ok(DeviceCapabilities {
            configurations,
            native_output: true,
            rgb_output: true,
        })
    }
    async fn resolve(&self, id: &DeviceId) -> CameraResult<DeviceInfo> {
        let system = if let Some(b) = id.backend {
            Self::with_backend(b)
        } else {
            Self::synthetic()
        };
        if id.backend.is_none() && !self.synthetic {
            return Err(CameraError::DeviceNotFound(id.to_string()));
        }
        let mut matches = system.devices().await?.into_iter().filter(|d| d.id == *id);
        let device = matches
            .next()
            .ok_or_else(|| CameraError::DeviceNotFound(id.to_string()))?;
        if matches.next().is_some() {
            return Err(CameraError::AmbiguousDevice(id.to_string()));
        }
        Ok(device)
    }
    pub async fn open(&self, id: &DeviceId) -> CameraResult<Camera> {
        let device = self.resolve(id).await?;
        let source = if let Some(backend) = id.backend {
            let index = device.index;
            let expected = id.native.clone();
            Source::native(
                tokio::task::spawn_blocking(move || {
                    backends::create_selected_camera(
                        index,
                        backend,
                        FrameHub::default(),
                        Some(&expected),
                    )
                })
                .await
                .map_err(join_error)??,
                Some(id.clone()),
            )?
        } else {
            Source::Synthetic(FrameHub::default())
        };
        let hub = source.hub()?;
        Ok(Camera {
            device,
            source,
            hub,
            busy: Arc::new(AtomicBool::new(false)),
        })
    }
    /// Open a borrowed, already-authorized USB descriptor. The backend duplicates
    /// it; ownership of the original descriptor stays with the caller.
    #[cfg(all(
        unix,
        all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        )
    ))]
    pub async fn open_usb_fd(&self, fd: std::os::fd::BorrowedFd<'_>) -> CameraResult<Camera> {
        use std::os::fd::AsRawFd;
        let owned = fd.try_clone_to_owned()?;
        let backend =
            tokio::task::spawn_blocking(move || backends::create_camera_from_fd(owned.as_raw_fd()))
                .await
                .map_err(join_error)??;
        let source = Source::native(backend, None)?;
        let hub = source.hub()?;
        Ok(Camera {
            device: DeviceInfo {
                id: DeviceId {
                    backend: Some(BackendType::Uvc),
                    native: "authorized-usb-fd".into(),
                },
                name: "Authorized USB camera".into(),
                description: String::new(),
                stability: IdentityStability::EnumerationOnly,
                index: 0,
            },
            source,
            hub,
            busy: Arc::new(AtomicBool::new(false)),
        })
    }
}
fn device_info_synthetic() -> DeviceInfo {
    DeviceInfo {
        id: DeviceId {
            backend: None,
            native: "test-pattern".into(),
        },
        name: "Synthetic RGB camera".into(),
        description: "Deterministic capture source".into(),
        stability: IdentityStability::Native,
        index: 0,
    }
}
fn device_info(backend: BackendType, d: CameraDeviceInfo) -> DeviceInfo {
    let stable = d.device_path.as_ref().is_some_and(|s| {
        !s.is_empty() && !(backend == BackendType::V4l2 && s.starts_with("/dev/video"))
    }) || d.serial_number.as_ref().is_some_and(|s| !s.is_empty());
    DeviceInfo {
        id: DeviceId {
            backend: Some(backend),
            native: d.unique_id(),
        },
        name: d.name,
        description: d.description,
        index: d.index,
        stability: if stable {
            IdentityStability::Native
        } else {
            IdentityStability::EnumerationOnly
        },
    }
}
fn resolve_backend(b: BackendType) -> CameraResult<BackendType> {
    let b = if b == BackendType::Auto {
        BackendType::default_for_platform()
    } else {
        b
    };
    if !b.is_available() {
        Err(CameraError::BackendUnavailable(b.to_string()))
    } else {
        Ok(b)
    }
}
fn join_error(e: tokio::task::JoinError) -> CameraError {
    CameraError::Other(format!("Camera worker: {e}"))
}
#[derive(Clone)]
enum Source {
    Native(Arc<NativeSource>),
    Synthetic(FrameHub),
}

struct NativeSource {
    camera: parking_lot::Mutex<Option<BackendCamera>>,
    id: Option<DeviceId>,
    hub: FrameHub,
}
impl NativeSource {
    fn current(&self) -> CameraResult<BackendCamera> {
        self.camera
            .lock()
            .clone()
            .ok_or_else(|| CameraError::Disconnected("Device is reconnecting".into()))
    }
    fn backend_type(&self) -> BackendType {
        self.id
            .as_ref()
            .and_then(|id| id.backend)
            .unwrap_or(BackendType::Uvc)
    }
    fn get_config(&self) -> Option<CameraConfig> {
        self.current().ok().and_then(|c| c.get_config())
    }
    async fn start_stream(&self, c: CameraConfig) -> CameraResult<()> {
        self.current()?.start_stream(c).await
    }
    async fn stop_stream(&self) -> CameraResult<()> {
        if let Ok(c) = self.current() {
            c.stop_stream().await
        } else {
            Ok(())
        }
    }
    async fn set_buffer_size(&self, n: usize) -> CameraResult<()> {
        self.current()?.set_buffer_size(n).await
    }
    async fn reopen(self: &Arc<Self>) -> CameraResult<()> {
        let id = self.id.clone().ok_or_else(|| {
            CameraError::Disconnected(
                "USB permission descriptor requires explicit reopening".into(),
            )
        })?;
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            drop(this.camera.lock().take());
            let backend = id.backend.unwrap();
            let mut matches = backends::list_devices(backend)?
                .into_iter()
                .filter(|d| d.unique_id() == id.native);
            let device = matches
                .next()
                .ok_or_else(|| CameraError::Disconnected(id.to_string()))?;
            if matches.next().is_some() {
                return Err(CameraError::AmbiguousDevice(id.to_string()));
            }
            let camera = backends::create_selected_camera(
                device.index,
                backend,
                this.hub.clone(),
                Some(&id.native),
            )?;
            *this.camera.lock() = Some(camera);
            Ok(())
        })
        .await
        .map_err(join_error)?
    }
}
impl Source {
    fn native(camera: BackendCamera, id: Option<DeviceId>) -> CameraResult<Self> {
        let hub = camera
            .frame_hub()
            .cloned()
            .ok_or_else(|| CameraError::InvalidState("Backend has no frame source".into()))?;
        Ok(Self::Native(Arc::new(NativeSource {
            camera: parking_lot::Mutex::new(Some(camera)),
            id,
            hub,
        })))
    }

    fn hub(&self) -> CameraResult<FrameHub> {
        match self {
            Self::Native(c) => Ok(c.hub.clone()),
            Self::Synthetic(h) => Ok(h.clone()),
        }
    }
    async fn start(&self, config: CameraConfig) -> CameraResult<()> {
        match self {
            Self::Native(c) => c.start_stream(config).await,
            Self::Synthetic(h) => {
                let session = h.start();
                let hub = h.clone();
                let interval = config.frame_rate()?.interval();
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(interval);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    let mut seq = 0u64;
                    while hub.is_streaming() && hub.session() == session {
                        ticker.tick().await;
                        if !hub.is_streaming() || hub.session() != session {
                            break;
                        }
                        seq += 1;
                        if hub.wants_native() {
                            let _ = hub.publish_native_into(
                                session,
                                FrameLayout::rgb(config.width, config.height),
                                None,
                                |out| {
                                    out.fill((seq % 255) as u8);
                                    Ok(())
                                },
                            );
                        } else {
                            let _ =
                                hub.publish_rgb(session, config.width, config.height, None, |b| {
                                    b.fill((seq % 255) as u8);
                                    Ok(())
                                });
                        }
                    }
                });
                Ok(())
            }
        }
    }
    async fn stop(&self) -> CameraResult<()> {
        match self {
            Self::Native(c) => c.stop_stream().await,
            Self::Synthetic(h) => {
                h.stop();
                Ok(())
            }
        }
    }
    fn actual(&self, fallback: &CameraConfig) -> CameraResult<CameraConfig> {
        match self {
            Self::Native(c) => c.get_config().ok_or_else(|| {
                CameraError::InvalidState("Backend did not report negotiated configuration".into())
            }),
            Self::Synthetic(_) => Ok(fallback.clone()),
        }
    }
}
/// Open device handle. Starting returns the sole owner of capture lifetime.
pub struct Camera {
    device: DeviceInfo,
    source: Source,
    hub: FrameHub,
    busy: Arc<AtomicBool>,
}
impl std::fmt::Debug for Camera {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Camera")
            .field("device", &self.device)
            .field("busy", &self.busy.load(Ordering::Acquire))
            .finish()
    }
}
impl Camera {
    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }
    pub async fn start(&mut self, request: StreamRequest) -> CameraResult<CaptureSession> {
        tokio::time::timeout(request.startup_timeout, self.start_inner(request))
            .await
            .map_err(|_| CameraError::Timeout)?
    }
    async fn start_inner(&mut self, request: StreamRequest) -> CameraResult<CaptureSession> {
        let startup_started = std::time::Instant::now();
        if request.output == OutputFormat::Rgb8 && request.format == Some(VideoFormat::H264) {
            return Err(CameraError::UnsupportedFormat(
                "H264 requires native output and an external decoder".into(),
            ));
        }
        if matches!(self.source, Source::Synthetic(_))
            && request.format.is_some_and(|f| f != VideoFormat::RGB)
        {
            return Err(CameraError::UnsupportedFormat(
                "Synthetic source produces RGB only".into(),
            ));
        }
        if request.reconnect.is_some()
            && self.device.stability == IdentityStability::EnumerationOnly
        {
            return Err(CameraError::InvalidConfig(
                "Automatic reconnect requires a stable native identity".into(),
            ));
        }
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(CameraError::InvalidState(
                "Camera is already starting, capturing, or closing".into(),
            ));
        }
        if let Err(e) = self.hub.configure(&request) {
            self.busy.store(false, Ordering::Release);
            return Err(e);
        }
        let (state, _) = watch::channel(SessionState::Starting);
        let inner = Arc::new(SessionInner {
            device: self.device.clone(),
            source: self.source.clone(),
            hub: self.hub.clone(),
            busy: self.busy.clone(),
            state,
            stopping: Arc::new(AtomicBool::new(false)),
            closed: AtomicBool::new(false),
            terminal_metrics: parking_lot::Mutex::new(None),
            requested_controls: std::sync::Mutex::new(std::collections::HashMap::new()),
            lifecycle: Mutex::new(()),
        });
        let guard = StartGuard(Some(inner.clone()));
        let mut candidates = if matches!(self.source, Source::Synthetic(_)) {
            vec![CameraConfig::new(
                VideoFormat::RGB,
                request.width,
                request.height,
                request.fps.numerator(),
            )
            .with_frame_rate(request.fps)]
        } else if matches!(&self.source,Source::Native(n) if n.id.is_none()) {
            vec![CameraConfig::new(
                request.format.unwrap_or(VideoFormat::MJPEG),
                request.width,
                request.height,
                request.fps.numerator(),
            )
            .with_frame_rate(request.fps)]
        } else {
            let b = self.device.id.backend.unwrap();
            let index = self.device.index;
            let supported =
                tokio::task::spawn_blocking(move || backends::get_supported_configs(b, index))
                    .await
                    .map_err(join_error)??;
            select_configs(&request, supported)?
        };
        if let Some(count) = request.driver_buffers {
            if let Source::Native(c) = &inner.source {
                if c.backend_type() != BackendType::V4l2 {
                    return Err(CameraError::InvalidConfig(
                        "Driver queue configuration is only supported by V4L2".into(),
                    ));
                }
                c.set_buffer_size(count).await?;
            }
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task_inner = inner.clone();
        let startup = request.startup_timeout;
        tokio::spawn(async move {
            let _lock = task_inner.lifecycle.lock().await;
            let mut result = Err(CameraError::UnsupportedFormat(
                "No usable capture configuration".into(),
            ));
            for config in candidates.drain(..) {
                if task_inner.stopping.load(Ordering::Acquire) || tx.is_closed() {
                    break;
                }
                let attempt = async {
                    task_inner.source.start(config.clone()).await?;
                    let frame = task_inner.hub.wait_after(None, startup).await?;
                    Ok::<_, CameraError>((task_inner.source.actual(&config)?, frame))
                }
                .await;
                match attempt {
                    Ok(ok) => {
                        result = Ok(ok);
                        break;
                    }
                    Err(e) => {
                        result = Err(e);
                        let _ = task_inner.source.stop().await;
                    }
                }
            }
            if task_inner.stopping.load(Ordering::Acquire) || tx.is_closed() || result.is_err() {
                let _ = task_inner.source.stop().await;
                task_inner.stopping.store(true, Ordering::Release);
                task_inner.closed.store(true, Ordering::Release);
                task_inner.state.send_replace(SessionState::Stopped);
                task_inner.busy.store(false, Ordering::Release);
            }
            let _ = tx.send(result);
        });
        let (actual, first) = tokio::time::timeout(request.startup_timeout, rx)
            .await
            .map_err(|_| CameraError::Timeout)?
            .map_err(|_| CameraError::StreamStopped)??;
        let frame_rate = actual.frame_rate()?;
        let mut adjustments = Vec::new();
        if (actual.width, actual.height) != (request.width, request.height) {
            adjustments.push(format!(
                "Resolution {}x{} selected",
                actual.width, actual.height
            ));
        }
        if frame_rate != request.fps {
            adjustments.push(format!(
                "Requested {} fps, selected {} fps",
                request.fps.as_f64(),
                frame_rate.as_f64()
            ));
        }
        if request.selection == SelectionPolicy::Exact
            && ((actual.width, actual.height) != (request.width, request.height)
                || frame_rate != request.fps
                || request.format.is_some_and(|f| f != actual.format))
        {
            return Err(CameraError::UnsupportedFormat(
                "Native driver changed an exact request".into(),
            ));
        }
        if request.format.is_some_and(|f| f != actual.format) {
            adjustments.push(format!("Capture format {:?} selected", actual.format));
        }
        if first.layout().width != actual.width || first.layout().height != actual.height {
            return Err(CameraError::InvalidFormat(
                "First frame differs from negotiated dimensions".into(),
            ));
        }
        inner.state.send_replace(SessionState::Streaming);
        let session = CaptureSession {
            inner,
            negotiated: NegotiatedConfig {
                capture: actual,
                frame_rate,
                output: request.output,
                first_frame_latency: startup_started.elapsed(),
                adjustments,
            },
        };
        if let Some(policy) = request.reconnect {
            session.spawn_recovery(policy);
        }
        guard.disarm();
        Ok(session)
    }
}
fn select_configs(
    r: &StreamRequest,
    mut supported: Vec<CameraConfig>,
) -> CameraResult<Vec<CameraConfig>> {
    let enumeration_known = !supported.is_empty();
    supported.retain(|c| {
        r.format.is_none_or(|f| f == c.format)
            && c.validate().is_ok()
            && !(r.output == OutputFormat::Rgb8 && c.format == VideoFormat::H264)
    });
    // Empty enumeration is unknown, not an invented capability. Explicit requests
    // may be attempted, with first-frame and actual-format validation afterwards.
    if supported.is_empty() && enumeration_known {
        return Err(CameraError::UnsupportedFormat(
            "Requested capture format is not advertised".into(),
        ));
    }
    if supported.is_empty() {
        supported.push(
            CameraConfig::new(
                r.format.unwrap_or(VideoFormat::MJPEG),
                r.width,
                r.height,
                r.fps.numerator(),
            )
            .with_frame_rate(r.fps),
        );
    }
    // Enumerated FPS lists can be samples from continuous driver ranges.
    // Try the requested rate on advertised dimensions; actual negotiation remains authoritative.
    let trials: Vec<_> = supported
        .iter()
        .filter(|c| c.width == r.width && c.height == r.height)
        .map(|c| c.clone().with_frame_rate(r.fps))
        .collect();
    supported.extend(trials);
    if r.selection == SelectionPolicy::Exact {
        supported.retain(|c| {
            c.width == r.width && c.height == r.height && c.frame_rate().ok() == Some(r.fps)
        });
    }
    supported.sort_by_key(|c| {
        (
            (c.width as i64 - r.width as i64).unsigned_abs()
                + (c.height as i64 - r.height as i64).unsigned_abs(),
            ((c.frame_rate().unwrap().as_f64() - r.fps.as_f64()).abs() * 1_000_000.0) as u64,
        )
    });
    supported.dedup();
    if supported.is_empty() {
        return Err(CameraError::UnsupportedFormat(
            "Requested capture mode is unavailable".into(),
        ));
    }
    Ok(supported)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Streaming,
    Recovering { attempt: u32 },
    Stopping,
    Stopped,
    Failed(String),
}
struct SessionInner {
    device: DeviceInfo,
    source: Source,
    hub: FrameHub,
    busy: Arc<AtomicBool>,
    state: watch::Sender<SessionState>,
    stopping: Arc<AtomicBool>,
    closed: AtomicBool,
    terminal_metrics: parking_lot::Mutex<Option<(FrameMetrics, StreamStats)>>,
    requested_controls:
        std::sync::Mutex<std::collections::HashMap<crate::ControlId, crate::ControlValue>>,
    lifecycle: Mutex<()>,
}
impl SessionInner {
    fn request_stop(self: &Arc<Self>) {
        if self.stopping.swap(true, Ordering::AcqRel) || self.closed.load(Ordering::Acquire) {
            return;
        }
        self.state.send_replace(SessionState::Stopping);
        self.hub.end_recovery();
        self.hub.stop();
        let inner = self.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = inner.stop().await;
            });
        } else {
            std::thread::spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    let _ = rt.block_on(inner.stop());
                }
            });
        }
    }
    async fn stop(&self) -> CameraResult<()> {
        let _guard = self.lifecycle.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return match &*self.state.borrow() {
                SessionState::Failed(error) => Err(CameraError::StreamError(error.clone())),
                _ => Ok(()),
            };
        }
        self.stopping.store(true, Ordering::Release);
        self.hub.end_recovery();
        self.hub.stop();
        let result = self.source.stop().await;
        *self.terminal_metrics.lock() = Some((self.hub.metrics(), self.hub.stats()));
        self.closed.store(true, Ordering::Release);
        self.state.send_replace(match &result {
            Ok(_) => SessionState::Stopped,
            Err(e) => SessionState::Failed(e.to_string()),
        });
        self.busy.store(false, Ordering::Release);
        result
    }
}
struct StartGuard(Option<Arc<SessionInner>>);
impl StartGuard {
    fn disarm(mut self) {
        self.0.take();
    }
}
impl Drop for StartGuard {
    fn drop(&mut self) {
        if let Some(i) = self.0.take() {
            i.request_stop();
        }
    }
}
/// Owning stream session. Drop requests stop immediately; stop().await also
/// waits for native shutdown. Receivers never own permission to stop capture.
pub struct CaptureSession {
    inner: Arc<SessionInner>,
    negotiated: NegotiatedConfig,
}
impl std::fmt::Debug for CaptureSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSession")
            .field("negotiated", &self.negotiated)
            .field("state", &self.state())
            .finish()
    }
}
impl CaptureSession {
    pub fn device(&self) -> &DeviceInfo {
        &self.inner.device
    }
    pub fn is_streaming(&self) -> bool {
        !self.inner.stopping.load(Ordering::Acquire) && self.inner.hub.is_streaming()
    }
    pub fn negotiated_config(&self) -> &NegotiatedConfig {
        &self.negotiated
    }
    pub fn subscribe(&self) -> FrameReceiver {
        self.inner
            .hub
            .subscribe()
            .with_stop_flag(self.inner.stopping.clone())
    }
    pub fn latest(&self) -> Option<Frame> {
        if self.inner.stopping.load(Ordering::Acquire) {
            None
        } else {
            self.inner.hub.latest()
        }
    }
    pub fn metrics(&self) -> FrameMetrics {
        self.inner
            .terminal_metrics
            .lock()
            .as_ref()
            .map(|(metrics, _)| metrics.clone())
            .unwrap_or_else(|| self.inner.hub.metrics())
    }
    pub fn stats(&self) -> StreamStats {
        self.inner
            .terminal_metrics
            .lock()
            .as_ref()
            .map(|(_, stats)| stats.clone())
            .unwrap_or_else(|| self.inner.hub.stats())
    }
    pub fn state(&self) -> SessionState {
        self.inner.state.borrow().clone()
    }
    pub fn events(&self) -> watch::Receiver<SessionState> {
        self.inner.state.subscribe()
    }
    pub async fn stop(&self) -> CameraResult<()> {
        self.inner.request_stop();
        self.inner.stop().await
    }
    fn spawn_recovery(&self, policy: ReconnectPolicy) {
        let weak = Arc::downgrade(&self.inner);
        let config = self.negotiated.capture.clone();
        tokio::spawn(async move {
            let mut attempt = 0;
            loop {
                tokio::time::sleep(policy.delay).await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                if inner.stopping.load(Ordering::Acquire) {
                    break;
                }
                let stale = inner
                    .hub
                    .latest()
                    .is_none_or(|f| f.age() > policy.stall_timeout);
                if !stale {
                    attempt = 0;
                    continue;
                }
                let _guard = inner.lifecycle.lock().await;
                if inner.stopping.load(Ordering::Acquire) {
                    break;
                }
                attempt += 1;
                inner
                    .state
                    .send_replace(SessionState::Recovering { attempt });
                inner.hub.begin_recovery();
                let result = async {
                    inner.source.stop().await?;
                    if let Source::Native(native) = &inner.source {
                        native.reopen().await?;
                        inner.requested_controls.lock().unwrap().clear();
                    }
                    if inner.stopping.load(Ordering::Acquire) {
                        return Err(CameraError::StreamStopped);
                    }
                    inner.source.start(config.clone()).await?;
                    let first = inner.hub.wait_after(None, policy.stall_timeout).await?;
                    validate_recovered_capture(
                        &config,
                        &inner.source.actual(&config)?,
                        first.layout(),
                    )?;
                    Ok::<_, CameraError>(())
                }
                .await;
                if inner.stopping.load(Ordering::Acquire) {
                    let _ = inner.source.stop().await;
                    break;
                }
                match result {
                    Ok(()) => {
                        inner.hub.end_recovery();
                        inner.state.send_replace(SessionState::Streaming);
                        attempt = 0;
                    }
                    Err(e) => {
                        // A failed first-frame/configuration check must not leave
                        // a live source preventing the next recovery attempt.
                        let _ = inner.source.stop().await;
                        inner.hub.stop();
                        if policy.max_attempts.is_some_and(|n| attempt >= n) {
                            inner
                                .state
                                .send_replace(SessionState::Failed(e.to_string()));
                            inner.stopping.store(true, Ordering::Release);
                            inner.hub.end_recovery();
                            inner.hub.stop();
                            let _ = inner.source.stop().await;
                            *inner.terminal_metrics.lock() =
                                Some((inner.hub.metrics(), inner.hub.stats()));
                            inner.closed.store(true, Ordering::Release);
                            inner.busy.store(false, Ordering::Release);
                            break;
                        }
                    }
                }
            }
        });
    }
    pub(crate) async fn with_native<T: Send + 'static>(
        &self,
        operation: impl FnOnce(BackendCamera) -> CameraResult<T> + Send + 'static,
    ) -> CameraResult<T> {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _guard = inner.lifecycle.lock().await;
            if inner.stopping.load(Ordering::Acquire) {
                return Err(CameraError::StreamStopped);
            }
            let Source::Native(camera) = &inner.source else {
                return Err(CameraError::ControlNotSupported(
                    "Synthetic source has no device controls".into(),
                ));
            };
            let camera = camera.current()?;
            tokio::task::spawn_blocking(move || operation(camera))
                .await
                .map_err(join_error)?
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) fn requested_control(&self, id: crate::ControlId) -> Option<crate::ControlValue> {
        self.inner
            .requested_controls
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
    }
    pub(crate) fn record_control(&self, id: crate::ControlId, value: crate::ControlValue) {
        self.inner
            .requested_controls
            .lock()
            .unwrap()
            .insert(id, value);
    }
}
fn validate_recovered_capture(
    expected: &CameraConfig,
    actual: &CameraConfig,
    first: &crate::FrameLayout,
) -> CameraResult<()> {
    if (actual.width, actual.height, actual.format)
        != (expected.width, expected.height, expected.format)
        || actual.frame_rate()? != expected.frame_rate()?
    {
        return Err(CameraError::UnsupportedFormat(
            "Recovered capture differs from the negotiated configuration".into(),
        ));
    }
    if (first.width, first.height) != (actual.width, actual.height) {
        return Err(CameraError::InvalidFormat(
            "Recovered first frame differs from negotiated dimensions".into(),
        ));
    }
    Ok(())
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        if !self.inner.stopping.load(Ordering::Acquire) {
            self.inner.request_stop();
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn recovery_requires_the_original_mode_and_a_matching_frame() {
        let config = CameraConfig::new(VideoFormat::RGB, 8, 8, 30);
        let hub = FrameHub::default();
        let epoch = hub.start();
        hub.publish_rgb(epoch, 8, 8, None, |_| Ok(())).unwrap();
        let first = hub.latest().unwrap();
        assert!(validate_recovered_capture(&config, &config, first.layout()).is_ok());
        let equivalent = CameraConfig {
            fps: 60,
            fps_denominator: 2,
            ..config.clone()
        };
        assert!(validate_recovered_capture(&config, &equivalent, first.layout()).is_ok());
        for changed in [
            CameraConfig {
                fps: 15,
                ..config.clone()
            },
            CameraConfig {
                width: 16,
                ..config.clone()
            },
            CameraConfig {
                format: VideoFormat::NV12,
                ..config.clone()
            },
        ] {
            assert!(matches!(
                validate_recovered_capture(&config, &changed, first.layout()),
                Err(CameraError::UnsupportedFormat(_))
            ));
        }
        let mut layout = first.layout().clone();
        layout.height = 16;
        assert!(matches!(
            validate_recovered_capture(&config, &config, &layout),
            Err(CameraError::InvalidFormat(_))
        ));
    }
}
