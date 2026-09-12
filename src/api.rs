//! Device selection and owned capture sessions.
use crate::extension::RegisteredBackend;
use crate::{
    backends::{self, BackendCamera, BackendType},
    format::*,
    BackendAvailability, BackendId, BackendOptions, BackendPolicy, CameraConfig, CameraDeviceInfo,
    CameraError, CameraFacing, CameraResult, CaptureFormat, CaptureMode, CapturePlan,
    CaptureProfile, CaptureRequest, DeviceCapabilities, DeviceId, DeviceInfo, DeviceSelector,
    Frame, FrameHub, FrameMetrics, FrameReceiver, IdentityStability, NegotiatedCapture,
    StreamStats, SubscriptionOptions, UsbIdentity, VideoFormat,
};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{broadcast, watch, Mutex};

#[derive(Clone, Copy)]
struct StartupDeadline(tokio::time::Instant);

impl StartupDeadline {
    fn new(timeout: Duration) -> Self {
        Self(tokio::time::Instant::now() + timeout)
    }

    async fn run<T>(
        self,
        stage: crate::OperationStage,
        future: impl std::future::Future<Output = CameraResult<T>>,
    ) -> CameraResult<T> {
        tokio::time::timeout_at(self.0, future)
            .await
            .map_err(|_| CameraError::Timeout { stage })?
    }

    fn remaining(self) -> Duration {
        self.0
            .saturating_duration_since(tokio::time::Instant::now())
    }
}

/// Enumeration is isolated from the async executor. Explicit backend requests
/// never fall back to another device namespace.
#[derive(Clone, Debug)]
pub struct CameraSystem {
    policy: BackendPolicy,
    synthetic: bool,
    registered: Arc<HashMap<BackendId, RegisteredBackend>>,
    backend_options: Arc<Vec<BackendOptions>>,
}

#[derive(Default)]
pub struct SystemBuilder {
    policy: BackendPolicy,
    registered: HashMap<BackendId, RegisteredBackend>,
    backend_options: Vec<BackendOptions>,
}

impl std::fmt::Debug for SystemBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemBuilder")
            .field("policy", &self.policy)
            .field("registered", &self.registered.keys().collect::<Vec<_>>())
            .field("backend_options", &self.backend_options)
            .finish()
    }
}

impl SystemBuilder {
    pub fn backend_policy(mut self, policy: BackendPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn register_backend(mut self, provider: impl crate::BackendProvider) -> Self {
        let id = provider.id();
        self.registered.insert(
            id.clone(),
            RegisteredBackend {
                id,
                provider: Arc::new(provider),
            },
        );
        self
    }

    pub fn backend_options(mut self, options: impl Into<BackendOptions>) -> Self {
        self.backend_options.push(options.into());
        self
    }

    pub fn build(self) -> CameraResult<CameraSystem> {
        for options in &self.backend_options {
            options.validate()?;
        }
        if let BackendPolicy::Require(selected) = &self.policy {
            for options in &self.backend_options {
                let option_backend = options.backend();
                if &option_backend != selected {
                    return Err(CameraError::BackendOptionMismatch {
                        option_backend,
                        selected_backend: selected.clone(),
                    });
                }
            }
        }
        if let BackendPolicy::Prefer(backends) = &self.policy {
            if backends.is_empty() {
                return Err(CameraError::InvalidConfig(
                    "Backend preference list cannot be empty".into(),
                ));
            }
        }
        Ok(CameraSystem {
            policy: self.policy,
            synthetic: false,
            registered: Arc::new(self.registered),
            backend_options: Arc::new(self.backend_options),
        })
    }
}

impl Default for CameraSystem {
    fn default() -> Self {
        Self::builder().build().expect("default system is valid")
    }
}
impl CameraSystem {
    pub fn builder() -> SystemBuilder {
        SystemBuilder::default()
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Deterministic, timer-driven RGB source for examples and hardware-free tests.
    pub fn synthetic() -> Self {
        Self {
            policy: BackendPolicy::Require(BackendId::SYNTHETIC),
            synthetic: true,
            registered: Arc::new(HashMap::new()),
            backend_options: Arc::new(Vec::new()),
        }
    }

    pub fn backend_policy(&self) -> &BackendPolicy {
        &self.policy
    }

    pub fn available_backends(&self) -> Vec<BackendId> {
        let mut available = backends::available_backends()
            .into_iter()
            .map(BackendId::from)
            .collect::<Vec<_>>();
        available.extend(self.registered.keys().cloned());
        if self.synthetic {
            available.push(BackendId::SYNTHETIC);
        }
        available.sort();
        available.dedup();
        available
    }

    pub fn backend_availability(&self, backend: &BackendId) -> BackendAvailability {
        if (backend == &BackendId::SYNTHETIC && self.synthetic)
            || self.registered.contains_key(backend)
        {
            return BackendAvailability::Available;
        }
        let Some(backend) = backends::backend_type(backend) else {
            return BackendAvailability::NotCompiled;
        };
        if !backend.is_compiled() {
            BackendAvailability::NotCompiled
        } else if !backend.supports_target() {
            BackendAvailability::UnsupportedTarget
        } else {
            BackendAvailability::Available
        }
    }

    pub fn permission_status(&self, backend: &BackendId) -> CameraResult<crate::PermissionStatus> {
        if backend == &BackendId::AV_FOUNDATION {
            #[cfg(all(
                feature = "backend-avfoundation",
                any(target_os = "macos", target_os = "ios")
            ))]
            {
                return Ok(
                    crate::backends::avfoundation::camera::AVFoundationCamera::permission_status(),
                );
            }
            #[cfg(not(all(
                feature = "backend-avfoundation",
                any(target_os = "macos", target_os = "ios")
            )))]
            return Err(CameraError::UnsupportedTarget {
                backend: backend.clone(),
                target: std::env::consts::OS,
            });
        }
        if backend == &BackendId::CAMERA2 || backend == &BackendId::UVC {
            Ok(crate::PermissionStatus::ManagedExternally)
        } else {
            Ok(crate::PermissionStatus::NotRequired)
        }
    }

    /// Explicitly request a permission only where the backend owns a supported
    /// platform prompt. `open` never calls this method implicitly.
    pub async fn request_permission(&self, backend: &BackendId) -> CameraResult<bool> {
        if backend == &BackendId::AV_FOUNDATION {
            #[cfg(all(
                feature = "backend-avfoundation",
                any(target_os = "macos", target_os = "ios")
            ))]
            return crate::backends::avfoundation::camera::AVFoundationCamera::request_permission()
                .await;
            #[cfg(not(all(
                feature = "backend-avfoundation",
                any(target_os = "macos", target_os = "ios")
            )))]
            return Err(CameraError::UnsupportedTarget {
                backend: backend.clone(),
                target: std::env::consts::OS,
            });
        }
        Err(CameraError::PermissionManagedExternally {
            backend: backend.clone(),
        })
    }

    fn policy_backends(&self) -> Vec<BackendId> {
        match &self.policy {
            BackendPolicy::PlatformDefault => {
                vec![BackendId::from(BackendType::default_for_platform())]
            }
            BackendPolicy::Require(backend) => vec![backend.clone()],
            BackendPolicy::Prefer(backends) => backends.clone(),
        }
    }

    fn requiring(&self, backend: BackendId) -> Self {
        Self {
            policy: BackendPolicy::Require(backend.clone()),
            synthetic: self.synthetic,
            registered: self.registered.clone(),
            backend_options: Arc::new(
                self.backend_options
                    .iter()
                    .filter(|options| options.backend() == backend)
                    .cloned()
                    .collect(),
            ),
        }
    }

    fn devices_for_backend(&self, backend: &BackendId) -> CameraResult<Vec<DeviceInfo>> {
        if backend == &BackendId::SYNTHETIC && self.synthetic {
            return Ok(vec![device_info_synthetic()]);
        }
        if let Some(registered) = self.registered.get(backend) {
            return registered
                .provider
                .enumerate()?
                .into_iter()
                .enumerate()
                .map(|(index, info)| {
                    Ok(DeviceInfo {
                        id: DeviceId::new(backend.clone(), info.native_id)?,
                        name: info.name,
                        description: info.description,
                        stability: info.stability,
                        index: index as u32,
                        facing: info.facing,
                        usb: info.usb,
                    })
                })
                .collect();
        }
        let backend_type =
            backends::backend_type(backend).ok_or_else(|| CameraError::BackendNotCompiled {
                backend: backend.clone(),
            })?;
        let backend_type = resolve_backend(backend_type)?;
        backends::list_devices(backend_type).map(|devices| {
            devices
                .into_iter()
                .map(|device| device_info(backend_type, device))
                .collect()
        })
    }

    /// Blocking enumeration for native worker threads and synchronous applications.
    pub fn devices_blocking(&self) -> CameraResult<Vec<DeviceInfo>> {
        let mut devices = Vec::new();
        let mut errors = Vec::new();
        for backend in self.policy_backends() {
            match self.devices_for_backend(&backend) {
                Ok(mut found) => devices.append(&mut found),
                Err(error) => errors.push(error),
            }
        }
        if devices.is_empty() && !errors.is_empty() {
            return Err(errors.remove(0));
        }
        Ok(devices)
    }

    pub fn capabilities_blocking(&self, id: &DeviceId) -> CameraResult<DeviceCapabilities> {
        let devices = self.devices_for_backend(&id.backend)?;
        let matching: Vec<_> = devices.iter().filter(|d| d.id == *id).collect();
        if matching.len() > 1 {
            return Err(CameraError::AmbiguousDevice(id.to_string()));
        }
        let device = matching
            .first()
            .ok_or_else(|| CameraError::DeviceNotFound(id.to_string()))?;
        if id.backend == BackendId::SYNTHETIC {
            return Ok(DeviceCapabilities::single_native(
                CaptureFormat::Rgb8,
                640,
                480,
                FrameRate::default(),
            ));
        }
        if let Some(registered) = self.registered.get(&id.backend) {
            return registered.provider.open(id.native_id())?.capabilities();
        }
        let backend =
            backends::backend_type(&id.backend).ok_or_else(|| CameraError::BackendNotCompiled {
                backend: id.backend.clone(),
            })?;
        backends::get_device_capabilities(backend, device.index)
    }

    pub async fn devices(&self) -> CameraResult<Vec<DeviceInfo>> {
        let system = self.clone();
        tokio::task::spawn_blocking(move || system.devices_blocking())
            .await
            .map_err(join_error)?
    }

    pub async fn capabilities(&self, id: &DeviceId) -> CameraResult<DeviceCapabilities> {
        let system = self.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || system.capabilities_blocking(&id))
            .await
            .map_err(join_error)?
    }

    pub async fn capabilities_timeout(
        &self,
        id: &DeviceId,
        timeout: Duration,
    ) -> CameraResult<DeviceCapabilities> {
        if timeout.is_zero() {
            return Err(CameraError::InvalidConfig(
                "Capability query timeout must be positive".into(),
            ));
        }
        tokio::time::timeout(timeout, self.capabilities(id))
            .await
            .map_err(|_| CameraError::Timeout {
                stage: crate::OperationStage::Capabilities,
            })?
    }

    async fn resolve_selector(&self, selector: DeviceSelector) -> CameraResult<DeviceInfo> {
        let devices = self.devices().await?;
        let mut matches = devices.into_iter().filter(|device| match &selector {
            DeviceSelector::Default => true,
            DeviceSelector::Id(id) => device.id == *id,
            DeviceSelector::Facing(facing) => device.facing == *facing,
            DeviceSelector::Usb {
                vendor_id,
                product_id,
                serial_number,
            } => device.usb.as_ref().is_some_and(|usb| {
                usb.vendor_id == *vendor_id
                    && usb.product_id == *product_id
                    && serial_number
                        .as_ref()
                        .is_none_or(|serial| usb.serial_number.as_ref() == Some(serial))
            }),
            DeviceSelector::Name(name) => device.name.contains(name),
        });
        let selected = matches
            .next()
            .ok_or_else(|| CameraError::DeviceNotFound(format!("{selector:?}")))?;
        if !matches!(selector, DeviceSelector::Default) && matches.next().is_some() {
            return Err(CameraError::AmbiguousDevice(format!("{selector:?}")));
        }
        Ok(selected)
    }

    pub async fn open(&self, selector: DeviceSelector) -> CameraResult<Device> {
        self.open_inner(selector).await
    }

    pub async fn open_timeout(
        &self,
        selector: DeviceSelector,
        timeout: Duration,
    ) -> CameraResult<Device> {
        if timeout.is_zero() {
            return Err(CameraError::InvalidConfig(
                "Device open timeout must be positive".into(),
            ));
        }
        self.open_until(selector, StartupDeadline::new(timeout))
            .await
    }

    async fn open_until(
        &self,
        selector: DeviceSelector,
        deadline: StartupDeadline,
    ) -> CameraResult<Device> {
        deadline
            .run(crate::OperationStage::Open, self.open_inner(selector))
            .await
    }

    async fn open_inner(&self, selector: DeviceSelector) -> CameraResult<Device> {
        let device = self.resolve_selector(selector).await?;
        let source = if device.id.backend == BackendId::SYNTHETIC {
            Source::Synthetic(FrameHub::default())
        } else if let Some(registered) = self.registered.get(&device.id.backend) {
            let provider = registered.provider.clone();
            let native_id = device.id.native.clone();
            let handle = tokio::task::spawn_blocking(move || provider.open(&native_id))
                .await
                .map_err(join_error)??;
            Source::custom(handle)
        } else {
            let backend = backends::backend_type(&device.id.backend).ok_or_else(|| {
                CameraError::BackendNotCompiled {
                    backend: device.id.backend.clone(),
                }
            })?;
            let index = device.index;
            let expected = device.id.native.clone();
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
                Some(device.id.clone()),
            )?
        };
        let hub = source.hub()?;
        Ok(Device {
            device,
            source,
            hub,
            busy: Arc::new(AtomicBool::new(false)),
            backend_options: self.backend_options.clone(),
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
    pub async fn open_usb_fd(&self, fd: std::os::fd::BorrowedFd<'_>) -> CameraResult<Device> {
        use std::os::fd::AsRawFd;
        let owned = fd.try_clone_to_owned()?;
        let backend =
            tokio::task::spawn_blocking(move || backends::create_camera_from_fd(owned.as_raw_fd()))
                .await
                .map_err(join_error)??;
        let source = Source::native(backend, None)?;
        let hub = source.hub()?;
        Ok(Device {
            device: DeviceInfo {
                id: DeviceId {
                    backend: BackendId::UVC,
                    native: "authorized-usb-fd".into(),
                },
                name: "Authorized USB camera".into(),
                description: String::new(),
                stability: IdentityStability::EnumerationOnly,
                index: 0,
                facing: CameraFacing::External,
                usb: None,
            },
            source,
            hub,
            busy: Arc::new(AtomicBool::new(false)),
            backend_options: self.backend_options.clone(),
        })
    }

    pub fn capture(&self, selector: DeviceSelector) -> CaptureBuilder {
        CaptureBuilder::new(self.clone(), selector)
    }

    pub async fn plan(
        &self,
        selector: DeviceSelector,
        request: CaptureRequest,
    ) -> CameraResult<CapturePlan> {
        let device = self.resolve_selector(selector).await?;
        let mut configurations = if device.id.backend == BackendId::SYNTHETIC {
            vec![CameraConfig::new(
                request
                    .preferred_formats
                    .first()
                    .copied()
                    .unwrap_or(CaptureFormat::Rgb8)
                    .into(),
                request.width,
                request.height,
                request.preferred_fps.numerator(),
            )
            .with_frame_rate(request.preferred_fps)]
        } else {
            let capabilities = self.capabilities(&device.id).await?;
            let mut configurations = capabilities.configurations()?;
            if let Some(backend) = backends::backend_type(&device.id.backend) {
                let index = device.index;
                let requested = request.clone();
                let advertised = configurations.clone();
                configurations.extend(
                    tokio::task::spawn_blocking(move || {
                        backends::requested_configs(backend, index, &requested, &advertised)
                    })
                    .await
                    .map_err(join_error)??,
                );
            }
            configurations
        };
        rank_capture_configurations(&request, &mut configurations)?;
        let selected = configurations[0].clone();
        let per_frame = selected
            .frame_size()
            .unwrap_or_else(|| (selected.width as usize * selected.height as usize).max(1));
        let estimated_pool_bytes = per_frame
            .saturating_mul(request.memory.buffers)
            .min(request.memory.bytes);
        Ok(CapturePlan {
            selected: NegotiatedCapture {
                capture: CaptureMode::from_config(&selected)?,
                adjustments: Vec::new(),
                first_frame_latency: std::time::Duration::ZERO,
            },
            alternatives: configurations
                .iter()
                .skip(1)
                .map(CaptureMode::from_config)
                .collect::<CameraResult<Vec<_>>>()?,
            ranking_reason: format!(
                "ranked for {:?}: format preference, required FPS, resolution, then frame rate",
                request.priority
            ),
            estimated_pool_bytes,
            adjustments: Vec::new(),
        })
    }
}
fn device_info_synthetic() -> DeviceInfo {
    DeviceInfo {
        id: DeviceId {
            backend: BackendId::SYNTHETIC,
            native: "test-pattern".into(),
        },
        name: "Synthetic RGB camera".into(),
        description: "Deterministic capture source".into(),
        stability: IdentityStability::Native,
        index: 0,
        facing: CameraFacing::Unknown,
        usb: None,
    }
}
fn device_info(backend: BackendType, d: CameraDeviceInfo) -> DeviceInfo {
    let stable = d.device_path.as_ref().is_some_and(|s| {
        !s.is_empty() && !(backend == BackendType::V4l2 && s.starts_with("/dev/video"))
    }) || d.serial_number.as_ref().is_some_and(|s| !s.is_empty());
    let facing = infer_facing(&d.name, &d.description);
    let usb = d
        .vendor_id
        .zip(d.product_id)
        .map(|(vendor_id, product_id)| UsbIdentity {
            vendor_id,
            product_id,
            serial_number: d.serial_number.clone(),
        });
    DeviceInfo {
        id: DeviceId {
            backend: backend.into(),
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
        facing,
        usb,
    }
}

fn infer_facing(name: &str, description: &str) -> CameraFacing {
    let value = format!("{name} {description}").to_ascii_lowercase();
    if value.contains("front") || value.contains("user") {
        CameraFacing::Front
    } else if value.contains("back") || value.contains("rear") || value.contains("environment") {
        CameraFacing::Back
    } else if value.contains("usb") || value.contains("external") {
        CameraFacing::External
    } else {
        CameraFacing::Unknown
    }
}
fn resolve_backend(b: BackendType) -> CameraResult<BackendType> {
    let b = if b == BackendType::Auto {
        BackendType::default_for_platform()
    } else {
        b
    };
    if !b.is_compiled() {
        Err(CameraError::BackendNotCompiled { backend: b.into() })
    } else if !b.supports_target() {
        Err(CameraError::UnsupportedTarget {
            backend: b.into(),
            target: std::env::consts::OS,
        })
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
    Custom(Arc<CustomSource>),
    Synthetic(FrameHub),
}

struct CustomSource {
    device: parking_lot::Mutex<Box<dyn crate::BackendDevice>>,
    hub: FrameHub,
    actual: parking_lot::Mutex<Option<CameraConfig>>,
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
            .and_then(|id| backends::backend_type(&id.backend))
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
            let _ = this.camera.lock().take();
            let backend = backends::backend_type(&id.backend).ok_or_else(|| {
                CameraError::BackendNotCompiled {
                    backend: id.backend.clone(),
                }
            })?;
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

    fn custom(device: Box<dyn crate::BackendDevice>) -> Self {
        Self::Custom(Arc::new(CustomSource {
            device: parking_lot::Mutex::new(device),
            hub: FrameHub::default(),
            actual: parking_lot::Mutex::new(None),
        }))
    }

    fn hub(&self) -> CameraResult<FrameHub> {
        match self {
            Self::Native(c) => Ok(c.hub.clone()),
            Self::Custom(c) => Ok(c.hub.clone()),
            Self::Synthetic(h) => Ok(h.clone()),
        }
    }
    async fn start(&self, config: CameraConfig) -> CameraResult<()> {
        match self {
            Self::Native(c) => c.start_stream(config).await,
            Self::Custom(c) => {
                let c = c.clone();
                tokio::task::spawn_blocking(move || {
                    let sink = crate::FrameSink::begin(c.hub.clone());
                    let plan = CapturePlan {
                        selected: NegotiatedCapture {
                            capture: CaptureMode::from_config(&config)?,
                            adjustments: Vec::new(),
                            first_frame_latency: Duration::ZERO,
                        },
                        alternatives: Vec::new(),
                        ranking_reason: "selected by the camera negotiation layer".into(),
                        estimated_pool_bytes: 0,
                        adjustments: Vec::new(),
                    };
                    c.device.lock().start(sink, &plan)?;
                    *c.actual.lock() = Some(config);
                    Ok(())
                })
                .await
                .map_err(join_error)?
            }
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
            Self::Custom(c) => {
                let c = c.clone();
                tokio::task::spawn_blocking(move || {
                    c.hub.stop();
                    let result = c.device.lock().stop();
                    *c.actual.lock() = None;
                    result
                })
                .await
                .map_err(join_error)?
            }
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
            Self::Custom(c) => c.actual.lock().clone().ok_or_else(|| {
                CameraError::InvalidState("External backend did not start a capture".into())
            }),
        }
    }

    async fn external_capabilities(&self) -> CameraResult<Option<DeviceCapabilities>> {
        match self {
            Self::Custom(c) => {
                let c = c.clone();
                tokio::task::spawn_blocking(move || c.device.lock().capabilities())
                    .await
                    .map_err(join_error)?
                    .map(Some)
            }
            _ => Ok(None),
        }
    }
}

async fn apply_backend_options(
    source: &Source,
    selected_backend: &BackendId,
    options: &[BackendOptions],
) -> CameraResult<()> {
    for option in options {
        if &option.backend() != selected_backend {
            return Err(CameraError::BackendOptionMismatch {
                option_backend: option.backend(),
                selected_backend: selected_backend.clone(),
            });
        }
        let Source::Native(native) = source else {
            continue;
        };
        match option {
            BackendOptions::V4l2(options) => {
                if native.backend_type() != BackendType::V4l2 {
                    return Err(CameraError::InvalidConfig(
                        "Driver queue configuration is only supported by V4L2".into(),
                    ));
                }
                if let Some(count) = options.mmap_buffers {
                    native.set_buffer_size(count).await?;
                }
            }
            BackendOptions::Camera2(options) => {
                native.current()?.set_camera2_options(options.clone())?;
            }
            BackendOptions::AvFoundation(options) => {
                native
                    .current()?
                    .set_avfoundation_options(options.clone())?;
            }
        }
    }
    Ok(())
}
/// Open device handle. Starting returns the sole owner of capture lifetime.
pub struct Camera {
    device: DeviceInfo,
    source: Source,
    hub: FrameHub,
    busy: Arc<AtomicBool>,
    backend_options: Arc<Vec<BackendOptions>>,
}
pub type Device = Camera;
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
    pub async fn start(&mut self, request: CaptureRequest) -> CameraResult<Session> {
        let deadline = StartupDeadline::new(request.startup_timeout);
        self.start_until(request, deadline).await
    }

    async fn start_until(
        &mut self,
        request: CaptureRequest,
        deadline: StartupDeadline,
    ) -> CameraResult<Session> {
        let mut raw = request.to_stream_request()?;
        for options in self.backend_options.iter() {
            match options {
                BackendOptions::V4l2(options) if self.device.id.backend == BackendId::V4L2 => {
                    raw.driver_buffers = options.mmap_buffers;
                }
                BackendOptions::Camera2(_) if self.device.id.backend == BackendId::CAMERA2 => {}
                BackendOptions::AvFoundation(_)
                    if self.device.id.backend == BackendId::AV_FOUNDATION => {}
                options if options.backend() != self.device.id.backend => {
                    return Err(CameraError::BackendOptionMismatch {
                        option_backend: options.backend(),
                        selected_backend: self.device.id.backend.clone(),
                    });
                }
                _ => {}
            }
        }
        let candidates = if matches!(self.source, Source::Synthetic(_)) {
            vec![CameraConfig::new(
                VideoFormat::RGB,
                request.width,
                request.height,
                request.preferred_fps.numerator(),
            )
            .with_frame_rate(request.preferred_fps)]
        } else if matches!(&self.source, Source::Native(native) if native.id.is_none()) {
            direct_capture_candidates(&request)
        } else {
            let external = deadline
                .run(
                    crate::OperationStage::Startup,
                    self.source.external_capabilities(),
                )
                .await?;
            let mut configurations = if let Some(capabilities) = external {
                capabilities.configurations()?
            } else {
                let backend = backends::backend_type(&self.device.id.backend).ok_or_else(|| {
                    CameraError::BackendNotCompiled {
                        backend: self.device.id.backend.clone(),
                    }
                })?;
                let index = self.device.index;
                let requested = request.clone();
                deadline
                    .run(crate::OperationStage::Startup, async move {
                        tokio::task::spawn_blocking(move || {
                            let capabilities = backends::get_device_capabilities(backend, index)?;
                            let mut configurations = capabilities.configurations()?;
                            let requested_configurations = backends::requested_configs(
                                backend,
                                index,
                                &requested,
                                &configurations,
                            )?;
                            configurations.extend(requested_configurations);
                            Ok(configurations)
                        })
                        .await
                        .map_err(join_error)?
                    })
                    .await?
            };
            rank_capture_configurations(&request, &mut configurations)?;
            configurations
        };
        self.start_raw(raw, candidates, request, deadline).await
    }

    async fn start_raw(
        &mut self,
        request: StreamRequest,
        candidates: Vec<CameraConfig>,
        intent: CaptureRequest,
        deadline: StartupDeadline,
    ) -> CameraResult<CaptureSession> {
        self.start_inner(request, candidates, intent, deadline)
            .await
    }
    async fn start_inner(
        &mut self,
        request: StreamRequest,
        mut candidates: Vec<CameraConfig>,
        intent: CaptureRequest,
        deadline: StartupDeadline,
    ) -> CameraResult<CaptureSession> {
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
        let (events, _) = broadcast::channel(64);
        self.hub.set_event_sender(events.clone());
        let inner = Arc::new(SessionInner {
            device: self.device.clone(),
            source: self.source.clone(),
            hub: self.hub.clone(),
            busy: self.busy.clone(),
            state,
            events,
            stopping: Arc::new(AtomicBool::new(false)),
            closed: AtomicBool::new(false),
            terminal_metrics: parking_lot::Mutex::new(None),
            requested_controls: std::sync::Mutex::new(std::collections::HashMap::new()),
            lifecycle: Mutex::new(()),
            intent: intent.clone(),
            backend_options: self.backend_options.clone(),
        });
        let guard = StartGuard(Some(inner.clone()));
        deadline
            .run(
                crate::OperationStage::Startup,
                apply_backend_options(
                    &inner.source,
                    &inner.device.id.backend,
                    inner.backend_options.as_ref(),
                ),
            )
            .await?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task_inner = inner.clone();
        let task_intent = intent.clone();
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
                    let previous_session = task_inner.hub.session();
                    task_inner.source.start(config.clone()).await?;
                    let session = task_inner.hub.session();
                    if session == previous_session {
                        return Err(CameraError::InvalidState(
                            "Backend did not begin a new frame session".into(),
                        ));
                    }
                    let frame = task_inner
                        .hub
                        .wait_first_in(session, deadline.remaining())
                        .await?;
                    let actual = task_inner.source.actual(&config)?;
                    validate_requested_capture(&task_intent, &actual, frame.layout())?;
                    Ok::<_, CameraError>((actual, frame))
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
                task_inner.set_state(SessionState::Stopped);
                task_inner.busy.store(false, Ordering::Release);
            }
            let _ = tx.send(result);
        });
        let (actual, _first) = tokio::time::timeout_at(deadline.0, rx)
            .await
            .map_err(|_| CameraError::Timeout {
                stage: crate::OperationStage::FirstFrame,
            })?
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
        if request.format.is_some_and(|f| f != actual.format) {
            adjustments.push(format!("Capture format {:?} selected", actual.format));
        }
        inner.set_state(SessionState::Streaming);
        for adjustment in &adjustments {
            let _ = inner
                .events
                .send(SessionEvent::ConfigurationAdjusted(adjustment.clone()));
        }
        let session = CaptureSession {
            inner,
            negotiated: NegotiatedCapture {
                capture: CaptureMode::from_config(&actual)?,
                first_frame_latency: startup_started.elapsed(),
                adjustments,
            },
            actual,
        };
        if let Some(policy) = request.reconnect {
            session.inner.hub.enable_recovery();
            session.spawn_recovery(policy);
        }
        guard.disarm();
        Ok(session)
    }
}

#[derive(Clone, Debug)]
pub struct CaptureBuilder {
    system: CameraSystem,
    selector: DeviceSelector,
    request: CaptureRequest,
}

impl CaptureBuilder {
    fn new(system: CameraSystem, selector: DeviceSelector) -> Self {
        Self {
            system,
            selector,
            request: CaptureRequest::builder()
                .build()
                .expect("default capture request is valid"),
        }
    }

    pub fn profile(mut self, profile: CaptureProfile) -> Self {
        let builder = match profile {
            CaptureProfile::Preview => CaptureRequest::builder()
                .preferred_resolution(1280, 720)
                .priority(crate::NegotiationPriority::LowLatency),
            CaptureProfile::Recognition => CaptureRequest::builder()
                .preferred_resolution(1280, 720)
                .priority(crate::NegotiationPriority::LowCpu),
            CaptureProfile::Recording => CaptureRequest::builder()
                .preferred_resolution(1920, 1080)
                .priority(crate::NegotiationPriority::Fidelity),
            CaptureProfile::NativeRelay => CaptureRequest::builder()
                .preferred_resolution(1280, 720)
                .priority(crate::NegotiationPriority::LowBandwidth)
                .preferred_formats([CaptureFormat::Mjpeg, CaptureFormat::H264]),
        };
        self.request = builder
            .build()
            .expect("built-in capture profiles are valid");
        self
    }

    pub fn resolution(mut self, width: u32, height: u32) -> Self {
        self.request.width = width;
        self.request.height = height;
        self
    }

    pub fn frame_rate(mut self, frame_rate: FrameRate) -> Self {
        self.request.preferred_fps = frame_rate;
        self
    }

    pub fn request(mut self, request: CaptureRequest) -> Self {
        self.request = request;
        self
    }

    pub async fn start(self) -> CameraResult<Capture> {
        self.request.to_stream_request()?;
        let deadline = StartupDeadline::new(self.request.startup_timeout);
        if matches!(self.selector, DeviceSelector::Default) {
            if let BackendPolicy::Prefer(backends) = self.system.backend_policy() {
                let mut attempts = Vec::new();
                for backend in backends.clone() {
                    let system = self.system.requiring(backend.clone());
                    let attempt = async {
                        let mut device =
                            system.open_until(DeviceSelector::Default, deadline).await?;
                        let session = device.start_until(self.request.clone(), deadline).await?;
                        let receiver = session.subscribe(SubscriptionOptions::latest())?;
                        Ok(Capture { session, receiver })
                    }
                    .await;
                    match attempt {
                        Ok(capture) => return Ok(capture),
                        Err(error @ CameraError::PermissionDenied(_))
                        | Err(error @ CameraError::PermissionRequired(_)) => return Err(error),
                        Err(error) => attempts.push(crate::BackendAttempt {
                            backend,
                            error: error.to_string(),
                        }),
                    }
                }
                return Err(CameraError::BackendAttemptsFailed(attempts));
            }
        }
        let mut device = self.system.open_until(self.selector, deadline).await?;
        let session = device.start_until(self.request, deadline).await?;
        let receiver = session.subscribe(SubscriptionOptions::latest())?;
        Ok(Capture { session, receiver })
    }
}

#[derive(Debug)]
pub struct Capture {
    session: Session,
    receiver: FrameReceiver,
}

impl Capture {
    pub async fn next_frame(&mut self) -> CameraResult<Frame> {
        self.receiver.next().await
    }

    pub fn negotiated(&self) -> &NegotiatedCapture {
        self.session.negotiated()
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub async fn close(&self) -> CameraResult<()> {
        self.session.close().await
    }
}

fn validate_requested_capture(
    request: &CaptureRequest,
    actual: &CameraConfig,
    first_layout: &crate::FrameLayout,
) -> CameraResult<()> {
    actual.validate()?;
    let frame_rate = actual.frame_rate()?;
    if request
        .minimum_fps
        .is_some_and(|minimum| frame_rate < minimum)
    {
        return Err(CameraError::UnsupportedFormat(format!(
            "Driver negotiated {} fps below required {} fps",
            frame_rate.as_f64(),
            request.minimum_fps.unwrap().as_f64()
        )));
    }
    if !request.preferred_formats.is_empty()
        && !request
            .preferred_formats
            .iter()
            .any(|format| VideoFormat::from(*format) == actual.format)
    {
        return Err(CameraError::UnsupportedFormat(format!(
            "Driver negotiated unaccepted format {:?}",
            actual.format
        )));
    }
    if request.exact_resolution && (actual.width, actual.height) != (request.width, request.height)
    {
        return Err(CameraError::UnsupportedFormat(format!(
            "Driver negotiated {}x{} instead of the required {}x{} resolution",
            actual.width, actual.height, request.width, request.height
        )));
    }
    if request
        .required_aspect_ratio
        .is_some_and(|(width, height)| {
            u64::from(actual.width) * u64::from(height)
                != u64::from(actual.height) * u64::from(width)
        })
    {
        return Err(CameraError::UnsupportedFormat(format!(
            "Driver negotiated {}x{} outside the required aspect ratio",
            actual.width, actual.height
        )));
    }
    if first_layout.width != actual.width || first_layout.height != actual.height {
        return Err(CameraError::InvalidFormat(
            "First frame differs from negotiated dimensions".into(),
        ));
    }
    Ok(())
}

fn rank_capture_configurations(
    request: &CaptureRequest,
    configurations: &mut Vec<CameraConfig>,
) -> CameraResult<()> {
    configurations.retain(|config| {
        config.validate().is_ok()
            && (request.preferred_formats.is_empty()
                || request
                    .preferred_formats
                    .iter()
                    .any(|format| VideoFormat::from(*format) == config.format))
            && request
                .minimum_fps
                .is_none_or(|minimum| config.frame_rate().is_ok_and(|rate| rate >= minimum))
            && (!request.exact_resolution
                || (config.width, config.height) == (request.width, request.height))
            && request.required_aspect_ratio.is_none_or(|(width, height)| {
                u64::from(config.width) * u64::from(height)
                    == u64::from(config.height) * u64::from(width)
            })
    });
    let preference = |format: VideoFormat| {
        request
            .preferred_formats
            .iter()
            .position(|preferred| VideoFormat::from(*preferred) == format)
            .unwrap_or(request.preferred_formats.len())
    };
    let cost = |format: VideoFormat| match request.priority {
        crate::NegotiationPriority::LowCpu => match format {
            VideoFormat::RGB | VideoFormat::NV12 | VideoFormat::YUYV | VideoFormat::UYVY => 0,
            VideoFormat::Gray => 1,
            VideoFormat::MJPEG => 2,
            VideoFormat::H264 => 3,
        },
        crate::NegotiationPriority::LowBandwidth => match format {
            VideoFormat::H264 => 0,
            VideoFormat::MJPEG => 1,
            VideoFormat::NV12 | VideoFormat::Gray => 2,
            _ => 3,
        },
        crate::NegotiationPriority::LowLatency => match format {
            VideoFormat::NV12 | VideoFormat::YUYV | VideoFormat::UYVY | VideoFormat::RGB => 0,
            VideoFormat::Gray => 1,
            _ => 2,
        },
        crate::NegotiationPriority::Fidelity => 0,
    };
    configurations.sort_by_key(|config| {
        let resolution_delta = (i64::from(config.width) - i64::from(request.width)).unsigned_abs()
            + (i64::from(config.height) - i64::from(request.height)).unsigned_abs();
        let frame_rate_delta = config.frame_rate().map_or(u64::MAX, |rate| {
            ((rate.as_f64() - request.preferred_fps.as_f64()).abs() * 1_000_000.0) as u64
        });
        (
            preference(config.format),
            cost(config.format),
            resolution_delta,
            frame_rate_delta,
        )
    });
    configurations.dedup();
    if configurations.is_empty() {
        return Err(CameraError::UnsupportedFormat(
            "No capture mode satisfies the required constraints".into(),
        ));
    }
    Ok(())
}

fn direct_capture_candidates(request: &CaptureRequest) -> Vec<CameraConfig> {
    let formats = if request.preferred_formats.is_empty() {
        vec![CaptureFormat::Mjpeg]
    } else {
        request.preferred_formats.clone()
    };
    formats
        .into_iter()
        .map(|format| {
            CameraConfig::new(
                format.into(),
                request.width,
                request.height,
                request.preferred_fps.numerator(),
            )
            .with_frame_rate(request.preferred_fps)
        })
        .collect()
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
impl SessionState {
    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Lifecycle(SessionState),
    ConfigurationAdjusted(String),
    ResourcePressure { dropped_frames: u64 },
    Missed { count: u64 },
}

impl SessionEvent {
    pub fn is_lifecycle(&self) -> bool {
        matches!(self, Self::Lifecycle(_))
    }
}

#[derive(Debug)]
pub struct StateWatcher {
    receiver: watch::Receiver<SessionState>,
}

impl StateWatcher {
    pub fn current(&self) -> SessionState {
        self.receiver.borrow().clone()
    }

    pub async fn changed(&mut self) -> CameraResult<SessionState> {
        self.receiver
            .changed()
            .await
            .map_err(|_| CameraError::StreamStopped)?;
        Ok(self.current())
    }
}

#[derive(Debug)]
pub struct EventReceiver {
    receiver: broadcast::Receiver<SessionEvent>,
}

impl EventReceiver {
    pub async fn next(&mut self) -> CameraResult<SessionEvent> {
        match self.receiver.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(count)) => Ok(SessionEvent::Missed { count }),
            Err(broadcast::error::RecvError::Closed) => Err(CameraError::StreamStopped),
        }
    }
}
struct SessionInner {
    device: DeviceInfo,
    source: Source,
    hub: FrameHub,
    busy: Arc<AtomicBool>,
    state: watch::Sender<SessionState>,
    events: broadcast::Sender<SessionEvent>,
    stopping: Arc<AtomicBool>,
    closed: AtomicBool,
    terminal_metrics: parking_lot::Mutex<Option<(FrameMetrics, StreamStats)>>,
    requested_controls:
        std::sync::Mutex<std::collections::HashMap<crate::ControlId, crate::ControlValue>>,
    lifecycle: Mutex<()>,
    intent: CaptureRequest,
    backend_options: Arc<Vec<BackendOptions>>,
}
impl SessionInner {
    fn set_state(&self, state: SessionState) {
        self.state.send_replace(state.clone());
        let _ = self.events.send(SessionEvent::Lifecycle(state));
    }

    fn request_stop(self: &Arc<Self>) {
        if self.stopping.swap(true, Ordering::AcqRel) || self.closed.load(Ordering::Acquire) {
            return;
        }
        self.set_state(SessionState::Stopping);
        self.hub.disable_recovery();
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
        self.hub.disable_recovery();
        self.hub.end_recovery();
        self.hub.stop();
        let result = self.source.stop().await;
        *self.terminal_metrics.lock() = Some((self.hub.metrics(), self.hub.stats()));
        self.closed.store(true, Ordering::Release);
        self.set_state(match &result {
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
/// Owning stream session. Drop requests stop immediately; close().await also
/// waits for native shutdown. Receivers never own permission to stop capture.
pub struct CaptureSession {
    inner: Arc<SessionInner>,
    negotiated: NegotiatedCapture,
    actual: CameraConfig,
}
pub type Session = CaptureSession;
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
    pub fn negotiated(&self) -> &NegotiatedCapture {
        &self.negotiated
    }
    pub fn subscribe(&self, options: SubscriptionOptions) -> CameraResult<FrameReceiver> {
        Ok(self
            .inner
            .hub
            .subscribe_with(options)?
            .with_stop_flag(self.inner.stopping.clone()))
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
    pub fn watch_state(&self) -> StateWatcher {
        StateWatcher {
            receiver: self.inner.state.subscribe(),
        }
    }
    pub fn events(&self) -> EventReceiver {
        EventReceiver {
            receiver: self.inner.events.subscribe(),
        }
    }
    pub async fn close(&self) -> CameraResult<()> {
        self.inner.request_stop();
        self.inner.stop().await
    }
    fn spawn_recovery(&self, policy: ReconnectPolicy) {
        let weak = Arc::downgrade(&self.inner);
        let config = self.actual.clone();
        tokio::spawn(async move {
            let mut attempt = 0;
            let mut pending_trigger = None;
            loop {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let retrying = pending_trigger.is_some();
                let trigger = if let Some(trigger) = pending_trigger {
                    tokio::time::sleep(recovery_retry_delay(policy.delay, attempt)).await;
                    trigger
                } else {
                    inner
                        .hub
                        .wait_for_recovery_trigger(policy.stall_timeout)
                        .await
                };
                if inner.stopping.load(Ordering::Acquire) {
                    break;
                }
                let _guard = inner.lifecycle.lock().await;
                if inner.stopping.load(Ordering::Acquire) {
                    break;
                }
                if !retrying && inner.hub.recovery_trigger(policy.stall_timeout).is_none() {
                    continue;
                }
                attempt += 1;
                inner.set_state(SessionState::Recovering { attempt });
                inner.hub.begin_recovery();
                let force_reopen =
                    attempt > 1 || matches!(trigger, crate::frame::RecoveryTrigger::SourceStopped);
                let result = async {
                    let stop_result = inner.source.stop().await;
                    let reopen = force_reopen
                        || matches!((&inner.source, &stop_result), (Source::Native(_), Err(_)));
                    if let Source::Native(native) = &inner.source {
                        if reopen {
                            native.reopen().await?;
                            apply_backend_options(
                                &inner.source,
                                &inner.device.id.backend,
                                inner.backend_options.as_ref(),
                            )
                            .await?;
                            inner.requested_controls.lock().unwrap().clear();
                        } else {
                            stop_result?;
                        }
                    } else {
                        stop_result?;
                    }
                    if inner.stopping.load(Ordering::Acquire) {
                        return Err(CameraError::StreamStopped);
                    }
                    inner.source.start(config.clone()).await?;
                    let session = inner.hub.session();
                    let first_frame_timeout = if reopen {
                        policy.stall_timeout
                    } else {
                        fast_restart_first_frame_timeout(&config, policy.stall_timeout)?
                    };
                    let first = inner
                        .hub
                        .wait_first_in(session, first_frame_timeout)
                        .await?;
                    let actual = inner.source.actual(&config)?;
                    validate_recovered_capture(&config, &actual, first.layout())?;
                    validate_requested_capture(&inner.intent, &actual, first.layout())?;
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
                        inner.set_state(SessionState::Streaming);
                        attempt = 0;
                        pending_trigger = None;
                    }
                    Err(e) => {
                        // A failed first-frame/configuration check must not leave
                        // a live source preventing the next recovery attempt.
                        let _ = inner.source.stop().await;
                        inner.hub.stop();
                        pending_trigger = Some(trigger);
                        if policy.max_attempts.is_some_and(|n| attempt >= n) {
                            inner.set_state(SessionState::Failed(e.to_string()));
                            inner.stopping.store(true, Ordering::Release);
                            inner.hub.disable_recovery();
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

fn recovery_retry_delay(configured: Duration, failed_attempts: u32) -> Duration {
    let fast = match failed_attempts {
        0 | 1 => Duration::from_millis(100),
        2 => Duration::from_millis(200),
        3 => Duration::from_millis(400),
        _ => configured,
    };
    configured.min(fast)
}

fn fast_restart_first_frame_timeout(
    config: &CameraConfig,
    recovery_timeout: Duration,
) -> CameraResult<Duration> {
    let three_frames = config.frame_rate()?.interval().saturating_mul(3);
    Ok(recovery_timeout.min(Duration::from_millis(750).max(three_frames)))
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

    #[test]
    fn descriptor_candidates_preserve_all_format_preferences() {
        let request = CaptureRequest::builder()
            .exact_resolution(800, 600)
            .preferred_frame_rate(FrameRate::new(30000, 1001).unwrap())
            .preferred_formats([CaptureFormat::Mjpeg, CaptureFormat::Yuyv])
            .build()
            .unwrap();
        let candidates = direct_capture_candidates(&request);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].format, VideoFormat::MJPEG);
        assert_eq!(candidates[1].format, VideoFormat::YUYV);
        assert!(candidates.iter().all(|candidate| {
            candidate.width == 800
                && candidate.height == 600
                && candidate.frame_rate().unwrap() == FrameRate::new(30000, 1001).unwrap()
        }));
    }

    #[test]
    fn reconnect_retries_are_fast_first_and_then_respect_the_policy_delay() {
        let configured = Duration::from_secs(1);
        assert_eq!(
            recovery_retry_delay(configured, 1),
            Duration::from_millis(100)
        );
        assert_eq!(
            recovery_retry_delay(configured, 2),
            Duration::from_millis(200)
        );
        assert_eq!(
            recovery_retry_delay(configured, 3),
            Duration::from_millis(400)
        );
        assert_eq!(recovery_retry_delay(configured, 4), configured);
        assert_eq!(
            recovery_retry_delay(Duration::from_millis(3), 3),
            Duration::from_millis(3)
        );
    }

    #[test]
    fn soft_restart_first_frame_deadline_tracks_slow_frame_rates() {
        assert_eq!(
            fast_restart_first_frame_timeout(
                &CameraConfig::new(VideoFormat::RGB, 8, 8, 30),
                Duration::from_secs(5),
            )
            .unwrap(),
            Duration::from_millis(750)
        );
        assert_eq!(
            fast_restart_first_frame_timeout(
                &CameraConfig::new(VideoFormat::RGB, 8, 8, 2),
                Duration::from_secs(5),
            )
            .unwrap(),
            Duration::from_millis(1500)
        );
    }
}
