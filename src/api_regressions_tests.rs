use crate::*;
use std::sync::Arc;
fn publish(h: &FrameHub, s: u64) {
    h.publish_rgb(s, 2, 2, None, |b| {
        b.fill(1);
        Ok(())
    })
    .unwrap();
}
#[test]
fn weak_pixels_do_not_panic_or_keep_a_slot_unusable() {
    let h = FrameHub::new(2);
    let s = h.start();
    publish(&h, s);
    let a = h.latest().unwrap();
    let weak = Arc::downgrade(a.rgb_pixels().unwrap());
    publish(&h, s);
    drop(a);
    publish(&h, s);
    assert_eq!(h.metrics().published, 3);
    drop(weak);
}
#[test]
fn native_identity_uses_device_path() {
    let mut a = CameraDeviceInfo::new(0, "a".into(), "".into());
    a.device_path = Some("native-a".into());
    let mut b = CameraDeviceInfo::new(0, "b".into(), "".into());
    b.device_path = Some("native-b".into());
    assert_ne!(a.unique_id(), b.unique_id());
}
