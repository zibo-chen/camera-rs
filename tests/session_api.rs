use camera::{CameraError, CameraSystem, FrameRate, OutputFormat, SelectionPolicy, StreamRequest};
#[test]
fn rational_rates_preserve_precision_and_reject_zero() {
    assert_eq!(
        FrameRate::new(60000, 2002).unwrap(),
        FrameRate::new(30000, 1001).unwrap()
    );
    assert!(FrameRate::new(0, 1).is_err());
    assert!(FrameRate::new(1, 0).is_err());
}
#[tokio::test]
async fn synthetic_session_has_independent_receivers_and_stop_wakes_waiters() {
    let system = CameraSystem::synthetic();
    let devices = system.devices().await.unwrap();
    let mut camera = system.open(&devices[0].id).await.unwrap();
    let session = camera
        .start(
            StreamRequest::builder()
                .resolution(16, 8)
                .selection(SelectionPolicy::Exact)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut a = session.subscribe();
    let mut b = session.subscribe();
    let first = a.next().await.unwrap();
    let same = b.next().await.unwrap();
    assert_eq!(first.key, same.key);
    let second = a.next().await.unwrap();
    assert_ne!(first.key, second.key);
    assert_eq!(first.layout().width, 16);
    session.stop().await.unwrap();
    assert!(matches!(a.next().await, Err(CameraError::StreamStopped)));
}
#[tokio::test]
async fn dropping_session_stops_retained_receivers() {
    let system = CameraSystem::synthetic();
    let d = system.devices().await.unwrap();
    let mut c = system.open(&d[0].id).await.unwrap();
    let s = c
        .start(StreamRequest::builder().build().unwrap())
        .await
        .unwrap();
    let mut r = s.subscribe();
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
    let mut c = system.open(&d[0].id).await.unwrap();
    let s = c
        .start(
            StreamRequest::builder()
                .output(OutputFormat::Native)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let f = s.subscribe().next().await.unwrap();
    assert_eq!(f.bytes().len(), f.layout().planes[0].length);
    s.stop().await.unwrap();
}

#[tokio::test]
async fn stopping_an_old_session_cannot_stop_a_new_one() {
    let system = CameraSystem::synthetic();
    let devices = system.devices().await.unwrap();
    let mut camera = system.open(&devices[0].id).await.unwrap();
    let old = camera
        .start(StreamRequest::builder().build().unwrap())
        .await
        .unwrap();
    old.stop().await.unwrap();
    let new = camera
        .start(StreamRequest::builder().build().unwrap())
        .await
        .unwrap();
    old.stop().await.unwrap();
    assert!(new.subscribe().next().await.is_ok());
    new.stop().await.unwrap();
}

#[tokio::test]
async fn old_receivers_and_session_views_do_not_attach_to_new_capture() {
    let sys = CameraSystem::synthetic();
    let mut camera = sys.open(&sys.devices().await.unwrap()[0].id).await.unwrap();
    let old = camera
        .start(StreamRequest::builder().resolution(8, 8).build().unwrap())
        .await
        .unwrap();
    let mut reader = old.subscribe();
    old.stop().await.unwrap();
    let stopped_bytes = old.metrics().allocated_bytes;
    let new = camera
        .start(StreamRequest::builder().resolution(16, 16).build().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        reader.next().await,
        Err(CameraError::StreamStopped)
    ));
    assert!(matches!(
        old.subscribe().next().await,
        Err(CameraError::StreamStopped)
    ));
    assert!(old.latest().is_none());
    assert_eq!(old.subscribe().queued_frames(), 0);
    assert_eq!(reader.queued_frames(), 0);
    assert_eq!(old.metrics().allocated_bytes, stopped_bytes);
    assert_eq!(new.latest().unwrap().layout().width, 16);
    new.stop().await.unwrap();
}

#[tokio::test]
async fn exact_fractional_rate_is_preserved_and_wrong_format_is_rejected() {
    let sys = CameraSystem::synthetic();
    let mut camera = sys.open(&sys.devices().await.unwrap()[0].id).await.unwrap();
    let invalid = StreamRequest::builder()
        .capture_format(camera::VideoFormat::MJPEG)
        .build()
        .unwrap();
    assert!(matches!(
        camera.start(invalid).await,
        Err(CameraError::UnsupportedFormat(_))
    ));
    let fps = FrameRate::new(30000, 1001).unwrap();
    let s = camera
        .start(
            StreamRequest::builder()
                .resolution(8, 8)
                .frame_rate(fps)
                .selection(SelectionPolicy::Exact)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(s.negotiated_config().frame_rate, fps);
    s.stop().await.unwrap();
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
