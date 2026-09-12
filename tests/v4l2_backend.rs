#![cfg(feature = "runtime-tokio")]

use camera::{BackendAvailability, BackendId, CameraSystem};

#[test]
fn v4l2_is_available_only_on_supported_platforms() {
    let expected = if cfg!(feature = "backend-v4l2") {
        if cfg!(any(target_os = "linux", target_os = "android")) {
            BackendAvailability::Available
        } else {
            BackendAvailability::UnsupportedTarget
        }
    } else {
        BackendAvailability::NotCompiled
    };
    assert_eq!(
        CameraSystem::new().backend_availability(&BackendId::V4L2),
        expected
    );
}
