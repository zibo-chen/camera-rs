#![cfg(feature = "runtime-tokio")]

use camera::{
    CameraError, CameraSystem, CaptureFormat, CaptureRequest, DeviceSelector, FrameRate,
    SubscriptionOptions,
};
#[test]
fn rational_rates_preserve_precision_and_reject_zero() {
    assert_eq!(
        FrameRate::new(60000, 2002).unwrap(),
        FrameRate::new(30000, 1001).unwrap()
    );
    assert!(FrameRate::new(0, 1).is_err());
    assert!(FrameRate::new(1, 0).is_err());
}

#[cfg(feature = "serde")]
#[test]
fn frame_rate_deserialization_validates_and_normalizes() {
    assert!(serde_json::from_str::<FrameRate>(r#"{"numerator":0,"denominator":1}"#).is_err());
    assert!(serde_json::from_str::<FrameRate>(r#"{"numerator":1,"denominator":0}"#).is_err());
    let normalized: FrameRate =
        serde_json::from_str(r#"{"numerator":60,"denominator":2}"#).unwrap();
    assert_eq!(normalized, FrameRate::new(30, 1).unwrap());
    assert_eq!(
        serde_json::to_string(&normalized).unwrap(),
        r#"{"numerator":30,"denominator":1}"#
    );
}
#[tokio::test]
async fn synthetic_session_has_independent_receivers_and_stop_wakes_waiters() {
    let system = CameraSystem::synthetic();
    let devices = system.devices().await.unwrap();
    let mut camera = system
        .open(DeviceSelector::Id(devices[0].id.clone()))
        .await
        .unwrap();
    let session = camera
        .start(
            CaptureRequest::builder()
                .exact_resolution(16, 8)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut a = session.subscribe(SubscriptionOptions::latest()).unwrap();
    let mut b = session.subscribe(SubscriptionOptions::latest()).unwrap();
    let first = a.next().await.unwrap();
    let same = b.next().await.unwrap();
    assert_eq!(first.key, same.key);
    let second = a.next().await.unwrap();
    assert_ne!(first.key, second.key);
    assert_eq!(first.layout().width, 16);
    session.close().await.unwrap();
    assert!(matches!(a.next().await, Err(CameraError::StreamStopped)));
}
#[tokio::test]
async fn dropping_session_stops_retained_receivers() {
    let system = CameraSystem::synthetic();
    let d = system.devices().await.unwrap();
    let mut c = system
        .open(DeviceSelector::Id(d[0].id.clone()))
        .await
        .unwrap();
    let s = c
        .start(CaptureRequest::builder().build().unwrap())
        .await
        .unwrap();
    let mut r = s.subscribe(SubscriptionOptions::latest()).unwrap();
    drop(s);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), r.next())
        .await
        .unwrap();
    assert!(matches!(result, Err(CameraError::StreamStopped)));
}
#[tokio::test]
async fn requested_native_output_is_described_honestly() {
    let system = CameraSystem::synthetic();
    let d = system.devices().await.unwrap();
    let mut c = system
        .open(DeviceSelector::Id(d[0].id.clone()))
        .await
        .unwrap();
    let s = c
        .start(CaptureRequest::builder().build().unwrap())
        .await
        .unwrap();
    let f = s
        .subscribe(SubscriptionOptions::latest())
        .unwrap()
        .next()
        .await
        .unwrap();
    assert_eq!(f.bytes().len(), f.layout().planes[0].length);
    s.close().await.unwrap();
}

#[tokio::test]
async fn stopping_an_old_session_cannot_stop_a_new_one() {
    let system = CameraSystem::synthetic();
    let devices = system.devices().await.unwrap();
    let mut camera = system
        .open(DeviceSelector::Id(devices[0].id.clone()))
        .await
        .unwrap();
    let old = camera
        .start(CaptureRequest::builder().build().unwrap())
        .await
        .unwrap();
    old.close().await.unwrap();
    let new = camera
        .start(CaptureRequest::builder().build().unwrap())
        .await
        .unwrap();
    old.close().await.unwrap();
    assert!(new
        .subscribe(SubscriptionOptions::latest())
        .unwrap()
        .next()
        .await
        .is_ok());
    new.close().await.unwrap();
}

#[tokio::test]
async fn old_receivers_and_session_views_do_not_attach_to_new_capture() {
    let sys = CameraSystem::synthetic();
    let id = sys.devices().await.unwrap()[0].id.clone();
    let mut camera = sys.open(DeviceSelector::Id(id)).await.unwrap();
    let old = camera
        .start(
            CaptureRequest::builder()
                .preferred_resolution(8, 8)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut reader = old.subscribe(SubscriptionOptions::latest()).unwrap();
    old.close().await.unwrap();
    let stopped_bytes = old.metrics().allocated_bytes;
    let new = camera
        .start(
            CaptureRequest::builder()
                .preferred_resolution(16, 16)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        reader.next().await,
        Err(CameraError::StreamStopped)
    ));
    assert!(matches!(
        old.subscribe(SubscriptionOptions::latest())
            .unwrap()
            .next()
            .await,
        Err(CameraError::StreamStopped)
    ));
    assert!(old.latest().is_none());
    assert_eq!(
        old.subscribe(SubscriptionOptions::latest())
            .unwrap()
            .queued_frames(),
        0
    );
    assert_eq!(reader.queued_frames(), 0);
    assert_eq!(old.metrics().allocated_bytes, stopped_bytes);
    assert_eq!(new.latest().unwrap().layout().width, 16);
    new.close().await.unwrap();
}

#[tokio::test]
async fn exact_fractional_rate_is_preserved_and_wrong_format_is_rejected() {
    let sys = CameraSystem::synthetic();
    let id = sys.devices().await.unwrap()[0].id.clone();
    let mut camera = sys.open(DeviceSelector::Id(id)).await.unwrap();
    let invalid = CaptureRequest::builder()
        .preferred_formats([CaptureFormat::Mjpeg])
        .build()
        .unwrap();
    assert!(matches!(
        camera.start(invalid).await,
        Err(CameraError::UnsupportedFormat(_))
    ));
    let fps = FrameRate::new(30000, 1001).unwrap();
    let s = camera
        .start(
            CaptureRequest::builder()
                .exact_resolution(8, 8)
                .preferred_frame_rate(fps)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(s.negotiated().capture.frame_rate, fps);
    s.close().await.unwrap();
}

#[tokio::test]
async fn required_aspect_ratio_is_enforced_for_synthetic_capture() {
    let system = CameraSystem::synthetic();
    let mut camera = system.open(DeviceSelector::Default).await.unwrap();
    let request = CaptureRequest::builder()
        .preferred_resolution(640, 480)
        .require_aspect_ratio(16, 9)
        .build()
        .unwrap();
    assert!(matches!(
        camera.start(request).await,
        Err(CameraError::UnsupportedFormat(_))
    ));
}

#[tokio::test]
async fn stopping_device_watcher_is_terminal_and_idempotent() {
    let mut watcher = CameraSystem::synthetic()
        .watch_devices(std::time::Duration::from_millis(1))
        .await
        .unwrap();
    assert_eq!(watcher.snapshot().devices.len(), 1);
    watcher.stop().await.unwrap();
    watcher.stop().await.unwrap();
    assert!(matches!(
        watcher.changed().await,
        Err(CameraError::StreamStopped)
    ));
}
