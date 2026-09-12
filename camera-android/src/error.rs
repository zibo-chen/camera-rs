use camera::{BackendId, CameraErrorKind, OperationStage, RecoveryHint};
use serde_json::json;
use thiserror::Error;

/// Android/JNI adapter error that preserves the core camera error contract.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum CameraError {
    /// Error propagated by the platform-independent camera crate.
    #[error(transparent)]
    Camera(#[from] camera::CameraError),
    /// Failure while interacting with the JVM through JNI.
    #[error("JNI: {0}")]
    Jni(#[from] jni::errors::Error),
    /// Failure produced by Android adapter validation or state management.
    #[error("Android adapter: {message}")]
    Adapter {
        /// Stable machine-readable error category.
        kind: CameraErrorKind,
        /// Recommended recovery action for the caller.
        recovery_hint: RecoveryHint,
        /// Human-readable diagnostic message.
        message: String,
    },
    /// Operating-system I/O failure while handling device descriptors.
    #[error("Android I/O: {0}")]
    Io(#[from] std::io::Error),
}

impl CameraError {
    /// Creates an Android adapter error with no automatic recovery action.
    pub fn adapter(kind: CameraErrorKind, message: impl Into<String>) -> Self {
        Self::Adapter {
            kind,
            recovery_hint: RecoveryHint::None,
            message: message.into(),
        }
    }

    /// Returns the stable machine-readable error category.
    pub fn kind(&self) -> CameraErrorKind {
        match self {
            Self::Camera(error) => error.kind(),
            Self::Jni(_) => CameraErrorKind::BackendFailure,
            Self::Adapter { kind, .. } => *kind,
            Self::Io(_) => CameraErrorKind::Io,
        }
    }

    /// Returns the stable string code for this error category.
    pub fn code(&self) -> &'static str {
        self.kind().as_str()
    }

    /// Returns the recovery action recommended to the caller.
    pub fn recovery_hint(&self) -> RecoveryHint {
        match self {
            Self::Camera(error) => error.recovery_hint(),
            Self::Jni(_) | Self::Adapter { .. } => RecoveryHint::None,
            Self::Io(_) => RecoveryHint::Retry,
        }
    }

    /// Returns whether retrying is a meaningful recovery action.
    pub fn is_retryable(&self) -> bool {
        self.recovery_hint().is_retryable()
    }

    /// Returns the backend associated with a core camera error, if known.
    pub fn backend(&self) -> Option<&BackendId> {
        match self {
            Self::Camera(error) => error.backend(),
            _ => None,
        }
    }

    /// Returns the operation stage at which the failure occurred, if known.
    pub fn stage(&self) -> Option<OperationStage> {
        match self {
            Self::Camera(error) => error.stage(),
            Self::Jni(_) | Self::Adapter { .. } => Some(OperationStage::BackendCommand),
            Self::Io(_) => Some(OperationStage::Open),
        }
    }

    /// Returns the native platform error code, if one is available.
    pub fn native_code(&self) -> Option<i64> {
        match self {
            Self::Camera(error) => error.native_code(),
            Self::Io(error) => error.raw_os_error().map(i64::from),
            _ => None,
        }
    }

    /// JSON carried by the JNI exception message. Kotlin/Flutter callers can
    /// branch on `code` and `recovery` without parsing localized prose.
    pub fn boundary_payload(&self) -> String {
        json!({
            "schemaVersion": 1,
            "code": self.code(),
            "recovery": self.recovery_hint().as_str(),
            "backend": self.backend().map(BackendId::as_str),
            "stage": self.stage().map(OperationStage::as_str),
            "nativeCode": self.native_code(),
            "message": self.to_string(),
        })
        .to_string()
    }
}

/// Result type returned by the Android adapter APIs.
pub type Result<T> = std::result::Result<T, CameraError>;
