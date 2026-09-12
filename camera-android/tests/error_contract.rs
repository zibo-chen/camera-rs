use camera::{BackendId, CameraErrorKind, OperationStage, RecoveryHint};
use camera_android::error::CameraError;

#[test]
fn core_error_metadata_survives_the_android_adapter() {
    let core = camera::CameraError::permission_denied("USB permission denied".into())
        .with_backend(BackendId::UVC)
        .with_stage(OperationStage::Open);
    let error = CameraError::from(core);

    assert_eq!(error.kind(), CameraErrorKind::PermissionDenied);
    assert_eq!(error.recovery_hint(), RecoveryHint::RequestPermission);
    assert_eq!(error.backend(), Some(&BackendId::UVC));
    assert_eq!(error.stage(), Some(OperationStage::Open));
    assert_eq!(error.code(), "permission_denied");
    assert!(!error.is_retryable());
}

#[test]
fn adapter_retryability_matches_the_core_contract() {
    let retryable = CameraError::from(camera::CameraError::device_busy("camera".into()));
    let resource_pressure = CameraError::from(camera::CameraError::buffer_exhausted());

    assert!(retryable.is_retryable());
    assert!(!resource_pressure.is_retryable());
}

#[test]
fn jni_boundary_payload_is_machine_readable() {
    let error = CameraError::from(
        camera::CameraError::device_busy("camera is in use".into())
            .with_backend(BackendId::CAMERA2),
    );
    let payload: serde_json::Value = serde_json::from_str(&error.boundary_payload()).unwrap();

    assert_eq!(payload["schemaVersion"], 1);
    assert_eq!(payload["code"], "device_busy");
    assert_eq!(payload["backend"], "camera2");
    assert_eq!(payload["recovery"], "retry");
    assert!(payload["message"]
        .as_str()
        .unwrap()
        .contains("camera is in use"));
}

#[test]
fn default_camera2_java_api_can_deliver_rgb_frames() {
    const {
        assert!(
            cfg!(feature = "convert-rgb"),
            "camera-rs-android's default Java start() API requests RGB frames, so the default feature set must include convert-rgb"
        );
    }
}
