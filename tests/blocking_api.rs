#![cfg(feature = "blocking")]

use camera::{blocking, CaptureRequest, DeviceSelector, SubscriptionOptions};

#[test]
fn blocking_facade_reuses_the_capture_contract() {
    let system = blocking::CameraSystem::synthetic();
    let mut device = system.open(DeviceSelector::Default).unwrap();
    let session = device
        .start(
            CaptureRequest::builder()
                .preferred_resolution(8, 6)
                .build()
                .unwrap(),
        )
        .unwrap();
    let mut frames = session.subscribe(SubscriptionOptions::latest()).unwrap();
    let frame = frames.next_frame().unwrap();
    assert_eq!((frame.layout().width, frame.layout().height), (8, 6));
    session.close().unwrap();
}
