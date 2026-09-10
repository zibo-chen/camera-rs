use crate::{FrameHub, FrameKey};
use std::{sync::Arc, time::Duration};

fn publish(hub: &FrameHub, epoch: u64, value: u8) -> bool {
    hub.publish_rgb(epoch, 2, 2, Some(123), |out| {
        out.fill(value);
        Ok(())
    })
    .unwrap()
}

#[tokio::test]
async fn metadata_and_pixels_are_one_snapshot_and_next_does_not_repeat() {
    let hub = FrameHub::new(3);
    let epoch = hub.start();
    assert!(publish(&hub, epoch, 7));
    let first = hub.latest().unwrap();
    assert_eq!(
        first.key,
        FrameKey {
            session: epoch,
            sequence: 1
        }
    );
    assert_eq!(first.source_timestamp_ns, Some(123));
    assert!(Arc::ptr_eq(&first, &hub.latest().unwrap()));
    assert!(hub
        .wait_after(Some(first.key), Duration::from_millis(5))
        .await
        .is_err());
    assert!(publish(&hub, epoch, 8));
    let next = hub
        .wait_after(Some(first.key), Duration::from_millis(5))
        .await
        .unwrap();
    assert_eq!(next.key.sequence, 2);
    assert_eq!(first.bytes()[0], 7);
    assert_eq!(next.bytes()[0], 8);
}

#[tokio::test]
async fn restart_rejects_late_callbacks_and_stop_wakes_waiters() {
    let hub = Arc::new(FrameHub::new(3));
    let old = hub.start();
    publish(&hub, old, 1);
    let old_key = hub.latest().unwrap().key;
    hub.stop();
    assert!(hub.latest().is_none());
    let new = hub.start();
    assert_ne!(old, new);
    assert!(!publish(&hub, old, 2));
    publish(&hub, new, 3);
    let frame = hub
        .wait_after(Some(old_key), Duration::from_millis(10))
        .await
        .unwrap();
    let waiter = {
        let hub = hub.clone();
        tokio::spawn(async move {
            hub.wait_after(Some(frame.key), Duration::from_secs(10))
                .await
        })
    };
    tokio::task::yield_now().await;
    hub.stop();
    assert!(tokio::time::timeout(Duration::from_millis(100), waiter)
        .await
        .unwrap()
        .unwrap()
        .is_err());
}

#[test]
fn retained_frames_bound_memory_and_reuse_buffers() {
    let hub = FrameHub::new(2);
    let epoch = hub.start();
    publish(&hub, epoch, 1);
    let first = hub.latest().unwrap();
    publish(&hub, epoch, 2);
    assert!(!publish(&hub, epoch, 3));
    assert_eq!(hub.metrics().pool_drops, 1);
    drop(first);
    for value in 4..100 {
        assert!(publish(&hub, epoch, value));
    }
    assert_eq!(hub.metrics().allocated_buffers, 2);
    assert_eq!(hub.latest().unwrap().bytes()[0], 99);
}

#[test]
fn failed_conversion_preserves_latest_and_instances_are_isolated() {
    let a = FrameHub::new(2);
    let b = FrameHub::new(2);
    let ae = a.start();
    let be = b.start();
    assert_ne!(ae, be);
    publish(&a, ae, 1);
    publish(&b, be, 2);
    assert!(a
        .publish_rgb(ae, 2, 2, None, |_| Err(crate::CameraError::BufferEmpty))
        .is_err());
    assert_eq!(a.latest().unwrap().bytes()[0], 1);
    assert_eq!(b.latest().unwrap().bytes()[0], 2);
    assert!(a
        .publish_rgb(ae, u32::MAX, u32::MAX, None, |_| Ok(()))
        .is_err());
}

#[tokio::test]
async fn delivery_policies_bound_each_queue_and_report_drops() {
    use crate::{DeliveryPolicy, OverflowPolicy, StreamRequest};
    for (delivery, expected) in [
        (DeliveryPolicy::Latest, vec![4]),
        (
            DeliveryPolicy::Buffered {
                capacity: 2,
                overflow: OverflowPolicy::DropOldest,
            },
            vec![3, 4],
        ),
        (
            DeliveryPolicy::Buffered {
                capacity: 2,
                overflow: OverflowPolicy::DropNewest,
            },
            vec![1, 2],
        ),
    ] {
        let hub = FrameHub::new(6);
        hub.configure(
            &StreamRequest::builder()
                .resolution(2, 2)
                .delivery(delivery)
                .build()
                .unwrap(),
        )
        .unwrap();
        let epoch = hub.start();
        let mut receiver = hub.subscribe();
        for value in 1..=4 {
            assert!(publish(&hub, epoch, value));
        }
        assert_eq!(receiver.dropped_frames(), 4 - expected.len() as u64);
        for value in expected {
            assert_eq!(receiver.next().await.unwrap().bytes()[0], value);
        }
        assert!(matches!(
            receiver.next_timeout(Duration::from_millis(1)).await,
            Err(crate::CameraError::Timeout)
        ));
        assert!(publish(&hub, epoch, 5));
        assert_eq!(receiver.next().await.unwrap().bytes()[0], 5);
    }
}

#[test]
fn variable_native_payload_reuses_capacity_and_accounts_reserved_bytes() {
    use crate::{FrameLayout, PixelFormat};
    let hub = FrameHub::new(2);
    let epoch = hub.start();
    let frame = |n| FrameLayout::packed(2, 2, PixelFormat::Mjpeg, 0, n);
    for n in [100, 120, 50, 60, 70, 80] {
        assert!(hub
            .publish_native_into(epoch, frame(n), None, |out| {
                out.fill(7);
                Ok(())
            })
            .unwrap());
        assert_eq!(hub.latest().unwrap().bytes().len(), n);
    }
    assert_eq!(hub.metrics().allocated_buffers, 2);
    assert_eq!(hub.metrics().allocated_bytes, 220);
}

#[test]
fn stop_during_conversion_cannot_publish_or_charge_the_next_epoch() {
    let hub = FrameHub::new(2);
    let epoch = hub.start();
    let worker_hub = hub.clone();
    let (entered, started) = std::sync::mpsc::channel();
    let (resume, resumed) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_hub
            .publish_rgb(epoch, 2, 2, None, |_| {
                entered.send(()).unwrap();
                resumed.recv().unwrap();
                Ok(())
            })
            .unwrap()
    });
    started.recv().unwrap();
    hub.stop();
    let next = hub.start();
    resume.send(()).unwrap();
    assert!(!worker.join().unwrap());
    assert!(hub.latest().is_none());
    assert_eq!(hub.metrics().received, 0);
    assert!(publish(&hub, next, 9));
    assert_eq!(hub.latest().unwrap().key.sequence, 1);
}
