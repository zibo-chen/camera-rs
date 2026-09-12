#![cfg(feature = "runtime-tokio")]

use camera::{
    BackendDevice, BackendDeviceInfo, BackendId, BackendPolicy, BackendProvider, CameraError,
    CameraFacing, CameraResult, CameraSystem, CaptureFormat, CaptureProfile, CaptureRequest,
    DeviceCapabilities, DeviceSelector, FrameLayout, FrameRate, FrameSink, IdentityStability,
    MemoryBudget, NegotiationPriority, OverflowPolicy, PixelFormat, PlaneLayout,
    SubscriptionOptions,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[test]
fn public_contract_is_the_breaking_0_4_api() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.4.0");

    let request = CaptureRequest::builder()
        .preferred_resolution(1280, 720)
        .minimum_frame_rate(FrameRate::new(30, 1).unwrap())
        .preferred_formats([CaptureFormat::Nv12, CaptureFormat::Mjpeg])
        .require_aspect_ratio(16, 9)
        .priority(NegotiationPriority::LowCpu)
        .memory_budget(MemoryBudget {
            buffers: 8,
            bytes: 64 * 1024 * 1024,
        })
        .build()
        .unwrap();

    assert_eq!(request.preferred_resolution(), (1280, 720));
    assert_eq!(request.minimum_frame_rate().unwrap().as_f64(), 30.0);
    assert_eq!(request.preferred_formats()[0], CaptureFormat::Nv12);
    assert_eq!(request.required_aspect_ratio(), Some((16, 9)));
    assert!(request.delivers_native_frames());
}

#[test]
fn backend_policy_and_device_ids_have_unambiguous_semantics() {
    let v4l2 = BackendId::V4L2;
    let persisted = camera::DeviceId::new(v4l2.clone(), "/dev/v4l/by-id/camera-a").unwrap();
    let encoded = persisted.to_persistent_string();
    assert_eq!(camera::DeviceId::parse(&encoded).unwrap(), persisted);

    let custom = BackendId::custom("com.example", "industrial-camera").unwrap();
    assert_eq!(custom.as_str(), "com.example/industrial-camera");

    let require = CameraSystem::builder()
        .backend_policy(BackendPolicy::Require(v4l2))
        .build()
        .unwrap();
    assert!(matches!(
        require.backend_policy(),
        BackendPolicy::Require(_)
    ));
}

#[tokio::test]
async fn simple_capture_owns_the_session_and_default_receiver() {
    let system = CameraSystem::synthetic();
    let mut capture = system
        .capture(DeviceSelector::Default)
        .profile(CaptureProfile::Preview)
        .resolution(32, 24)
        .start()
        .await
        .unwrap();

    let frame = capture.next_frame().await.unwrap();
    assert_eq!((frame.layout().width, frame.layout().height), (32, 24));
    assert_eq!(capture.negotiated().capture.width, 32);
    capture.close().await.unwrap();
    assert!(matches!(
        capture.next_frame().await,
        Err(CameraError::StreamStopped)
    ));
}

#[tokio::test]
async fn subscriptions_are_independent_and_rate_limits_do_not_change_capture_rate() {
    let system = CameraSystem::synthetic();
    let mut device = system.open(DeviceSelector::Default).await.unwrap();
    let session = device
        .start(
            CaptureRequest::builder()
                .preferred_resolution(16, 16)
                .preferred_frame_rate(FrameRate::new(60, 1).unwrap())
                .memory_budget(MemoryBudget {
                    buffers: 12,
                    bytes: 1024 * 1024,
                })
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

    let mut preview = session.subscribe(SubscriptionOptions::latest()).unwrap();
    let mut recording = session
        .subscribe(SubscriptionOptions::buffered(4).overflow(OverflowPolicy::DropOldest))
        .unwrap();
    let mut inference = session
        .subscribe(SubscriptionOptions::latest().max_rate(FrameRate::new(10, 1).unwrap()))
        .unwrap();

    let p = preview.next().await.unwrap();
    let r = recording.next().await.unwrap();
    let i1 = inference.next().await.unwrap();
    let i2 = inference.next().await.unwrap();
    assert_eq!(p.key.session, r.key.session);
    assert!(i2.captured_at.duration_since(i1.captured_at) >= Duration::from_millis(80));
    assert_eq!(
        session.negotiated().capture.frame_rate,
        FrameRate::new(60, 1).unwrap()
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn planning_and_capabilities_report_conditions_instead_of_boolean_promises() {
    let system = CameraSystem::synthetic();
    let info = system.devices().await.unwrap().remove(0);
    let capabilities = system.capabilities(&info.id).await.unwrap();
    assert!(matches!(capabilities, DeviceCapabilities { .. }));
    assert!(capabilities.native_formats.contains(&CaptureFormat::Rgb8));
    assert!(capabilities.conversions.iter().all(|c| c.available));

    let request = CaptureRequest::builder()
        .preferred_resolution(640, 480)
        .preferred_frame_rate(FrameRate::new(30, 1).unwrap())
        .build()
        .unwrap();
    let plan = system
        .plan(DeviceSelector::Id(info.id), request)
        .await
        .unwrap();
    assert_eq!(plan.selected.capture.width, 640);
    assert!(!plan.ranking_reason.is_empty());
    assert!(plan.estimated_pool_bytes > 0);
}

#[tokio::test]
async fn state_watch_and_loss_reporting_events_are_distinct() {
    let system = CameraSystem::synthetic();
    let mut device = system.open(DeviceSelector::Default).await.unwrap();
    let session = device
        .start(CaptureRequest::builder().build().unwrap())
        .await
        .unwrap();
    let mut state = session.watch_state();
    let mut events = session.events();

    assert!(session.state().is_streaming());
    session.close().await.unwrap();
    state.changed().await.unwrap();
    assert!(state.current().is_terminal());
    let event = events.next().await.unwrap();
    assert!(event.is_lifecycle());
}

#[derive(Debug)]
struct TestProvider;

impl BackendProvider for TestProvider {
    fn id(&self) -> BackendId {
        BackendId::custom("com.example", "test-pattern").unwrap()
    }

    fn enumerate(&self) -> CameraResult<Vec<BackendDeviceInfo>> {
        Ok(vec![BackendDeviceInfo {
            native_id: "stable-test-device".into(),
            name: "External test camera".into(),
            description: "Integration fixture".into(),
            stability: IdentityStability::Native,
            facing: CameraFacing::External,
            usb: None,
        }])
    }

    fn open(&self, _native_id: &str) -> CameraResult<Box<dyn BackendDevice>> {
        Ok(Box::new(TestDevice {
            running: Arc::new(AtomicBool::new(false)),
        }))
    }
}

struct TestDevice {
    running: Arc<AtomicBool>,
}

impl BackendDevice for TestDevice {
    fn capabilities(&self) -> CameraResult<DeviceCapabilities> {
        Ok(DeviceCapabilities::single_native(
            CaptureFormat::Rgb8,
            4,
            2,
            FrameRate::new(30, 1).unwrap(),
        ))
    }

    fn start(&mut self, sink: FrameSink, plan: &camera::CapturePlan) -> CameraResult<()> {
        self.running.store(true, Ordering::Release);
        let running = self.running.clone();
        let config = plan.selected.capture.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            if running.load(Ordering::Acquire) {
                let layout = FrameLayout {
                    width: config.width,
                    height: config.height,
                    format: PixelFormat::Rgb8,
                    planes: vec![PlaneLayout {
                        offset: 0,
                        length: (config.width * config.height * 3) as usize,
                        row_stride: (config.width * 3) as usize,
                        pixel_stride: 3,
                    }],
                    color: Default::default(),
                    orientation: Default::default(),
                    bottom_up: false,
                };
                let mut lease = sink.writable(layout).unwrap();
                lease.bytes_mut().fill(7);
                lease.commit().unwrap();
            }
        });
        Ok(())
    }

    fn stop(&mut self) -> CameraResult<()> {
        self.running.store(false, Ordering::Release);
        Ok(())
    }
}

#[tokio::test]
async fn external_backend_registers_without_internal_module_access() {
    let backend = BackendId::custom("com.example", "test-pattern").unwrap();
    let system = CameraSystem::builder()
        .register_backend(TestProvider)
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();
    let devices = system.devices().await.unwrap();
    assert_eq!(devices.len(), 1);

    let mut device = system
        .open(DeviceSelector::Facing(CameraFacing::External))
        .await
        .unwrap();
    let session = device
        .start(
            CaptureRequest::builder()
                .preferred_resolution(4, 2)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut frames = session.subscribe(SubscriptionOptions::latest()).unwrap();
    let frame = frames.next().await.unwrap();
    assert_eq!(frame.rgb_view().unwrap().row(0).unwrap(), &[7; 12]);
    session.close().await.unwrap();
}

#[tokio::test]
async fn preferred_backends_fall_back_in_order_and_keep_device_identity_exact() {
    let custom = BackendId::custom("com.example", "test-pattern").unwrap();
    let system = CameraSystem::builder()
        .register_backend(TestProvider)
        .backend_policy(BackendPolicy::Prefer(vec![
            BackendId::CAMERA2,
            custom.clone(),
        ]))
        .build()
        .unwrap();
    let mut capture = system
        .capture(DeviceSelector::Default)
        .request(
            CaptureRequest::builder()
                .preferred_resolution(4, 2)
                .build()
                .unwrap(),
        )
        .start()
        .await
        .unwrap();
    assert_eq!(capture.session().device().id.backend(), &custom);
    assert_eq!(capture.next_frame().await.unwrap().bytes().len(), 24);
    capture.close().await.unwrap();
}

#[test]
fn invalid_backend_options_are_structured_errors() {
    let error = CameraSystem::builder()
        .backend_policy(BackendPolicy::Require(BackendId::CAMERA2))
        .backend_options(camera::V4l2Options::default().mmap_buffers(4))
        .build()
        .unwrap_err();
    assert!(matches!(error, CameraError::BackendOptionMismatch { .. }));

    let error = CameraSystem::builder()
        .backend_policy(BackendPolicy::Require(BackendId::CAMERA2))
        .backend_options(camera::Camera2Options::default().max_images(1))
        .build()
        .unwrap_err();
    assert!(matches!(error, CameraError::InvalidConfig(_)));
}
