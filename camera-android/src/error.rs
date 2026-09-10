use thiserror::Error;
#[derive(Debug, Error)]
pub enum CameraError {
    #[error("{0}")]
    Camera(#[from] camera::CameraError),
    #[error("JNI: {0}")]
    Jni(#[from] jni::errors::Error),
    #[error("Android: {0}")]
    Android(String),
    #[error("Device not found: {0}")]
    DeviceNotFound(String),
    #[error("Open failed: {0}")]
    DeviceOpenFailed(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, CameraError>;
