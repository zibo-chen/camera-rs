//! Stable, backend-neutral error contract.

use crate::{BackendAttempt, BackendId, DeviceId, OperationStage};
use std::{error::Error as StdError, fmt, str::FromStr};
use thiserror::Error;

/// Stable semantic category intended for caller-side branching.
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CameraErrorKind {
    /// The `InvalidArgument` variant.
    InvalidArgument,
    /// The `BackendNotCompiled` variant.
    BackendNotCompiled,
    /// The `UnsupportedTarget` variant.
    UnsupportedTarget,
    /// The `BackendOptionMismatch` variant.
    BackendOptionMismatch,
    /// The `BackendFailure` variant.
    BackendFailure,
    /// The `MultipleBackendsFailed` variant.
    MultipleBackendsFailed,
    /// The `DeviceNotFound` variant.
    DeviceNotFound,
    /// The `DeviceBusy` variant.
    DeviceBusy,
    /// The `DeviceOpenFailed` variant.
    DeviceOpenFailed,
    /// The `DeviceDisconnected` variant.
    DeviceDisconnected,
    /// The `AmbiguousDevice` variant.
    AmbiguousDevice,
    /// The `PermissionRequired` variant.
    PermissionRequired,
    /// The `PermissionDenied` variant.
    PermissionDenied,
    /// The `PermissionManagedExternally` variant.
    PermissionManagedExternally,
    /// The `InvalidState` variant.
    InvalidState,
    /// The `StreamStopped` variant.
    StreamStopped,
    /// The `StreamFailure` variant.
    StreamFailure,
    /// The `Timeout` variant.
    Timeout,
    /// The `BufferExhausted` variant.
    BufferExhausted,
    /// The `InvalidFrame` variant.
    InvalidFrame,
    /// The `UnsupportedFormat` variant.
    UnsupportedFormat,
    /// The `ControlNotSupported` variant.
    ControlNotSupported,
    /// The `ControlNotReadable` variant.
    ControlNotReadable,
    /// The `ControlFailure` variant.
    ControlFailure,
    /// The `Io` variant.
    Io,
    /// The `WorkerFailure` variant.
    WorkerFailure,
    /// The `Internal` variant.
    Internal,
}

impl CameraErrorKind {
    /// Stable machine-readable code for logs, FFI boundaries, and API responses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::BackendNotCompiled => "backend_not_compiled",
            Self::UnsupportedTarget => "unsupported_target",
            Self::BackendOptionMismatch => "backend_option_mismatch",
            Self::BackendFailure => "backend_failure",
            Self::MultipleBackendsFailed => "multiple_backends_failed",
            Self::DeviceNotFound => "device_not_found",
            Self::DeviceBusy => "device_busy",
            Self::DeviceOpenFailed => "device_open_failed",
            Self::DeviceDisconnected => "device_disconnected",
            Self::AmbiguousDevice => "ambiguous_device",
            Self::PermissionRequired => "permission_required",
            Self::PermissionDenied => "permission_denied",
            Self::PermissionManagedExternally => "permission_managed_externally",
            Self::InvalidState => "invalid_state",
            Self::StreamStopped => "stream_stopped",
            Self::StreamFailure => "stream_failure",
            Self::Timeout => "timeout",
            Self::BufferExhausted => "buffer_exhausted",
            Self::InvalidFrame => "invalid_frame",
            Self::UnsupportedFormat => "unsupported_format",
            Self::ControlNotSupported => "control_not_supported",
            Self::ControlNotReadable => "control_not_readable",
            Self::ControlFailure => "control_failure",
            Self::Io => "io",
            Self::WorkerFailure => "worker_failure",
            Self::Internal => "internal",
        }
    }
}

impl FromStr for CameraErrorKind {
    type Err = CameraError;

    fn from_str(code: &str) -> Result<Self> {
        match code {
            "invalid_argument" => Ok(Self::InvalidArgument),
            "backend_not_compiled" => Ok(Self::BackendNotCompiled),
            "unsupported_target" => Ok(Self::UnsupportedTarget),
            "backend_option_mismatch" => Ok(Self::BackendOptionMismatch),
            "backend_failure" => Ok(Self::BackendFailure),
            "multiple_backends_failed" => Ok(Self::MultipleBackendsFailed),
            "device_not_found" => Ok(Self::DeviceNotFound),
            "device_busy" => Ok(Self::DeviceBusy),
            "device_open_failed" => Ok(Self::DeviceOpenFailed),
            "device_disconnected" => Ok(Self::DeviceDisconnected),
            "ambiguous_device" => Ok(Self::AmbiguousDevice),
            "permission_required" => Ok(Self::PermissionRequired),
            "permission_denied" => Ok(Self::PermissionDenied),
            "permission_managed_externally" => Ok(Self::PermissionManagedExternally),
            "invalid_state" => Ok(Self::InvalidState),
            "stream_stopped" => Ok(Self::StreamStopped),
            "stream_failure" => Ok(Self::StreamFailure),
            "timeout" => Ok(Self::Timeout),
            "buffer_exhausted" => Ok(Self::BufferExhausted),
            "invalid_frame" => Ok(Self::InvalidFrame),
            "unsupported_format" => Ok(Self::UnsupportedFormat),
            "control_not_supported" => Ok(Self::ControlNotSupported),
            "control_not_readable" => Ok(Self::ControlNotReadable),
            "control_failure" => Ok(Self::ControlFailure),
            "io" => Ok(Self::Io),
            "worker_failure" => Ok(Self::WorkerFailure),
            "internal" => Ok(Self::Internal),
            _ => Err(CameraError::invalid_argument(
                "camera error code",
                code.to_owned(),
            )),
        }
    }
}

/// Conservative action a caller can take without inspecting backend text.
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecoveryHint {
    /// The `None` variant.
    None,
    /// The `Retry` variant.
    Retry,
    /// The `RequestPermission` variant.
    RequestPermission,
    /// The `ReenumerateDevice` variant.
    ReenumerateDevice,
    /// The `ChangeConfiguration` variant.
    ChangeConfiguration,
    /// The `EnableBackend` variant.
    EnableBackend,
    /// The `DropFrame` variant.
    DropFrame,
    /// The `ReduceResourceUsage` variant.
    ReduceResourceUsage,
}

impl RecoveryHint {
    /// Stable machine-readable recovery action.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Retry => "retry",
            Self::RequestPermission => "request_permission",
            Self::ReenumerateDevice => "reenumerate_device",
            Self::ChangeConfiguration => "change_configuration",
            Self::EnableBackend => "enable_backend",
            Self::DropFrame => "drop_frame",
            Self::ReduceResourceUsage => "reduce_resource_usage",
        }
    }

    /// The `fn` constant.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Retry | Self::ReenumerateDevice)
    }
}

impl FromStr for RecoveryHint {
    type Err = CameraError;

    fn from_str(code: &str) -> Result<Self> {
        match code {
            "none" => Ok(Self::None),
            "retry" => Ok(Self::Retry),
            "request_permission" => Ok(Self::RequestPermission),
            "reenumerate_device" => Ok(Self::ReenumerateDevice),
            "change_configuration" => Ok(Self::ChangeConfiguration),
            "enable_backend" => Ok(Self::EnableBackend),
            "drop_frame" => Ok(Self::DropFrame),
            "reduce_resource_usage" => Ok(Self::ReduceResourceUsage),
            _ => Err(CameraError::invalid_argument(
                "camera recovery code",
                code.to_owned(),
            )),
        }
    }
}

/// One public error type returned by every camera operation.
///
/// The representation is a struct so new diagnostics do not force downstream
/// users to update exhaustive enum matches. Branch on [`Self::kind`] instead.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct CameraError {
    kind: CameraErrorKind,
    recovery_hint: RecoveryHint,
    message: String,
    #[source]
    source: Option<SharedSource>,
    details: Box<CameraErrorDetails>,
}

#[derive(Clone, Debug)]
struct CameraErrorDetails {
    backend: Option<BackendId>,
    device: Option<DeviceId>,
    stage: Option<OperationStage>,
    operation: Option<String>,
    native_code: Option<i64>,
    backend_attempts: Vec<BackendAttempt>,
    causes: Vec<std::sync::Arc<CameraError>>,
}

impl PartialEq for CameraError {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.recovery_hint == other.recovery_hint
            && self.message == other.message
            && self.details.backend == other.details.backend
            && self.details.device == other.details.device
            && self.details.stage == other.details.stage
            && self.details.operation == other.details.operation
            && self.details.native_code == other.details.native_code
            && self.details.backend_attempts == other.details.backend_attempts
            && self.details.causes == other.details.causes
    }
}
impl Eq for CameraError {}

impl CameraError {
    /// Performs the `new` operation.
    pub fn new(kind: CameraErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            recovery_hint: default_recovery_hint(kind),
            message: message.into(),
            source: None,
            details: Box::new(CameraErrorDetails {
                backend: None,
                device: None,
                stage: None,
                operation: None,
                native_code: None,
                backend_attempts: Vec::new(),
                causes: Vec::new(),
            }),
        }
    }

    /// Performs the `kind` operation.
    pub fn kind(&self) -> CameraErrorKind {
        self.kind
    }
    /// Performs the `code` operation.
    pub fn code(&self) -> &'static str {
        self.kind.as_str()
    }
    /// Performs the `recovery_hint` operation.
    pub fn recovery_hint(&self) -> RecoveryHint {
        self.recovery_hint
    }
    /// Performs the `is_retryable` operation.
    pub fn is_retryable(&self) -> bool {
        self.recovery_hint.is_retryable()
    }
    /// Performs the `backend` operation.
    pub fn backend(&self) -> Option<&BackendId> {
        self.details.backend.as_ref()
    }
    /// Performs the `device` operation.
    pub fn device(&self) -> Option<&DeviceId> {
        self.details.device.as_ref()
    }
    /// Performs the `stage` operation.
    pub fn stage(&self) -> Option<OperationStage> {
        self.details.stage
    }
    /// Performs the `operation` operation.
    pub fn operation(&self) -> Option<&str> {
        self.details.operation.as_deref()
    }
    /// Performs the `native_code` operation.
    pub fn native_code(&self) -> Option<i64> {
        self.details.native_code
    }
    /// Performs the `io_error` operation.
    pub fn io_error(&self) -> Option<&std::io::Error> {
        self.source_error()?.downcast_ref::<std::io::Error>()
    }
    /// Performs the `source_error` operation.
    pub fn source_error(&self) -> Option<&(dyn StdError + Send + Sync + 'static)> {
        self.source.as_ref().map(SharedSource::get)
    }
    /// Performs the `backend_attempts` operation.
    pub fn backend_attempts(&self) -> Option<&[BackendAttempt]> {
        (!self.details.backend_attempts.is_empty())
            .then_some(self.details.backend_attempts.as_slice())
    }
    /// Performs the `causes` operation.
    pub fn causes(&self) -> &[std::sync::Arc<CameraError>] {
        &self.details.causes
    }

    /// Performs the `with_backend` operation.
    pub fn with_backend(mut self, backend: BackendId) -> Self {
        self.details.backend = Some(backend);
        self
    }
    /// Performs the `with_device` operation.
    pub fn with_device(mut self, device: DeviceId) -> Self {
        self.details.device = Some(device);
        self
    }
    /// Performs the `with_stage` operation.
    pub fn with_stage(mut self, stage: OperationStage) -> Self {
        self.details.stage = Some(stage);
        self
    }
    /// Performs the `with_operation` operation.
    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.details.operation = Some(operation.into());
        self
    }
    /// Performs the `with_native_code` operation.
    pub fn with_native_code(mut self, code: i64) -> Self {
        self.details.native_code = Some(code);
        self
    }
    /// Performs the `with_recovery_hint` operation.
    pub fn with_recovery_hint(mut self, hint: RecoveryHint) -> Self {
        self.recovery_hint = hint;
        self
    }
    /// Performs the `with_source` operation.
    pub fn with_source<E>(mut self, source: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        self.source = Some(SharedSource(std::sync::Arc::new(source)));
        self
    }

    /// Performs the `invalid_argument` operation.
    pub fn invalid_argument(field: &'static str, message: String) -> Self {
        Self::new(
            CameraErrorKind::InvalidArgument,
            format!("Invalid {field}: {message}"),
        )
        .with_operation(field)
    }
    /// Performs the `invalid_config` operation.
    pub fn invalid_config(message: String) -> Self {
        Self::invalid_argument("configuration", message)
    }
    /// Performs the `backend_not_compiled` operation.
    pub fn backend_not_compiled(backend: BackendId) -> Self {
        Self::new(
            CameraErrorKind::BackendNotCompiled,
            format!("Backend {backend} was not compiled into this build"),
        )
        .with_backend(backend)
    }
    /// Performs the `unsupported_target` operation.
    pub fn unsupported_target(backend: BackendId, target: &'static str) -> Self {
        Self::new(
            CameraErrorKind::UnsupportedTarget,
            format!("Backend {backend} is unsupported on target {target}"),
        )
        .with_backend(backend)
    }
    /// Performs the `backend_option_mismatch` operation.
    pub fn backend_option_mismatch(option_backend: BackendId, selected_backend: BackendId) -> Self {
        Self::new(
            CameraErrorKind::BackendOptionMismatch,
            format!("Backend option for {option_backend} cannot be used with {selected_backend}"),
        )
        .with_backend(selected_backend)
    }
    /// Performs the `backend_failure` operation.
    pub fn backend_failure(backend: BackendId, stage: OperationStage, message: String) -> Self {
        Self::new(CameraErrorKind::BackendFailure, message)
            .with_backend(backend)
            .with_stage(stage)
    }
    /// Performs the `native` operation.
    pub fn native(
        kind: CameraErrorKind,
        backend: BackendId,
        stage: OperationStage,
        operation: String,
        code: i64,
        message: String,
    ) -> Self {
        Self::new(
            kind,
            format!("{backend} {operation} failed (code {code}): {message}"),
        )
        .with_backend(backend)
        .with_stage(stage)
        .with_operation(operation)
        .with_native_code(code)
    }
    /// Performs the `all_backends_failed` operation.
    pub fn all_backends_failed(attempts: Vec<BackendAttempt>) -> Self {
        let details = attempts
            .iter()
            .map(|attempt| format!("{}: {}", attempt.backend(), attempt.error()))
            .collect::<Vec<_>>()
            .join("; ");
        let recovery_hint = aggregate_recovery_hint(
            attempts
                .iter()
                .map(|attempt| attempt.error().recovery_hint()),
        );
        let source = attempts
            .first()
            .map(|attempt| attempt.shared_error().clone());
        let mut error = Self::new(
            CameraErrorKind::MultipleBackendsFailed,
            format!("All preferred backends failed: {details}"),
        )
        .with_recovery_hint(recovery_hint);
        error.source = source.map(|source| SharedSource(source));
        error.details.backend_attempts = attempts;
        error
    }
    /// Performs the `device_busy` operation.
    pub fn device_busy(message: String) -> Self {
        Self::new(
            CameraErrorKind::DeviceBusy,
            format!("Device is busy: {message}"),
        )
    }
    /// Performs the `permission_required` operation.
    pub fn permission_required(message: String) -> Self {
        Self::new(
            CameraErrorKind::PermissionRequired,
            format!("Permission is required before opening {message}"),
        )
    }
    /// Performs the `permission_denied` operation.
    pub fn permission_denied(message: String) -> Self {
        Self::new(
            CameraErrorKind::PermissionDenied,
            format!("Permission denied: {message}"),
        )
    }
    /// Performs the `permission_managed_externally` operation.
    pub fn permission_managed_externally(backend: BackendId) -> Self {
        Self::new(CameraErrorKind::PermissionManagedExternally,
            format!("Permission for backend {backend} is managed by the application or platform adapter"))
            .with_backend(backend)
    }
    /// Performs the `stream_stopped` operation.
    pub fn stream_stopped() -> Self {
        Self::new(CameraErrorKind::StreamStopped, "Stream stopped")
    }
    /// Performs the `invalid_state` operation.
    pub fn invalid_state(message: String) -> Self {
        Self::new(
            CameraErrorKind::InvalidState,
            format!("Invalid state: {message}"),
        )
    }
    /// Performs the `ambiguous_device` operation.
    pub fn ambiguous_device(message: String) -> Self {
        Self::new(
            CameraErrorKind::AmbiguousDevice,
            format!("Device identity is ambiguous: {message}"),
        )
    }
    /// Performs the `disconnected` operation.
    pub fn disconnected(message: String) -> Self {
        Self::new(
            CameraErrorKind::DeviceDisconnected,
            format!("Device disconnected: {message}"),
        )
    }
    /// Performs the `control_not_readable` operation.
    pub fn control_not_readable(message: String) -> Self {
        Self::new(
            CameraErrorKind::ControlNotReadable,
            format!("Control is not readable: {message}"),
        )
    }
    /// Performs the `device_not_found` operation.
    pub fn device_not_found(message: String) -> Self {
        Self::new(
            CameraErrorKind::DeviceNotFound,
            format!("Device not found: {message}"),
        )
    }
    /// Performs the `device_open_failed` operation.
    pub fn device_open_failed(message: String) -> Self {
        Self::new(
            CameraErrorKind::DeviceOpenFailed,
            format!("Failed to open device: {message}"),
        )
    }
    /// Performs the `invalid_frame` operation.
    pub fn invalid_frame(message: String) -> Self {
        Self::new(
            CameraErrorKind::InvalidFrame,
            format!("Invalid frame: {message}"),
        )
    }
    /// Performs the `unsupported_format` operation.
    pub fn unsupported_format(message: String) -> Self {
        Self::new(
            CameraErrorKind::UnsupportedFormat,
            format!("Unsupported format: {message}"),
        )
    }
    /// Performs the `stream_error` operation.
    pub fn stream_error(message: String) -> Self {
        Self::new(
            CameraErrorKind::StreamFailure,
            format!("Stream error: {message}"),
        )
    }
    /// Performs the `timeout` operation.
    pub fn timeout(stage: OperationStage) -> Self {
        Self::new(
            CameraErrorKind::Timeout,
            format!("Operation timed out during {stage:?}"),
        )
        .with_stage(stage)
    }
    /// Performs the `buffer_exhausted` operation.
    pub fn buffer_exhausted() -> Self {
        Self::new(CameraErrorKind::BufferExhausted, "Frame buffer exhausted")
    }
    /// Performs the `control_not_supported` operation.
    pub fn control_not_supported(message: String) -> Self {
        Self::new(
            CameraErrorKind::ControlNotSupported,
            format!("Control not supported: {message}"),
        )
    }
    /// Performs the `control_batch` operation.
    pub fn control_batch(causes: Vec<std::sync::Arc<CameraError>>) -> Self {
        let details = causes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        let recovery_hint =
            aggregate_recovery_hint(causes.iter().map(|cause| cause.recovery_hint()));
        let source = causes.first().cloned();
        let mut error = Self::new(
            CameraErrorKind::ControlFailure,
            format!("Control operations failed: {details}"),
        )
        .with_recovery_hint(recovery_hint);
        error.source = source.map(|source| SharedSource(source));
        error.details.causes = causes;
        error
    }
    /// Performs the `io` operation.
    pub fn io(stage: OperationStage, source: std::io::Error) -> Self {
        let message = format!("I/O error during {stage:?}: {source}");
        Self::new(CameraErrorKind::Io, message)
            .with_stage(stage)
            .with_source(source)
    }
    /// Performs the `worker_failure` operation.
    pub fn worker_failure(message: String) -> Self {
        Self::new(CameraErrorKind::WorkerFailure, message).with_stage(OperationStage::Worker)
    }
    /// Performs the `internal` operation.
    pub fn internal(message: String) -> Self {
        Self::new(
            CameraErrorKind::Internal,
            format!("Internal error: {message}"),
        )
    }
    pub(crate) fn uvc_backend(code: i32, operation: String) -> Self {
        let kind = match UvcErrorCode::from_code(code) {
            UvcErrorCode::Access => CameraErrorKind::PermissionDenied,
            UvcErrorCode::NoDevice | UvcErrorCode::NotFound | UvcErrorCode::InvalidDevice => {
                CameraErrorKind::DeviceNotFound
            }
            UvcErrorCode::Busy => CameraErrorKind::DeviceBusy,
            UvcErrorCode::Timeout => CameraErrorKind::Timeout,
            UvcErrorCode::NotSupported => CameraErrorKind::UnsupportedFormat,
            UvcErrorCode::InvalidParam | UvcErrorCode::InvalidMode => {
                CameraErrorKind::InvalidArgument
            }
            _ => CameraErrorKind::BackendFailure,
        };
        Self::native(
            kind,
            BackendId::UVC,
            OperationStage::BackendCommand,
            operation,
            i64::from(code),
            UvcErrorCode::from_code(code).message().into(),
        )
    }
    #[cfg(any(
        test,
        all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        )
    ))]
    pub(crate) fn uvc(code: i32, operation: String) -> Self {
        let mut error = Self::uvc_backend(code, operation);
        error.kind = match error.kind {
            CameraErrorKind::UnsupportedFormat => CameraErrorKind::ControlNotSupported,
            CameraErrorKind::BackendFailure => CameraErrorKind::ControlFailure,
            kind => kind,
        };
        error.recovery_hint = default_recovery_hint(error.kind);
        error
    }
}

#[derive(Clone, Debug)]
struct SharedSource(std::sync::Arc<dyn StdError + Send + Sync + 'static>);

impl SharedSource {
    fn get(&self) -> &(dyn StdError + Send + Sync + 'static) {
        self.0.as_ref()
    }
}

impl fmt::Display for SharedSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.0.as_ref(), formatter)
    }
}

impl StdError for SharedSource {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.0.as_ref())
    }
}

impl From<std::io::Error> for CameraError {
    fn from(source: std::io::Error) -> Self {
        Self::io(OperationStage::BackendCommand, source)
    }
}

fn default_recovery_hint(kind: CameraErrorKind) -> RecoveryHint {
    match kind {
        CameraErrorKind::PermissionRequired
        | CameraErrorKind::PermissionDenied
        | CameraErrorKind::PermissionManagedExternally => RecoveryHint::RequestPermission,
        CameraErrorKind::DeviceNotFound | CameraErrorKind::DeviceDisconnected => {
            RecoveryHint::ReenumerateDevice
        }
        CameraErrorKind::DeviceBusy
        | CameraErrorKind::DeviceOpenFailed
        | CameraErrorKind::BackendFailure
        | CameraErrorKind::StreamFailure
        | CameraErrorKind::Timeout
        | CameraErrorKind::Io
        | CameraErrorKind::WorkerFailure => RecoveryHint::Retry,
        CameraErrorKind::InvalidArgument
        | CameraErrorKind::UnsupportedFormat
        | CameraErrorKind::ControlNotSupported
        | CameraErrorKind::ControlNotReadable => RecoveryHint::ChangeConfiguration,
        CameraErrorKind::BackendNotCompiled | CameraErrorKind::UnsupportedTarget => {
            RecoveryHint::EnableBackend
        }
        CameraErrorKind::InvalidFrame => RecoveryHint::DropFrame,
        CameraErrorKind::BufferExhausted => RecoveryHint::ReduceResourceUsage,
        CameraErrorKind::BackendOptionMismatch
        | CameraErrorKind::MultipleBackendsFailed
        | CameraErrorKind::AmbiguousDevice
        | CameraErrorKind::InvalidState
        | CameraErrorKind::StreamStopped
        | CameraErrorKind::ControlFailure
        | CameraErrorKind::Internal => RecoveryHint::None,
    }
}

fn aggregate_recovery_hint(hints: impl IntoIterator<Item = RecoveryHint>) -> RecoveryHint {
    hints
        .into_iter()
        .max_by_key(|hint| match hint {
            RecoveryHint::None => 0,
            RecoveryHint::DropFrame => 1,
            RecoveryHint::EnableBackend => 2,
            RecoveryHint::ChangeConfiguration => 3,
            RecoveryHint::ReduceResourceUsage => 4,
            RecoveryHint::Retry => 5,
            RecoveryHint::ReenumerateDevice => 6,
            RecoveryHint::RequestPermission => 7,
        })
        .unwrap_or(RecoveryHint::None)
}

#[allow(dead_code)]
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UvcErrorCode {
    Success = 0,
    Io = -1,
    InvalidParam = -2,
    Access = -3,
    NoDevice = -4,
    NotFound = -5,
    Busy = -6,
    Timeout = -7,
    Overflow = -8,
    Pipe = -9,
    Interrupted = -10,
    NoMem = -11,
    NotSupported = -12,
    InvalidDevice = -50,
    InvalidMode = -51,
    CallbackExists = -52,
    Other = -99,
}

#[allow(dead_code)]
impl UvcErrorCode {
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Success,
            -1 => Self::Io,
            -2 => Self::InvalidParam,
            -3 => Self::Access,
            -4 => Self::NoDevice,
            -5 => Self::NotFound,
            -6 => Self::Busy,
            -7 => Self::Timeout,
            -8 => Self::Overflow,
            -9 => Self::Pipe,
            -10 => Self::Interrupted,
            -11 => Self::NoMem,
            -12 => Self::NotSupported,
            -50 => Self::InvalidDevice,
            -51 => Self::InvalidMode,
            -52 => Self::CallbackExists,
            _ => Self::Other,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::Success => "Success",
            Self::Io => "I/O error",
            Self::InvalidParam => "Invalid parameter",
            Self::Access => "Access denied",
            Self::NoDevice => "Device does not exist",
            Self::NotFound => "Not found",
            Self::Busy => "Device busy",
            Self::Timeout => "Timeout",
            Self::Overflow => "Buffer overflow",
            Self::Pipe => "Pipe error",
            Self::Interrupted => "Operation interrupted",
            Self::NoMem => "Out of memory",
            Self::NotSupported => "Not supported",
            Self::InvalidDevice => "Invalid device",
            Self::InvalidMode => "Invalid mode",
            Self::CallbackExists => "Callback already exists",
            Self::Other => "Other error",
        }
    }
    pub fn check(
        code: i32,
        stage: OperationStage,
        operation: &'static str,
    ) -> std::result::Result<(), CameraError> {
        if code == 0 {
            Ok(())
        } else {
            Err(CameraError::uvc_backend(code, operation.into()).with_stage(stage))
        }
    }
}

impl fmt::Display for CameraErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for RecoveryHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) type Result<T> = std::result::Result<T, CameraError>;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uvc_statuses_are_semantically_normalized() {
        assert_eq!(
            CameraError::uvc_backend(-3, "open".into()).kind(),
            CameraErrorKind::PermissionDenied
        );
        assert_eq!(
            CameraError::uvc_backend(-6, "open".into()).kind(),
            CameraErrorKind::DeviceBusy
        );
        assert_eq!(
            CameraError::uvc_backend(-7, "read".into()).kind(),
            CameraErrorKind::Timeout
        );
        assert_eq!(
            CameraError::uvc(-12, "query exposure".into()).kind(),
            CameraErrorKind::ControlNotSupported
        );
    }
}
