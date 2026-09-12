//! Public, backend-neutral capture contracts.
use crate::{
    CameraConfig, CameraError, CameraResult, DeliveryPolicy, FrameRate, MemoryBudget,
    OverflowPolicy, PixelFormat, VideoFormat,
};
use std::{borrow::Cow, fmt, str::FromStr, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BackendId(Cow<'static, str>);

impl BackendId {
    pub const UVC: Self = Self::builtin("uvc");
    pub const V4L2: Self = Self::builtin("v4l2");
    pub const AV_FOUNDATION: Self = Self::builtin("avfoundation");
    pub const CAMERA2: Self = Self::builtin("camera2");
    pub const MEDIA_FOUNDATION: Self = Self::builtin("media-foundation");
    pub const SYNTHETIC: Self = Self::builtin("synthetic");

    const fn builtin(value: &'static str) -> Self {
        // All callers are associated constants backed by string literals.
        Self(Cow::Borrowed(value))
    }

    pub fn custom(namespace: &str, name: &str) -> CameraResult<Self> {
        fn valid(part: &str) -> bool {
            !part.is_empty()
                && part.len() <= 96
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        }
        if !valid(namespace) || !valid(name) {
            return Err(CameraError::InvalidConfig(
                "Backend namespace and name must be non-empty ASCII identifiers".into(),
            ));
        }
        Ok(Self(Cow::Owned(format!("{namespace}/{name}"))))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[allow(dead_code)]
    pub(crate) fn from_builtin(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BackendId {
    type Err = CameraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "uvc" => Ok(Self::UVC),
            "v4l2" => Ok(Self::V4L2),
            "avfoundation" => Ok(Self::AV_FOUNDATION),
            "camera2" => Ok(Self::CAMERA2),
            "media-foundation" => Ok(Self::MEDIA_FOUNDATION),
            "synthetic" => Ok(Self::SYNTHETIC),
            _ => {
                let (namespace, name) = value.split_once('/').ok_or_else(|| {
                    CameraError::InvalidConfig("Invalid backend identifier".into())
                })?;
                Self::custom(namespace, name)
            }
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for BackendId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for BackendId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BackendPolicy {
    #[default]
    PlatformDefault,
    Require(BackendId),
    Prefer(Vec<BackendId>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendAvailability {
    Available,
    NotCompiled,
    UnsupportedTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationStage {
    Enumeration,
    Open,
    Capabilities,
    Startup,
    FirstFrame,
    FrameWait,
    BackendCommand,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionStatus {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    ManagedExternally,
    NotRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendAttempt {
    pub backend: BackendId,
    pub error: String,
}

/// Backend-scoped device identity. Persistence preserves the backend namespace;
/// `IdentityStability` still tells callers whether the native part is stable
/// across enumeration cycles or process restarts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub(crate) backend: BackendId,
    pub(crate) native: String,
}

impl DeviceId {
    pub fn new(backend: BackendId, native_id: impl Into<String>) -> CameraResult<Self> {
        let native = native_id.into();
        if native.is_empty() || native.len() > 4096 || native.contains('\0') {
            return Err(CameraError::InvalidConfig(
                "Device native identifier is empty or invalid".into(),
            ));
        }
        Ok(Self { backend, native })
    }

    pub fn backend(&self) -> &BackendId {
        &self.backend
    }

    pub fn native_id(&self) -> &str {
        &self.native
    }

    pub fn to_persistent_string(&self) -> String {
        let encoded = self
            .native
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("{}|{encoded}", self.backend)
    }

    pub fn parse(value: &str) -> CameraResult<Self> {
        let (backend, encoded) = value
            .split_once('|')
            .ok_or_else(|| CameraError::InvalidConfig("Invalid persisted device ID".into()))?;
        if encoded.len() > 4096 * 2
            || encoded.len() % 2 != 0
            || !encoded.as_bytes().iter().all(u8::is_ascii_hexdigit)
        {
            return Err(CameraError::InvalidConfig(
                "Invalid persisted device ID payload".into(),
            ));
        }
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let bytes = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    b'A'..=b'F' => byte - b'A' + 10,
                    _ => unreachable!("ASCII hexadecimal was validated above"),
                };
                Ok((digit(pair[0]) << 4) | digit(pair[1]))
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_: std::convert::Infallible| {
                CameraError::InvalidConfig("Invalid persisted device ID payload".into())
            })?;
        let native = String::from_utf8(bytes)
            .map_err(|_| CameraError::InvalidConfig("Device ID payload is not UTF-8".into()))?;
        Self::new(backend.parse()?, native)
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.backend, self.native)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for DeviceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_persistent_string())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for DeviceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityStability {
    Native,
    EnumerationOnly,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CameraFacing {
    Front,
    Back,
    External,
    #[default]
    Unknown,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub description: String,
    pub stability: IdentityStability,
    pub index: u32,
    pub facing: CameraFacing,
    pub usb: Option<UsbIdentity>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub enum DeviceSelector {
    Default,
    Id(DeviceId),
    Facing(CameraFacing),
    Usb {
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<String>,
    },
    Name(String),
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptureFormat {
    Mjpeg,
    Yuyv,
    Uyvy,
    Nv12,
    Rgb8,
    H264,
    Gray8,
}

impl From<CaptureFormat> for VideoFormat {
    fn from(value: CaptureFormat) -> Self {
        match value {
            CaptureFormat::Mjpeg => Self::MJPEG,
            CaptureFormat::Yuyv => Self::YUYV,
            CaptureFormat::Uyvy => Self::UYVY,
            CaptureFormat::Nv12 => Self::NV12,
            CaptureFormat::Rgb8 => Self::RGB,
            CaptureFormat::H264 => Self::H264,
            CaptureFormat::Gray8 => Self::Gray,
        }
    }
}

impl From<VideoFormat> for CaptureFormat {
    fn from(value: VideoFormat) -> Self {
        match value {
            VideoFormat::MJPEG => Self::Mjpeg,
            VideoFormat::YUYV => Self::Yuyv,
            VideoFormat::UYVY => Self::Uyvy,
            VideoFormat::NV12 => Self::Nv12,
            VideoFormat::RGB => Self::Rgb8,
            VideoFormat::H264 => Self::H264,
            VideoFormat::Gray => Self::Gray8,
        }
    }
}

impl TryFrom<PixelFormat> for CaptureFormat {
    type Error = CameraError;

    fn try_from(value: PixelFormat) -> Result<Self, Self::Error> {
        Ok(match value {
            PixelFormat::Mjpeg => Self::Mjpeg,
            PixelFormat::Yuyv => Self::Yuyv,
            PixelFormat::Uyvy => Self::Uyvy,
            PixelFormat::Nv12 => Self::Nv12,
            PixelFormat::Rgb8 => Self::Rgb8,
            PixelFormat::H264 => Self::H264,
            PixelFormat::Gray8 => Self::Gray8,
            _ => {
                return Err(CameraError::UnsupportedFormat(format!(
                    "{value:?} has no capture-format equivalent"
                )))
            }
        })
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NegotiationPriority {
    LowLatency,
    #[default]
    LowCpu,
    LowBandwidth,
    Fidelity,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureProfile {
    #[default]
    Preview,
    Recognition,
    Recording,
    NativeRelay,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) preferred_fps: FrameRate,
    pub(crate) minimum_fps: Option<FrameRate>,
    pub(crate) preferred_formats: Vec<CaptureFormat>,
    pub(crate) priority: NegotiationPriority,
    pub(crate) exact_resolution: bool,
    pub(crate) required_aspect_ratio: Option<(u32, u32)>,
    pub(crate) memory: MemoryBudget,
    pub(crate) startup_timeout: Duration,
    pub(crate) reconnect: Option<crate::ReconnectPolicy>,
}

impl CaptureRequest {
    pub fn builder() -> CaptureRequestBuilder {
        CaptureRequestBuilder(Self {
            width: 640,
            height: 480,
            preferred_fps: FrameRate::default(),
            minimum_fps: None,
            preferred_formats: Vec::new(),
            priority: NegotiationPriority::default(),
            exact_resolution: false,
            required_aspect_ratio: None,
            memory: MemoryBudget::default(),
            startup_timeout: Duration::from_secs(3),
            reconnect: None,
        })
    }

    pub fn preferred_resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn preferred_frame_rate(&self) -> FrameRate {
        self.preferred_fps
    }

    pub fn minimum_frame_rate(&self) -> Option<FrameRate> {
        self.minimum_fps
    }

    pub fn preferred_formats(&self) -> &[CaptureFormat] {
        &self.preferred_formats
    }

    pub fn priority(&self) -> NegotiationPriority {
        self.priority
    }

    pub fn required_aspect_ratio(&self) -> Option<(u32, u32)> {
        self.required_aspect_ratio
    }

    pub const fn delivers_native_frames(&self) -> bool {
        true
    }

    pub(crate) fn to_stream_request(&self) -> CameraResult<crate::StreamRequest> {
        let mut builder = crate::StreamRequest::builder()
            .resolution(self.width, self.height)
            .frame_rate(self.preferred_fps)
            .output(crate::OutputFormat::Native)
            .selection(crate::SelectionPolicy::Closest)
            .memory_budget(self.memory)
            .startup_timeout(self.startup_timeout);
        if let Some(format) = self.preferred_formats.first().copied() {
            builder = builder.capture_format(format.into());
        }
        if let Some(policy) = self.reconnect.clone() {
            builder = builder.reconnect(policy);
        }
        builder.build()
    }
}

#[derive(Clone, Debug)]
pub struct CaptureRequestBuilder(CaptureRequest);

impl CaptureRequestBuilder {
    pub fn preferred_resolution(mut self, width: u32, height: u32) -> Self {
        self.0.width = width;
        self.0.height = height;
        self
    }

    pub fn exact_resolution(mut self, width: u32, height: u32) -> Self {
        self.0.width = width;
        self.0.height = height;
        self.0.exact_resolution = true;
        self
    }

    /// Require an exact display aspect ratio while still allowing the device to
    /// negotiate a different resolution with that ratio.
    pub fn require_aspect_ratio(mut self, width: u32, height: u32) -> Self {
        self.0.required_aspect_ratio = Some((width, height));
        self
    }

    pub fn preferred_frame_rate(mut self, rate: FrameRate) -> Self {
        self.0.preferred_fps = rate;
        self
    }

    pub fn minimum_frame_rate(mut self, rate: FrameRate) -> Self {
        self.0.minimum_fps = Some(rate);
        self
    }

    pub fn preferred_formats(mut self, formats: impl IntoIterator<Item = CaptureFormat>) -> Self {
        self.0.preferred_formats = formats.into_iter().collect();
        self
    }

    pub fn priority(mut self, priority: NegotiationPriority) -> Self {
        self.0.priority = priority;
        self
    }

    pub fn memory_budget(mut self, budget: MemoryBudget) -> Self {
        self.0.memory = budget;
        self
    }

    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.0.startup_timeout = timeout;
        self
    }

    pub fn reconnect(mut self, policy: crate::ReconnectPolicy) -> Self {
        self.0.reconnect = Some(policy);
        self
    }

    pub fn build(self) -> CameraResult<CaptureRequest> {
        let request = self.0;
        request.to_stream_request()?;
        if request
            .minimum_fps
            .is_some_and(|minimum| minimum > request.preferred_fps)
        {
            return Err(CameraError::InvalidConfig(
                "Minimum frame rate exceeds preferred frame rate".into(),
            ));
        }
        if request
            .required_aspect_ratio
            .is_some_and(|(width, height)| width == 0 || height == 0)
        {
            return Err(CameraError::InvalidConfig(
                "Required aspect ratio must be positive".into(),
            ));
        }
        if request.preferred_formats.len() > 1 {
            let mut dedup = request.preferred_formats.clone();
            dedup.sort_by_key(|format| *format as u8);
            dedup.dedup();
            if dedup.len() != request.preferred_formats.len() {
                return Err(CameraError::InvalidConfig(
                    "Preferred capture formats contain duplicates".into(),
                ));
            }
        }
        Ok(request)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureMode {
    pub format: CaptureFormat,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
}

impl CaptureMode {
    #[allow(dead_code)]
    pub(crate) fn from_config(config: &CameraConfig) -> CameraResult<Self> {
        Ok(Self {
            format: config.format.into(),
            width: config.width,
            height: config.height,
            frame_rate: config.frame_rate()?,
        })
    }

    pub(crate) fn to_config(&self) -> CameraResult<CameraConfig> {
        let config = CameraConfig::new(
            self.format.into(),
            self.width,
            self.height,
            self.frame_rate.numerator(),
        )
        .with_frame_rate(self.frame_rate);
        config.validate()?;
        Ok(config)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityKnowledge<T> {
    Known(T),
    /// A bounded sample of a backend capability space that also contains
    /// continuous or stepwise ranges.
    Representative {
        values: T,
        reason: String,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityRangeKind {
    Discrete,
    Continuous,
    Stepwise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DimensionRange {
    pub minimum: u32,
    pub maximum: u32,
    pub step: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInterval {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameIntervalRange {
    pub kind: CapabilityRangeKind,
    pub minimum: FrameInterval,
    pub maximum: FrameInterval,
    pub step: Option<FrameInterval>,
    /// Resolution at which the backend reported this interval range.
    pub at_resolution: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureModeRange {
    pub format: CaptureFormat,
    pub kind: CapabilityRangeKind,
    pub width: DimensionRange,
    pub height: DimensionRange,
    pub frame_intervals: Vec<FrameIntervalRange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversionCapability {
    pub input: CaptureFormat,
    pub output: PixelFormat,
    pub available: bool,
    pub requirement: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub struct DeviceCapabilities {
    pub capture_modes: CapabilityKnowledge<Vec<CaptureMode>>,
    /// Native size and/or frame-interval range descriptors. `capture_modes` is
    /// representative when this list is non-empty.
    pub capture_mode_ranges: Vec<CaptureModeRange>,
    pub native_formats: Vec<CaptureFormat>,
    pub conversions: Vec<ConversionCapability>,
    pub limitations: Vec<String>,
}

impl DeviceCapabilities {
    pub fn single_native(
        format: CaptureFormat,
        width: u32,
        height: u32,
        frame_rate: FrameRate,
    ) -> Self {
        Self::from_modes_unchecked(vec![CaptureMode {
            format,
            width,
            height,
            frame_rate,
        }])
    }

    /// Build a capability description whose public capture modes are also the
    /// single source used by planning and capture negotiation.
    pub fn from_modes(modes: impl IntoIterator<Item = CaptureMode>) -> CameraResult<Self> {
        let modes = modes.into_iter().collect::<Vec<_>>();
        if modes.is_empty() {
            return Err(CameraError::InvalidConfig(
                "Device capabilities require at least one capture mode".into(),
            ));
        }
        let mut configurations = modes
            .iter()
            .map(CaptureMode::to_config)
            .collect::<CameraResult<Vec<_>>>()?;
        configurations.sort_by_key(|config| {
            (
                config.format as u8,
                config.width,
                config.height,
                config.fps,
                config.fps_denominator,
            )
        });
        configurations.dedup();
        if configurations.len() != modes.len() {
            return Err(CameraError::InvalidConfig(
                "Device capabilities contain duplicate capture modes".into(),
            ));
        }
        Ok(Self::from_modes_unchecked(modes))
    }

    #[allow(dead_code)]
    pub(crate) fn from_configurations(configurations: Vec<CameraConfig>) -> Self {
        let modes = configurations
            .iter()
            .filter_map(|config| {
                Some(CaptureMode {
                    format: config.format.into(),
                    width: config.width,
                    height: config.height,
                    frame_rate: config.frame_rate().ok()?,
                })
            })
            .collect::<Vec<_>>();
        Self::from_modes_unchecked(modes)
    }

    fn from_modes_unchecked(modes: Vec<CaptureMode>) -> Self {
        let mut native_formats = modes.iter().map(|mode| mode.format).collect::<Vec<_>>();
        native_formats.sort_by_key(|format| *format as u8);
        native_formats.dedup();
        let conversions = native_formats
            .iter()
            .copied()
            .map(|input| {
                let jpeg = input == CaptureFormat::Mjpeg;
                let h264 = input == CaptureFormat::H264;
                let available = !h264
                    && cfg!(feature = "convert-rgb")
                    && (!jpeg || cfg!(feature = "decode-mjpeg"));
                ConversionCapability {
                    input,
                    output: PixelFormat::Rgb8,
                    available,
                    requirement: if h264 {
                        Some("external-h264-decoder")
                    } else if jpeg && !cfg!(feature = "decode-mjpeg") {
                        Some("decode-mjpeg")
                    } else if !cfg!(feature = "convert-rgb") {
                        Some("convert-rgb")
                    } else {
                        None
                    },
                }
            })
            .collect();
        Self {
            capture_modes: CapabilityKnowledge::Known(modes),
            capture_mode_ranges: Vec::new(),
            native_formats,
            conversions,
            limitations: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn configurations(&self) -> CameraResult<Vec<CameraConfig>> {
        match &self.capture_modes {
            CapabilityKnowledge::Known(modes)
            | CapabilityKnowledge::Representative { values: modes, .. } => {
                modes.iter().map(CaptureMode::to_config).collect()
            }
            CapabilityKnowledge::Unknown { reason } => Err(CameraError::UnsupportedFormat(
                format!("Capture modes are unknown: {reason}"),
            )),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn with_ranges(mut self, ranges: Vec<CaptureModeRange>) -> Self {
        if !ranges.is_empty() {
            if let CapabilityKnowledge::Known(modes) = self.capture_modes {
                self.capture_modes = CapabilityKnowledge::Representative {
                    values: modes,
                    reason:
                        "backend advertises ranged modes; listed modes are representative samples"
                            .into(),
                };
            }
            self.capture_mode_ranges = ranges;
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct NegotiatedCapture {
    pub capture: CaptureMode,
    pub adjustments: Vec<String>,
    pub first_frame_latency: Duration,
}

#[derive(Clone, Debug)]
pub struct CapturePlan {
    pub selected: NegotiatedCapture,
    pub alternatives: Vec<CaptureMode>,
    pub ranking_reason: String,
    pub estimated_pool_bytes: usize,
    pub adjustments: Vec<String>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionOptions {
    pub(crate) delivery: DeliveryPolicy,
    pub(crate) max_rate: Option<FrameRate>,
}

impl SubscriptionOptions {
    pub fn latest() -> Self {
        Self {
            delivery: DeliveryPolicy::Latest,
            max_rate: None,
        }
    }

    pub fn buffered(capacity: usize) -> Self {
        Self {
            delivery: DeliveryPolicy::Buffered {
                capacity,
                overflow: OverflowPolicy::DropOldest,
            },
            max_rate: None,
        }
    }

    pub fn overflow(mut self, overflow: OverflowPolicy) -> Self {
        if let DeliveryPolicy::Buffered {
            overflow: current, ..
        } = &mut self.delivery
        {
            *current = overflow;
        }
        self
    }

    pub fn max_rate(mut self, rate: FrameRate) -> Self {
        self.max_rate = Some(rate);
        self
    }

    #[allow(dead_code)]
    pub(crate) fn validate(self, budget: MemoryBudget) -> CameraResult<()> {
        if let DeliveryPolicy::Buffered { capacity, .. } = self.delivery {
            if capacity == 0 || capacity >= budget.buffers {
                return Err(CameraError::InvalidConfig(
                    "Subscription queue must be positive and smaller than the frame pool".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V4l2Options {
    pub mmap_buffers: Option<usize>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Camera2RequestTemplate {
    #[default]
    Preview,
    StillCapture,
    Record,
}

impl Camera2RequestTemplate {
    #[cfg(target_os = "android")]
    pub(crate) const fn native_value(self) -> i32 {
        match self {
            Self::Preview => 1,
            Self::StillCapture => 2,
            Self::Record => 3,
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Camera2Options {
    pub max_images: Option<u32>,
    pub request_template: Option<Camera2RequestTemplate>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LateFramePolicy {
    #[default]
    Drop,
    Deliver,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvFoundationOptions {
    pub late_frames: LateFramePolicy,
}

impl AvFoundationOptions {
    pub fn late_frames(mut self, policy: LateFramePolicy) -> Self {
        self.late_frames = policy;
        self
    }
}

impl Camera2Options {
    pub fn max_images(mut self, count: u32) -> Self {
        self.max_images = Some(count);
        self
    }

    pub fn request_template(mut self, template: Camera2RequestTemplate) -> Self {
        self.request_template = Some(template);
        self
    }
}

impl V4l2Options {
    pub fn mmap_buffers(mut self, count: usize) -> Self {
        self.mmap_buffers = Some(count);
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendOptions {
    V4l2(V4l2Options),
    Camera2(Camera2Options),
    AvFoundation(AvFoundationOptions),
}

impl BackendOptions {
    pub fn backend(&self) -> BackendId {
        match self {
            Self::V4l2(_) => BackendId::V4L2,
            Self::Camera2(_) => BackendId::CAMERA2,
            Self::AvFoundation(_) => BackendId::AV_FOUNDATION,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn validate(&self) -> CameraResult<()> {
        match self {
            Self::V4l2(options) if options.mmap_buffers.is_some_and(|n| !(2..=32).contains(&n)) => {
                Err(CameraError::InvalidConfig(
                    "V4L2 MMAP buffer count must be 2..=32".into(),
                ))
            }
            Self::Camera2(options)
                if options.max_images.is_some_and(|n| !(2..=16).contains(&n)) =>
            {
                Err(CameraError::InvalidConfig(
                    "Camera2 max_images must be 2..=16".into(),
                ))
            }
            Self::AvFoundation(_) => Ok(()),
            _ => Ok(()),
        }
    }
}

impl From<V4l2Options> for BackendOptions {
    fn from(value: V4l2Options) -> Self {
        Self::V4l2(value)
    }
}

impl From<Camera2Options> for BackendOptions {
    fn from(value: Camera2Options) -> Self {
        Self::Camera2(value)
    }
}

impl From<AvFoundationOptions> for BackendOptions {
    fn from(value: AvFoundationOptions) -> Self {
        Self::AvFoundation(value)
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn ranged_capabilities_mark_discrete_modes_as_representative() {
        let capabilities = DeviceCapabilities::single_native(
            CaptureFormat::Yuyv,
            640,
            480,
            FrameRate::new(30, 1).unwrap(),
        )
        .with_ranges(vec![CaptureModeRange {
            format: CaptureFormat::Yuyv,
            kind: CapabilityRangeKind::Stepwise,
            width: DimensionRange {
                minimum: 320,
                maximum: 1920,
                step: 16,
            },
            height: DimensionRange {
                minimum: 240,
                maximum: 1080,
                step: 16,
            },
            frame_intervals: vec![FrameIntervalRange {
                kind: CapabilityRangeKind::Continuous,
                minimum: FrameInterval {
                    numerator: 1,
                    denominator: 60,
                },
                maximum: FrameInterval {
                    numerator: 1,
                    denominator: 15,
                },
                step: None,
                at_resolution: (640, 480),
            }],
        }]);

        assert!(matches!(
            capabilities.capture_modes,
            CapabilityKnowledge::Representative { .. }
        ));
        assert_eq!(capabilities.capture_mode_ranges.len(), 1);
        assert_eq!(capabilities.configurations().unwrap().len(), 1);
    }
}
