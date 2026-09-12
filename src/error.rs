//! 错误类型定义

use thiserror::Error;

/// 摄像头错误类型
#[derive(Error, Debug)]
pub enum CameraError {
    /// Native failure with its original status code and operation context.
    #[error("{backend} {operation} failed (code {code}): {message}")]
    Native {
        backend: crate::BackendId,
        operation: String,
        code: i64,
        message: String,
    },
    #[error("Control operations failed: {0:?}")]
    ControlBatch(Vec<String>),
    #[error("Backend {backend} was not compiled into this build")]
    BackendNotCompiled { backend: crate::BackendId },
    #[error("Backend {backend} is unsupported on target {target}")]
    UnsupportedTarget {
        backend: crate::BackendId,
        target: &'static str,
    },
    #[error("Backend option for {option_backend} cannot be used with {selected_backend}")]
    BackendOptionMismatch {
        option_backend: crate::BackendId,
        selected_backend: crate::BackendId,
    },
    #[error("All preferred backends failed: {0:?}")]
    BackendAttemptsFailed(Vec<crate::BackendAttempt>),
    #[error("Device is busy: {0}")]
    DeviceBusy(String),
    #[error("Permission is required before opening {0}")]
    PermissionRequired(String),
    #[error("Permission for backend {backend} is managed by the application or platform adapter")]
    PermissionManagedExternally { backend: crate::BackendId },
    #[error("Stream stopped")]
    StreamStopped,
    #[error("Invalid state: {0}")]
    InvalidState(String),
    #[error("Device identity is ambiguous: {0}")]
    AmbiguousDevice(String),
    #[error("Device disconnected: {0}")]
    Disconnected(String),
    #[error("Control is not readable: {0}")]
    NotReadable(String),
    /// IO 错误
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// 设备未找到
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    /// 设备打开失败
    #[error("Failed to open device: {0}")]
    DeviceOpenFailed(String),

    /// 配置错误
    #[error("Invalid config: {0}")]
    InvalidConfig(String),

    /// 无效的格式/数据
    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    /// 不支持的格式
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    /// 流错误
    #[error("Stream error: {0}")]
    StreamError(String),

    /// 超时
    #[error("Operation timed out during {stage:?}")]
    Timeout { stage: crate::OperationStage },

    /// 缓冲区为空
    #[error("Buffer empty")]
    BufferEmpty,

    /// 权限错误
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// UVC 特定错误
    #[error("UVC error (code {code}): {message}")]
    UvcError { code: i32, message: String },

    /// 控制参数不支持
    #[error("Control not supported: {0}")]
    ControlNotSupported(String),

    /// 其他错误
    #[error("Unknown error: {0}")]
    Other(String),
}

/// UVC 特定错误码
#[allow(dead_code)] // Used only by the optional UVC adapter and its protocol tests.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvcErrorCode {
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
    /// 从错误码转换
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => UvcErrorCode::Success,
            -1 => UvcErrorCode::Io,
            -2 => UvcErrorCode::InvalidParam,
            -3 => UvcErrorCode::Access,
            -4 => UvcErrorCode::NoDevice,
            -5 => UvcErrorCode::NotFound,
            -6 => UvcErrorCode::Busy,
            -7 => UvcErrorCode::Timeout,
            -8 => UvcErrorCode::Overflow,
            -9 => UvcErrorCode::Pipe,
            -10 => UvcErrorCode::Interrupted,
            -11 => UvcErrorCode::NoMem,
            -12 => UvcErrorCode::NotSupported,
            -50 => UvcErrorCode::InvalidDevice,
            -51 => UvcErrorCode::InvalidMode,
            -52 => UvcErrorCode::CallbackExists,
            _ => UvcErrorCode::Other,
        }
    }

    /// 转换为错误消息
    pub fn message(&self) -> &'static str {
        match self {
            UvcErrorCode::Success => "Success",
            UvcErrorCode::Io => "IO error",
            UvcErrorCode::InvalidParam => "Invalid parameter",
            UvcErrorCode::Access => "Access denied",
            UvcErrorCode::NoDevice => "Device does not exist",
            UvcErrorCode::NotFound => "Not found",
            UvcErrorCode::Busy => "Device busy",
            UvcErrorCode::Timeout => "Timeout",
            UvcErrorCode::Overflow => "Buffer overflow",
            UvcErrorCode::Pipe => "Pipe error",
            UvcErrorCode::Interrupted => "Operation interrupted",
            UvcErrorCode::NoMem => "Out of memory",
            UvcErrorCode::NotSupported => "Not supported",
            UvcErrorCode::InvalidDevice => "Invalid device",
            UvcErrorCode::InvalidMode => "Invalid mode",
            UvcErrorCode::CallbackExists => "Callback already exists",
            UvcErrorCode::Other => "Other error",
        }
    }

    /// 检查错误码并转换为 Result
    pub fn check(code: i32) -> std::result::Result<(), CameraError> {
        if code == 0 {
            Ok(())
        } else {
            let error_code = Self::from_code(code);
            Err(CameraError::UvcError {
                code,
                message: error_code.message().to_string(),
            })
        }
    }
}

/// 便捷的 Result 类型
pub type Result<T> = std::result::Result<T, CameraError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uvc_error_code() {
        assert_eq!(UvcErrorCode::from_code(0), UvcErrorCode::Success);
        assert_eq!(UvcErrorCode::from_code(-1), UvcErrorCode::Io);
        assert_eq!(UvcErrorCode::from_code(-99), UvcErrorCode::Other);
    }

    #[test]
    fn test_error_check() {
        assert!(UvcErrorCode::check(0).is_ok());
        assert!(UvcErrorCode::check(-1).is_err());
    }
}
