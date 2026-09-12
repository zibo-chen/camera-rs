//! Public, backend-neutral capture contracts.
use crate::{
    CameraConfig, CameraError, CameraResult, DeliveryPolicy, FrameRate, MemoryBudget,
    OverflowPolicy, PixelFormat, VideoFormat,
};
use std::{borrow::Cow, fmt, str::FromStr, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
/// Stable identifier for a built-in or application-provided camera backend.
pub struct BackendId(Cow<'static, str>);

impl BackendId {
    /// USB Video Class backend backed by bundled libuvc/libusb.
    pub const UVC: Self = Self::builtin("uvc");
    /// Linux Video4Linux2 backend.
    pub const V4L2: Self = Self::builtin("v4l2");
    /// Apple AVFoundation backend.
    pub const AV_FOUNDATION: Self = Self::builtin("avfoundation");
    /// Android NDK Camera2 backend.
    pub const CAMERA2: Self = Self::builtin("camera2");
    /// Windows Media Foundation backend.
    pub const MEDIA_FOUNDATION: Self = Self::builtin("media-foundation");
    /// Deterministic in-process backend intended for examples and tests.
    pub const SYNTHETIC: Self = Self::builtin("synthetic");

    const fn builtin(value: &'static str) -> Self {
        // All callers are associated constants backed by string literals.
        Self(Cow::Borrowed(value))
    }

    /// Creates a namespaced identifier for an application-provided backend.
    ///
    /// Each component must be a non-empty ASCII identifier of at most 96 bytes.
    pub fn custom(namespace: &str, name: &str) -> CameraResult<Self> {
        fn valid(part: &str) -> bool {
            !part.is_empty()
                && part.len() <= 96
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        }
        if !valid(namespace) || !valid(name) {
            return Err(CameraError::invalid_config(
                "Backend namespace and name must be non-empty ASCII identifiers".into(),
            ));
        }
        Ok(Self(Cow::Owned(format!("{namespace}/{name}"))))
    }

    /// Returns the stable serialized backend identifier.
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
                    CameraError::invalid_config("Invalid backend identifier".into())
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
/// Policy used to select which compiled backend may satisfy an operation.
pub enum BackendPolicy {
    #[default]
    /// Use the platform's preferred native backend without unrelated fallback.
    PlatformDefault,
    /// Require exactly the specified backend.
    Require(BackendId),
    /// Try the listed backends in order and retain each typed failure.
    Prefer(Vec<BackendId>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Compile-time and target support status for a backend.
pub enum BackendAvailability {
    /// The backend is compiled and supports the current target.
    Available,
    /// The corresponding Cargo feature was not enabled.
    NotCompiled,
    /// The backend feature is enabled but cannot run on this target.
    UnsupportedTarget,
}

#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Possible values for `OperationStage`.
pub enum OperationStage {
    /// The `Enumeration` variant.
    Enumeration,
    /// The `Open` variant.
    Open,
    /// The `Capabilities` variant.
    Capabilities,
    /// The `Startup` variant.
    Startup,
    /// The `FirstFrame` variant.
    FirstFrame,
    /// The `FrameWait` variant.
    FrameWait,
    /// The `BackendCommand` variant.
    BackendCommand,
    /// The `Worker` variant.
    Worker,
    /// The `Close` variant.
    Close,
}

impl OperationStage {
    /// Stable machine-readable operation stage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enumeration => "enumeration",
            Self::Open => "open",
            Self::Capabilities => "capabilities",
            Self::Startup => "startup",
            Self::FirstFrame => "first_frame",
            Self::FrameWait => "frame_wait",
            Self::BackendCommand => "backend_command",
            Self::Worker => "worker",
            Self::Close => "close",
        }
    }
}

impl fmt::Display for OperationStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for OperationStage {
    type Err = CameraError;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        match code {
            "enumeration" => Ok(Self::Enumeration),
            "open" => Ok(Self::Open),
            "capabilities" => Ok(Self::Capabilities),
            "startup" => Ok(Self::Startup),
            "first_frame" => Ok(Self::FirstFrame),
            "frame_wait" => Ok(Self::FrameWait),
            "backend_command" => Ok(Self::BackendCommand),
            "worker" => Ok(Self::Worker),
            "close" => Ok(Self::Close),
            _ => Err(CameraError::invalid_argument(
                "camera operation stage",
                code.to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Current camera authorization state as reported by the selected platform.
pub enum PermissionStatus {
    /// The `NotDetermined` variant.
    NotDetermined,
    /// The `Restricted` variant.
    Restricted,
    /// The `Denied` variant.
    Denied,
    /// The `Authorized` variant.
    Authorized,
    /// The `ManagedExternally` variant.
    ManagedExternally,
    /// The `NotRequired` variant.
    NotRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One typed failure captured while evaluating a fallback backend policy.
pub struct BackendAttempt {
    backend: BackendId,
    error: std::sync::Arc<CameraError>,
}

impl BackendAttempt {
    /// Creates an attempt record from its backend and original shared error.
    pub fn new(backend: BackendId, error: std::sync::Arc<CameraError>) -> Self {
        Self { backend, error }
    }

    /// Returns the backend that produced the failure.
    pub fn backend(&self) -> &BackendId {
        &self.backend
    }

    /// Returns the original structured camera error.
    pub fn error(&self) -> &CameraError {
        &self.error
    }

    pub(crate) fn shared_error(&self) -> &std::sync::Arc<CameraError> {
        &self.error
    }
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
    /// Performs the `new` operation.
    pub fn new(backend: BackendId, native_id: impl Into<String>) -> CameraResult<Self> {
        let native = native_id.into();
        if native.is_empty() || native.len() > 4096 || native.contains('\0') {
            return Err(CameraError::invalid_config(
                "Device native identifier is empty or invalid".into(),
            ));
        }
        Ok(Self { backend, native })
    }

    /// Performs the `backend` operation.
    pub fn backend(&self) -> &BackendId {
        &self.backend
    }

    /// Performs the `native_id` operation.
    pub fn native_id(&self) -> &str {
        &self.native
    }

    /// Performs the `to_persistent_string` operation.
    pub fn to_persistent_string(&self) -> String {
        let encoded = self
            .native
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("{}|{encoded}", self.backend)
    }

    /// Performs the `parse` operation.
    pub fn parse(value: &str) -> CameraResult<Self> {
        let (backend, encoded) = value
            .split_once('|')
            .ok_or_else(|| CameraError::invalid_config("Invalid persisted device ID".into()))?;
        if encoded.len() > 4096 * 2
            || encoded.len() % 2 != 0
            || !encoded.as_bytes().iter().all(u8::is_ascii_hexdigit)
        {
            return Err(CameraError::invalid_config(
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
                CameraError::invalid_config("Invalid persisted device ID payload".into())
            })?;
        let native = String::from_utf8(bytes)
            .map_err(|_| CameraError::invalid_config("Device ID payload is not UTF-8".into()))?;
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
/// Possible values for `IdentityStability`.
pub enum IdentityStability {
    /// The `Native` variant.
    Native,
    /// The `EnumerationOnly` variant.
    EnumerationOnly,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Possible values for `CameraFacing`.
pub enum CameraFacing {
    /// The `Front` variant.
    Front,
    /// The `Back` variant.
    Back,
    /// The `External` variant.
    External,
    #[default]
    /// The `Unknown` variant.
    Unknown,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Values for `UsbIdentity`.
pub struct UsbIdentity {
    /// USB vendor identifier.
    pub vendor_id: u16,
    /// USB product identifier.
    pub product_id: u16,
    /// USB serial number when available.
    pub serial_number: Option<String>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
/// Values for `DeviceInfo`.
pub struct DeviceInfo {
    /// The id value.
    pub id: DeviceId,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Persistence guarantee for the identifier.
    pub stability: IdentityStability,
    /// Enumeration index for the current snapshot.
    pub index: u32,
    /// Physical camera facing.
    pub facing: CameraFacing,
    /// USB identity when available.
    pub usb: Option<UsbIdentity>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
/// Possible values for `DeviceSelector`.
pub enum DeviceSelector {
    /// The `Default` variant.
    Default,
    /// The `Id` variant.
    Id(DeviceId),
    /// The `Facing` variant.
    Facing(CameraFacing),
    /// The `Usb` variant.
    Usb {
        /// The `vendor_id` struct field.
        vendor_id: u16,
        /// The `product_id` struct field.
        product_id: u16,
        /// The `serial_number` struct field.
        serial_number: Option<String>,
    },
    /// The `Name` variant.
    Name(String),
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Possible values for `CaptureFormat`.
pub enum CaptureFormat {
    /// The `Mjpeg` variant.
    Mjpeg,
    /// The `Yuyv` variant.
    Yuyv,
    /// The `Uyvy` variant.
    Uyvy,
    /// The `Nv12` variant.
    Nv12,
    /// The `Rgb8` variant.
    Rgb8,
    /// The `H264` variant.
    H264,
    /// The `Gray8` variant.
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
                return Err(CameraError::unsupported_format(format!(
                    "{value:?} has no capture-format equivalent"
                )))
            }
        })
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Possible values for `NegotiationPriority`.
pub enum NegotiationPriority {
    /// The `LowLatency` variant.
    LowLatency,
    #[default]
    /// The `LowCpu` variant.
    LowCpu,
    /// The `LowBandwidth` variant.
    LowBandwidth,
    /// The `Fidelity` variant.
    Fidelity,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Possible values for `CaptureProfile`.
pub enum CaptureProfile {
    #[default]
    /// The `Preview` variant.
    Preview,
    /// The `Recognition` variant.
    Recognition,
    /// The `Recording` variant.
    Recording,
    /// The `NativeRelay` variant.
    NativeRelay,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
/// Values for `CaptureRequest`.
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
    /// Performs the `builder` operation.
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

    /// Performs the `preferred_resolution` operation.
    pub fn preferred_resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Performs the `preferred_frame_rate` operation.
    pub fn preferred_frame_rate(&self) -> FrameRate {
        self.preferred_fps
    }

    /// Performs the `minimum_frame_rate` operation.
    pub fn minimum_frame_rate(&self) -> Option<FrameRate> {
        self.minimum_fps
    }

    /// Performs the `preferred_formats` operation.
    pub fn preferred_formats(&self) -> &[CaptureFormat] {
        &self.preferred_formats
    }

    /// Performs the `priority` operation.
    pub fn priority(&self) -> NegotiationPriority {
        self.priority
    }

    /// Performs the `required_aspect_ratio` operation.
    pub fn required_aspect_ratio(&self) -> Option<(u32, u32)> {
        self.required_aspect_ratio
    }

    /// The `fn` constant.
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
/// Values for `CaptureRequestBuilder`.
pub struct CaptureRequestBuilder(CaptureRequest);

impl CaptureRequestBuilder {
    /// Performs the `preferred_resolution` operation.
    pub fn preferred_resolution(mut self, width: u32, height: u32) -> Self {
        self.0.width = width;
        self.0.height = height;
        self
    }

    /// Performs the `exact_resolution` operation.
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

    /// Performs the `preferred_frame_rate` operation.
    pub fn preferred_frame_rate(mut self, rate: FrameRate) -> Self {
        self.0.preferred_fps = rate;
        self
    }

    /// Performs the `minimum_frame_rate` operation.
    pub fn minimum_frame_rate(mut self, rate: FrameRate) -> Self {
        self.0.minimum_fps = Some(rate);
        self
    }

    /// Performs the `preferred_formats` operation.
    pub fn preferred_formats(mut self, formats: impl IntoIterator<Item = CaptureFormat>) -> Self {
        self.0.preferred_formats = formats.into_iter().collect();
        self
    }

    /// Performs the `priority` operation.
    pub fn priority(mut self, priority: NegotiationPriority) -> Self {
        self.0.priority = priority;
        self
    }

    /// Performs the `memory_budget` operation.
    pub fn memory_budget(mut self, budget: MemoryBudget) -> Self {
        self.0.memory = budget;
        self
    }

    /// Performs the `startup_timeout` operation.
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.0.startup_timeout = timeout;
        self
    }

    /// Performs the `reconnect` operation.
    pub fn reconnect(mut self, policy: crate::ReconnectPolicy) -> Self {
        self.0.reconnect = Some(policy);
        self
    }

    /// Performs the `build` operation.
    pub fn build(self) -> CameraResult<CaptureRequest> {
        let request = self.0;
        request.to_stream_request()?;
        if request
            .minimum_fps
            .is_some_and(|minimum| minimum > request.preferred_fps)
        {
            return Err(CameraError::invalid_config(
                "Minimum frame rate exceeds preferred frame rate".into(),
            ));
        }
        if request
            .required_aspect_ratio
            .is_some_and(|(width, height)| width == 0 || height == 0)
        {
            return Err(CameraError::invalid_config(
                "Required aspect ratio must be positive".into(),
            ));
        }
        if request.preferred_formats.len() > 1 {
            let mut dedup = request.preferred_formats.clone();
            dedup.sort_by_key(|format| *format as u8);
            dedup.dedup();
            if dedup.len() != request.preferred_formats.len() {
                return Err(CameraError::invalid_config(
                    "Preferred capture formats contain duplicates".into(),
                ));
            }
        }
        Ok(request)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Values for `CaptureMode`.
pub struct CaptureMode {
    /// Pixel or capture format.
    pub format: CaptureFormat,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Capture frame rate.
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
/// Possible values for `CapabilityKnowledge`.
pub enum CapabilityKnowledge<T> {
    /// The `Known` variant.
    Known(T),
    /// A bounded sample of a backend capability space that also contains
    /// continuous or stepwise ranges.
    Representative {
        /// The `values` struct field.
        values: T,
        /// The `reason` struct field.
        reason: String,
    },
    /// The `Unknown` variant.
    Unknown {
        /// The `reason` struct field.
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Possible values for `CapabilityRangeKind`.
pub enum CapabilityRangeKind {
    /// The `Discrete` variant.
    Discrete,
    /// The `Continuous` variant.
    Continuous,
    /// The `Stepwise` variant.
    Stepwise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Values for `DimensionRange`.
pub struct DimensionRange {
    /// The minimum value.
    pub minimum: u32,
    /// The maximum value.
    pub maximum: u32,
    /// The step value.
    pub step: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Values for `FrameInterval`.
pub struct FrameInterval {
    /// The numerator value.
    pub numerator: u32,
    /// The denominator value.
    pub denominator: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Values for `FrameIntervalRange`.
pub struct FrameIntervalRange {
    /// The kind value.
    pub kind: CapabilityRangeKind,
    /// The minimum value.
    pub minimum: FrameInterval,
    /// The maximum value.
    pub maximum: FrameInterval,
    /// The step value.
    pub step: Option<FrameInterval>,
    /// Resolution at which the backend reported this interval range.
    pub at_resolution: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Values for `CaptureModeRange`.
pub struct CaptureModeRange {
    /// Pixel or capture format.
    pub format: CaptureFormat,
    /// The kind value.
    pub kind: CapabilityRangeKind,
    /// Frame width in pixels.
    pub width: DimensionRange,
    /// Frame height in pixels.
    pub height: DimensionRange,
    /// The frame intervals value.
    pub frame_intervals: Vec<FrameIntervalRange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Values for `ConversionCapability`.
pub struct ConversionCapability {
    /// The input value.
    pub input: CaptureFormat,
    /// The output value.
    pub output: PixelFormat,
    /// Whether this conversion is compiled and available.
    pub available: bool,
    /// The requirement value.
    pub requirement: Option<&'static str>,
}

#[derive(Clone, Debug)]
/// Values for `DeviceCapabilities`.
pub struct DeviceCapabilities {
    /// The capture modes value.
    pub capture_modes: CapabilityKnowledge<Vec<CaptureMode>>,
    /// Native size and/or frame-interval range descriptors. `capture_modes` is
    /// representative when this list is non-empty.
    pub capture_mode_ranges: Vec<CaptureModeRange>,
    /// The native formats value.
    pub native_formats: Vec<CaptureFormat>,
    /// The conversions value.
    pub conversions: Vec<ConversionCapability>,
    /// The limitations value.
    pub limitations: Vec<String>,
}

impl DeviceCapabilities {
    /// Performs the `single_native` operation.
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
            return Err(CameraError::invalid_config(
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
            return Err(CameraError::invalid_config(
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
            CapabilityKnowledge::Unknown { reason } => Err(CameraError::unsupported_format(
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
/// Values for `NegotiatedCapture`.
pub struct NegotiatedCapture {
    /// Negotiated native capture mode.
    pub capture: CaptureMode,
    /// Adjustments made during negotiation.
    pub adjustments: Vec<String>,
    /// The first frame latency value.
    pub first_frame_latency: Duration,
}

#[derive(Clone, Debug)]
/// Values for `CapturePlan`.
pub struct CapturePlan {
    /// The selected value.
    pub selected: NegotiatedCapture,
    /// Other compatible capture modes.
    pub alternatives: Vec<CaptureMode>,
    /// Human-readable explanation of the selected mode.
    pub ranking_reason: String,
    /// Estimated bytes reserved by the frame pool.
    pub estimated_pool_bytes: usize,
    /// Adjustments made during negotiation.
    pub adjustments: Vec<String>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Values for `SubscriptionOptions`.
pub struct SubscriptionOptions {
    pub(crate) delivery: DeliveryPolicy,
    pub(crate) max_rate: Option<FrameRate>,
}

impl SubscriptionOptions {
    /// Performs the `latest` operation.
    pub fn latest() -> Self {
        Self {
            delivery: DeliveryPolicy::Latest,
            max_rate: None,
        }
    }

    /// Performs the `buffered` operation.
    pub fn buffered(capacity: usize) -> Self {
        Self {
            delivery: DeliveryPolicy::Buffered {
                capacity,
                overflow: OverflowPolicy::DropOldest,
            },
            max_rate: None,
        }
    }

    /// Performs the `overflow` operation.
    pub fn overflow(mut self, overflow: OverflowPolicy) -> Self {
        if let DeliveryPolicy::Buffered {
            overflow: current, ..
        } = &mut self.delivery
        {
            *current = overflow;
        }
        self
    }

    /// Performs the `max_rate` operation.
    pub fn max_rate(mut self, rate: FrameRate) -> Self {
        self.max_rate = Some(rate);
        self
    }

    #[allow(dead_code)]
    pub(crate) fn validate(self, budget: MemoryBudget) -> CameraResult<()> {
        if let DeliveryPolicy::Buffered { capacity, .. } = self.delivery {
            if capacity == 0 || capacity >= budget.buffers {
                return Err(CameraError::invalid_config(
                    "Subscription queue must be positive and smaller than the frame pool".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Values for `V4l2Options`.
pub struct V4l2Options {
    /// The mmap buffers value.
    pub mmap_buffers: Option<usize>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Possible values for `Camera2RequestTemplate`.
pub enum Camera2RequestTemplate {
    #[default]
    /// The `Preview` variant.
    Preview,
    /// The `StillCapture` variant.
    StillCapture,
    /// The `Record` variant.
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
/// Values for `Camera2Options`.
pub struct Camera2Options {
    /// The max images value.
    pub max_images: Option<u32>,
    /// The request template value.
    pub request_template: Option<Camera2RequestTemplate>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Possible values for `LateFramePolicy`.
pub enum LateFramePolicy {
    #[default]
    /// The `Drop` variant.
    Drop,
    /// The `Deliver` variant.
    Deliver,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Values for `AvFoundationOptions`.
pub struct AvFoundationOptions {
    /// The late frames value.
    pub late_frames: LateFramePolicy,
}

impl AvFoundationOptions {
    /// Performs the `late_frames` operation.
    pub fn late_frames(mut self, policy: LateFramePolicy) -> Self {
        self.late_frames = policy;
        self
    }
}

impl Camera2Options {
    /// Performs the `max_images` operation.
    pub fn max_images(mut self, count: u32) -> Self {
        self.max_images = Some(count);
        self
    }

    /// Performs the `request_template` operation.
    pub fn request_template(mut self, template: Camera2RequestTemplate) -> Self {
        self.request_template = Some(template);
        self
    }
}

impl V4l2Options {
    /// Performs the `mmap_buffers` operation.
    pub fn mmap_buffers(mut self, count: usize) -> Self {
        self.mmap_buffers = Some(count);
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Possible values for `BackendOptions`.
pub enum BackendOptions {
    /// The `V4l2` variant.
    V4l2(V4l2Options),
    /// The `Camera2` variant.
    Camera2(Camera2Options),
    /// The `AvFoundation` variant.
    AvFoundation(AvFoundationOptions),
}

impl BackendOptions {
    /// Performs the `backend` operation.
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
                Err(CameraError::invalid_config(
                    "V4L2 MMAP buffer count must be 2..=32".into(),
                ))
            }
            Self::Camera2(options)
                if options.max_images.is_some_and(|n| !(2..=16).contains(&n)) =>
            {
                Err(CameraError::invalid_config(
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
