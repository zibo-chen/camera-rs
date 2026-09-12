use crate::{
    frame::{RecoveryTrigger, FRAME_ERROR_RECOVERY_THRESHOLD},
    FrameHub, FrameKey, MemoryBudget, StreamRequest,
};
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
async fn wait_after_none_waits_for_a_frame_newer_than_the_current_snapshot() {
    let hub = Arc::new(FrameHub::new(3));
    let epoch = hub.start();
    assert!(publish(&hub, epoch, 7));

    assert!(matches!(
        hub.wait_after(None, Duration::from_millis(2)).await,
        Err(ref error) if error.kind() == crate::CameraErrorKind::Timeout
    ));

    let waiter = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.wait_after(None, Duration::from_secs(1)).await })
    };
    tokio::task::yield_now().await;
    assert!(publish(&hub, epoch, 8));
    assert_eq!(waiter.await.unwrap().unwrap().bytes()[0], 8);
}

#[tokio::test]
async fn first_frame_wait_accepts_current_session_only() {
    let hub = FrameHub::new(2);
    let old = hub.start();
    assert!(publish(&hub, old, 1));
    hub.stop();
    let current = hub.start();

    assert!(matches!(
        hub.wait_first_in(old, Duration::from_millis(2)).await,
        Err(ref error) if error.kind() == crate::CameraErrorKind::StreamStopped
    ));
    assert!(matches!(
        hub.wait_first_in(current, Duration::from_millis(2)).await,
        Err(ref error) if error.kind() == crate::CameraErrorKind::Timeout
    ));
    assert!(!publish(&hub, old, 2));
    assert!(publish(&hub, current, 3));
    assert_eq!(
        hub.wait_first_in(current, Duration::from_millis(2))
            .await
            .unwrap()
            .bytes()[0],
        3
    );
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

#[tokio::test]
async fn stopped_source_wakes_recovery_without_waiting_for_the_stall_deadline() {
    let hub = Arc::new(FrameHub::new(2));
    let epoch = hub.start();
    assert!(publish(&hub, epoch, 1));
    let waiter = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.wait_for_recovery_trigger(Duration::from_secs(30)).await })
    };
    tokio::task::yield_now().await;

    hub.stop();

    assert_eq!(
        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("backend stop must wake recovery immediately")
            .unwrap(),
        RecoveryTrigger::SourceStopped
    );
}

#[tokio::test]
async fn recovery_enabled_receiver_survives_the_disconnect_race_and_next_epoch() {
    let hub = FrameHub::new(2);
    hub.enable_recovery();
    let first_epoch = hub.start();
    assert!(publish(&hub, first_epoch, 1));
    let mut receiver = hub
        .subscribe_with(crate::SubscriptionOptions::latest())
        .unwrap();
    receiver.next().await.unwrap();

    hub.stop();
    assert!(matches!(
        receiver.next_timeout(Duration::from_millis(2)).await,
        Err(ref error) if error.kind() == crate::CameraErrorKind::Timeout
    ));

    let next_epoch = hub.start();
    assert!(publish(&hub, next_epoch, 2));
    let recovered = receiver
        .next_timeout(Duration::from_millis(20))
        .await
        .unwrap();
    assert_eq!(recovered.key.session, next_epoch);
    assert_eq!(recovered.bytes()[0], 2);

    hub.disable_recovery();
    hub.stop();
    assert!(matches!(
        receiver.next().await,
        Err(ref error) if error.kind() == crate::CameraErrorKind::StreamStopped
    ));
}

#[test]
fn a_bad_frame_burst_triggers_recovery_and_a_good_frame_clears_it() {
    let hub = FrameHub::new(2);
    let epoch = hub.start();
    assert!(publish(&hub, epoch, 1));

    for expected in 1..FRAME_ERROR_RECOVERY_THRESHOLD {
        assert!(hub
            .publish_rgb(epoch, 2, 2, None, |_| {
                Err(crate::CameraError::invalid_frame(
                    "damaged test frame".into(),
                ))
            })
            .is_err());
        assert_eq!(hub.metrics().consecutive_conversion_errors, expected);
        assert_eq!(
            hub.recovery_trigger(Duration::from_secs(30)),
            None,
            "isolated damaged frames must not restart a healthy stream"
        );
    }

    assert!(hub
        .publish_rgb(epoch, 2, 2, None, |_| {
            Err(crate::CameraError::invalid_frame(
                "damaged test frame".into(),
            ))
        })
        .is_err());
    assert_eq!(
        hub.recovery_trigger(Duration::from_secs(30)),
        Some(RecoveryTrigger::FrameErrors {
            consecutive: FRAME_ERROR_RECOVERY_THRESHOLD,
        })
    );
    let metrics = hub.metrics();
    assert_eq!(
        metrics.max_consecutive_conversion_errors,
        FRAME_ERROR_RECOVERY_THRESHOLD
    );

    assert!(publish(&hub, epoch, 2));
    assert_eq!(hub.metrics().consecutive_conversion_errors, 0);
    assert_eq!(hub.recovery_trigger(Duration::from_secs(30)), None);
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
fn source_activity_advances_when_pool_pressure_drops_a_frame() {
    let hub = FrameHub::new(2);
    let epoch = hub.start();
    assert!(publish(&hub, epoch, 1));
    let retained = hub.latest().unwrap();
    assert!(publish(&hub, epoch, 2));
    std::thread::sleep(Duration::from_millis(4));
    assert!(hub.source_activity_is_stale(Duration::from_millis(2)));

    assert!(!publish(&hub, epoch, 3));
    assert!(!hub.source_activity_is_stale(Duration::from_millis(2)));
    assert_eq!(hub.metrics().pool_drops, 1);

    drop(retained);
    assert!(publish(&hub, epoch, 4));
    assert_eq!(hub.latest().unwrap().key.session, epoch);
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
        .publish_rgb(ae, 2, 2, None, |_| Err(
            crate::CameraError::buffer_exhausted()
        ))
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
        hub.configure(&StreamRequest::builder().resolution(2, 2).build().unwrap())
            .unwrap();
        let epoch = hub.start();
        let mut receiver = hub
            .subscribe_with(crate::SubscriptionOptions {
                delivery,
                max_rate: None,
            })
            .unwrap();
        for value in 1..=4 {
            assert!(publish(&hub, epoch, value));
        }
        assert_eq!(receiver.dropped_frames(), 4 - expected.len() as u64);
        for value in expected {
            assert_eq!(receiver.next().await.unwrap().bytes()[0], value);
        }
        assert!(matches!(
            receiver.next_timeout(Duration::from_millis(1)).await,
            Err(ref error) if error.kind() == crate::CameraErrorKind::Timeout
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
fn variable_native_payload_never_grows_past_the_memory_budget() {
    use crate::{FrameLayout, PixelFormat};
    let hub = FrameHub::new(2);
    hub.configure(
        &StreamRequest::builder()
            .resolution(2, 2)
            .memory_budget(MemoryBudget {
                buffers: 2,
                bytes: 170,
            })
            .build()
            .unwrap(),
    )
    .unwrap();
    let epoch = hub.start();
    let layout = |length| FrameLayout::packed(2, 2, PixelFormat::Mjpeg, 0, length);

    assert!(hub
        .publish_native_into(epoch, layout(60), None, |out| {
            out.fill(1);
            Ok(())
        })
        .unwrap());
    assert!(hub
        .publish_native_into(epoch, layout(60), None, |out| {
            out.fill(2);
            Ok(())
        })
        .unwrap());

    assert!(hub
        .publish_native_into(epoch, layout(80), None, |out| {
            out.fill(3);
            Ok(())
        })
        .unwrap());
    assert!(hub.metrics().allocated_bytes <= 170);

    assert!(!hub
        .publish_native_into(epoch, layout(120), None, |out| {
            out.fill(9);
            Ok(())
        })
        .unwrap());
    assert_eq!(hub.metrics().pool_drops, 1);
    assert!(hub
        .publish_native_into(epoch, layout(50), None, |out| {
            out.fill(4);
            Ok(())
        })
        .unwrap());
    assert_eq!(hub.latest().unwrap().bytes()[0], 4);
    assert!(hub.metrics().allocated_bytes <= 170);
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
