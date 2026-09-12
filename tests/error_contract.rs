use camera::{
    BackendAttempt, BackendId, CameraError, CameraErrorKind, OperationStage, RecoveryHint,
    SessionState,
};
use std::{error::Error as _, sync::Arc};

#[derive(Debug, thiserror::Error)]
#[error("driver rejected the command")]
struct DriverError;

#[test]
fn callers_can_classify_permission_failures_without_parsing_messages() {
    let error = CameraError::permission_denied("camera authorization was denied".into())
        .with_backend(BackendId::CAMERA2)
        .with_stage(OperationStage::Open);

    assert_eq!(error.kind(), CameraErrorKind::PermissionDenied);
    assert_eq!(error.recovery_hint(), RecoveryHint::RequestPermission);
    assert_eq!(error.backend(), Some(&BackendId::CAMERA2));
    assert_eq!(error.stage(), Some(OperationStage::Open));
    assert!(!error.is_retryable());
}

#[test]
fn native_failures_retain_platform_diagnostics() {
    let error = CameraError::native(
        CameraErrorKind::DeviceBusy,
        BackendId::UVC,
        OperationStage::Open,
        "uvc_open".into(),
        -6,
        "device busy".into(),
    );

    assert_eq!(error.kind(), CameraErrorKind::DeviceBusy);
    assert_eq!(error.backend(), Some(&BackendId::UVC));
    assert_eq!(error.stage(), Some(OperationStage::Open));
    assert_eq!(error.operation(), Some("uvc_open"));
    assert_eq!(error.native_code(), Some(-6));
    assert_eq!(error.recovery_hint(), RecoveryHint::Retry);
    assert!(error.is_retryable());
    assert_eq!(error.kind().as_str(), "device_busy");
    assert_eq!(error.recovery_hint().as_str(), "retry");
    assert_eq!(error.stage().unwrap().as_str(), "open");
}

#[test]
fn io_failures_preserve_the_standard_error_source() {
    let error = CameraError::io(
        OperationStage::Enumeration,
        std::io::Error::from_raw_os_error(libc::EACCES),
    );

    assert_eq!(error.kind(), CameraErrorKind::Io);
    assert_eq!(error.stage(), Some(OperationStage::Enumeration));
    assert_eq!(
        error
            .source()
            .and_then(std::error::Error::source)
            .and_then(|source| source.downcast_ref::<std::io::Error>())
            .and_then(std::io::Error::raw_os_error),
        Some(libc::EACCES)
    );
}

#[test]
fn cloning_an_error_preserves_the_typed_source() {
    let error = CameraError::backend_failure(
        BackendId::UVC,
        OperationStage::Startup,
        "driver rejected the command".into(),
    )
    .with_source(DriverError);

    let cloned = error.clone();
    assert!(cloned
        .source_error()
        .is_some_and(|source| source.downcast_ref::<DriverError>().is_some()));
}

#[test]
fn backend_attempts_keep_typed_errors() {
    let cause = Arc::new(
        CameraError::device_busy("USB camera is already in use".into())
            .with_backend(BackendId::UVC)
            .with_stage(OperationStage::Open),
    );
    let error = CameraError::all_backends_failed(vec![BackendAttempt::new(BackendId::UVC, cause)]);

    assert_eq!(error.kind(), CameraErrorKind::MultipleBackendsFailed);
    let attempts = error.backend_attempts().expect("attempt diagnostics");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].error().kind(), CameraErrorKind::DeviceBusy);
    assert_eq!(attempts[0].error().backend(), Some(&BackendId::UVC));
    assert_eq!(error.recovery_hint(), RecoveryHint::Retry);
    assert!(error.is_retryable());
    assert_eq!(
        error
            .source_error()
            .and_then(|source| source.downcast_ref::<CameraError>())
            .map(CameraError::kind),
        Some(CameraErrorKind::DeviceBusy)
    );
}

#[test]
fn control_batches_expose_an_actionable_recovery_and_source() {
    let cause = Arc::new(CameraError::control_not_supported("focus".into()));
    let error = CameraError::control_batch(vec![cause]);

    assert_eq!(error.recovery_hint(), RecoveryHint::ChangeConfiguration);
    assert_eq!(
        error
            .source_error()
            .and_then(|source| source.downcast_ref::<CameraError>())
            .map(CameraError::kind),
        Some(CameraErrorKind::ControlNotSupported)
    );
}

#[test]
fn failed_session_state_retains_the_original_typed_error() {
    let cause = Arc::new(
        CameraError::disconnected("camera unplugged".into())
            .with_backend(BackendId::V4L2)
            .with_stage(OperationStage::FrameWait),
    );
    let state = SessionState::Failed(cause);

    let error = state.error().expect("typed failure");
    assert_eq!(error.kind(), CameraErrorKind::DeviceDisconnected);
    assert_eq!(error.recovery_hint(), RecoveryHint::ReenumerateDevice);
}

#[test]
fn frame_and_configuration_failures_are_distinct() {
    let frame = CameraError::invalid_frame("truncated NV12 chroma plane".into());
    let request = CameraError::unsupported_format("H264 decoding is unavailable".into());

    assert_eq!(frame.kind(), CameraErrorKind::InvalidFrame);
    assert_eq!(frame.recovery_hint(), RecoveryHint::DropFrame);
    assert_eq!(request.kind(), CameraErrorKind::UnsupportedFormat);
    assert_eq!(request.recovery_hint(), RecoveryHint::ChangeConfiguration);
    assert!(!CameraError::buffer_exhausted().is_retryable());
}

#[test]
fn machine_codes_round_trip_without_parsing_display_messages() {
    assert_eq!(
        "device_busy".parse::<CameraErrorKind>().unwrap(),
        CameraErrorKind::DeviceBusy
    );
    assert_eq!(
        "request_permission".parse::<RecoveryHint>().unwrap(),
        RecoveryHint::RequestPermission
    );
    assert_eq!(
        "first_frame".parse::<OperationStage>().unwrap(),
        OperationStage::FirstFrame
    );

    let error = "not_a_camera_code"
        .parse::<CameraErrorKind>()
        .expect_err("unknown wire codes must be rejected");
    assert_eq!(error.kind(), CameraErrorKind::InvalidArgument);
}

#[cfg(feature = "serde")]
#[test]
fn machine_enums_use_stable_snake_case_serde_values() {
    assert_eq!(
        serde_json::to_string(&CameraErrorKind::DeviceDisconnected).unwrap(),
        "\"device_disconnected\""
    );
    assert_eq!(
        serde_json::from_str::<RecoveryHint>("\"reduce_resource_usage\"").unwrap(),
        RecoveryHint::ReduceResourceUsage
    );
    assert_eq!(
        serde_json::from_str::<OperationStage>("\"backend_command\"").unwrap(),
        OperationStage::BackendCommand
    );
}
