#![cfg(all(feature = "runtime-tokio", feature = "custom-backend"))]

use camera::{
    BackendDevice, BackendDeviceInfo, BackendId, BackendPolicy, BackendProvider, CameraError,
    CameraFacing, CameraResult, CameraSystem, CaptureFormat, CaptureProfile, CaptureRequest,
    DeviceCapabilities, DeviceSelector, FrameLayout, FrameRate, FrameSink, IdentityStability,
    MemoryBudget, NegotiationPriority, OverflowPolicy, PixelFormat, PlaneLayout,
    SubscriptionOptions,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    thread::JoinHandle,
    time::Duration,
};

#[test]
fn public_contract_is_the_breaking_0_5_api() {
    assert!(env!("CARGO_PKG_VERSION").starts_with("0.5."));

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
        Err(ref error) if error.kind() == camera::CameraErrorKind::StreamStopped
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
async fn an_open_device_can_report_its_own_capabilities_before_streaming() {
    let system = CameraSystem::synthetic();
    let device = system.open(DeviceSelector::Default).await.unwrap();

    let capabilities = device.capabilities().await.unwrap();

    assert!(capabilities.native_formats.contains(&CaptureFormat::Rgb8));
    assert!(matches!(
        capabilities.capture_modes,
        camera::CapabilityKnowledge::Known(ref modes)
            if modes.iter().any(|mode| mode.width == 640 && mode.height == 480)
    ));
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
        let config = plan.selected.capture.clone();
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
        let mut lease = sink.writable(layout)?;
        lease.bytes_mut().fill(7);
        lease.commit()?;
        Ok(())
    }

    fn stop(&mut self) -> CameraResult<()> {
        self.running.store(false, Ordering::Release);
        Ok(())
    }
}

#[test]
fn persisted_device_id_rejects_malformed_unicode_without_panicking() {
    let result = std::panic::catch_unwind(|| camera::DeviceId::parse("synthetic|中a"));
    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
    assert!(camera::DeviceId::parse(&format!("synthetic|{}", "00".repeat(4097))).is_err());

    let original = camera::DeviceId::new(BackendId::SYNTHETIC, "摄像头-📷").unwrap();
    assert_eq!(
        camera::DeviceId::parse(&original.to_persistent_string()).unwrap(),
        original
    );
}

#[derive(Debug)]
struct MultiModeProvider;

impl BackendProvider for MultiModeProvider {
    fn id(&self) -> BackendId {
        BackendId::custom("com.example", "multi-mode").unwrap()
    }

    fn enumerate(&self) -> CameraResult<Vec<BackendDeviceInfo>> {
        Ok(vec![BackendDeviceInfo {
            native_id: "multi".into(),
            name: "Multi-mode test camera".into(),
            description: String::new(),
            stability: IdentityStability::Native,
            facing: CameraFacing::External,
            usb: None,
        }])
    }

    fn open(&self, _native_id: &str) -> CameraResult<Box<dyn BackendDevice>> {
        Ok(Box::new(MultiModeDevice))
    }
}

struct MultiModeDevice;

impl BackendDevice for MultiModeDevice {
    fn capabilities(&self) -> CameraResult<DeviceCapabilities> {
        DeviceCapabilities::from_modes([
            camera::CaptureMode {
                format: CaptureFormat::Nv12,
                width: 640,
                height: 480,
                frame_rate: FrameRate::new(15, 1).unwrap(),
            },
            camera::CaptureMode {
                format: CaptureFormat::Rgb8,
                width: 1280,
                height: 720,
                frame_rate: FrameRate::new(15, 1).unwrap(),
            },
        ])
    }

    fn start(&mut self, sink: FrameSink, plan: &camera::CapturePlan) -> CameraResult<()> {
        let selected = &plan.selected.capture;
        let (format, length, row_stride, pixel_stride) = match selected.format {
            CaptureFormat::Nv12 => (
                PixelFormat::Nv12,
                (selected.width * selected.height * 3 / 2) as usize,
                selected.width as usize,
                1,
            ),
            CaptureFormat::Rgb8 => (
                PixelFormat::Rgb8,
                (selected.width * selected.height * 3) as usize,
                (selected.width * 3) as usize,
                3,
            ),
            other => panic!("unexpected test format: {other:?}"),
        };
        let layout = FrameLayout {
            width: selected.width,
            height: selected.height,
            format,
            planes: vec![PlaneLayout {
                offset: 0,
                length,
                row_stride,
                pixel_stride,
            }],
            color: Default::default(),
            orientation: Default::default(),
            bottom_up: false,
        };
        sink.publish_bytes(layout, None, &vec![1; length])?;
        Ok(())
    }

    fn stop(&mut self) -> CameraResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn public_multi_mode_capabilities_drive_plan_and_start_constraints() {
    let backend = BackendId::custom("com.example", "multi-mode").unwrap();
    let system = CameraSystem::builder()
        .register_backend(MultiModeProvider)
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();
    let request = CaptureRequest::builder()
        .exact_resolution(1280, 720)
        .preferred_frame_rate(FrameRate::new(30, 1).unwrap())
        .minimum_frame_rate(FrameRate::new(15, 1).unwrap())
        .preferred_formats([CaptureFormat::Nv12, CaptureFormat::Rgb8])
        .build()
        .unwrap();
    let plan = system
        .plan(DeviceSelector::Default, request.clone())
        .await
        .unwrap();
    assert_eq!(
        plan.selected.capture.frame_rate,
        FrameRate::new(15, 1).unwrap()
    );
    assert_eq!(plan.selected.capture.format, CaptureFormat::Rgb8);

    let mut camera = system.open(DeviceSelector::Default).await.unwrap();
    let session = camera.start(request).await.unwrap();
    assert_eq!(
        session.negotiated().capture.frame_rate,
        FrameRate::new(15, 1).unwrap()
    );
    session.close().await.unwrap();
}

#[derive(Debug, Default)]
struct SlowControlProvider {
    dropped_devices: Option<Arc<AtomicUsize>>,
}

impl BackendProvider for SlowControlProvider {
    fn id(&self) -> BackendId {
        BackendId::custom("com.example", "slow-control").unwrap()
    }

    fn enumerate(&self) -> CameraResult<Vec<BackendDeviceInfo>> {
        Ok(vec![BackendDeviceInfo {
            native_id: "slow".into(),
            name: "Slow control camera".into(),
            description: String::new(),
            stability: IdentityStability::Native,
            facing: CameraFacing::External,
            usb: None,
        }])
    }

    fn open(&self, _native_id: &str) -> CameraResult<Box<dyn BackendDevice>> {
        std::thread::sleep(Duration::from_millis(80));
        Ok(Box::new(SlowControlDevice {
            dropped_devices: self.dropped_devices.clone(),
        }))
    }
}

struct SlowControlDevice {
    dropped_devices: Option<Arc<AtomicUsize>>,
}

impl Drop for SlowControlDevice {
    fn drop(&mut self) {
        if let Some(dropped) = &self.dropped_devices {
            dropped.fetch_add(1, Ordering::Release);
        }
    }
}

impl BackendDevice for SlowControlDevice {
    fn capabilities(&self) -> CameraResult<DeviceCapabilities> {
        std::thread::sleep(Duration::from_millis(80));
        Ok(DeviceCapabilities::single_native(
            CaptureFormat::Rgb8,
            4,
            2,
            FrameRate::new(30, 1).unwrap(),
        ))
    }

    fn start(&mut self, sink: FrameSink, plan: &camera::CapturePlan) -> CameraResult<()> {
        let selected = &plan.selected.capture;
        let length = (selected.width * selected.height * 3) as usize;
        sink.publish_bytes(
            FrameLayout {
                width: selected.width,
                height: selected.height,
                format: PixelFormat::Rgb8,
                planes: vec![PlaneLayout {
                    offset: 0,
                    length,
                    row_stride: (selected.width * 3) as usize,
                    pixel_stride: 3,
                }],
                color: Default::default(),
                orientation: Default::default(),
                bottom_up: false,
            },
            None,
            &vec![3; length],
        )?;
        Ok(())
    }

    fn stop(&mut self) -> CameraResult<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn external_backend_control_calls_do_not_block_the_async_runtime() {
    let backend = BackendId::custom("com.example", "slow-control").unwrap();
    let system = CameraSystem::builder()
        .register_backend(SlowControlProvider::default())
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();

    let open_task = tokio::spawn(async move { system.open(DeviceSelector::Default).await });
    let timer_started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(timer_started.elapsed() < Duration::from_millis(40));
    let mut device = open_task.await.unwrap().unwrap();

    let start_task = tokio::spawn(async move {
        let session = device
            .start(
                CaptureRequest::builder()
                    .preferred_resolution(4, 2)
                    .build()
                    .unwrap(),
            )
            .await?;
        session.close().await
    });
    let timer_started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(timer_started.elapsed() < Duration::from_millis(40));
    start_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn startup_timeout_covers_external_capability_queries() {
    let backend = BackendId::custom("com.example", "slow-control").unwrap();
    let system = CameraSystem::builder()
        .register_backend(SlowControlProvider::default())
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();
    let mut device = system.open(DeviceSelector::Default).await.unwrap();
    let result = device
        .start(
            CaptureRequest::builder()
                .preferred_resolution(4, 2)
                .startup_timeout(Duration::from_millis(20))
                .build()
                .unwrap(),
        )
        .await;
    let error = result.unwrap_err();
    assert_eq!(error.kind(), camera::CameraErrorKind::Timeout);
    assert_eq!(error.stage(), Some(camera::OperationStage::Startup));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let session = device
        .start(
            CaptureRequest::builder()
                .preferred_resolution(4, 2)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn capture_builder_startup_timeout_is_one_total_deadline() {
    let backend = BackendId::custom("com.example", "slow-control").unwrap();
    let system = CameraSystem::builder()
        .register_backend(SlowControlProvider::default())
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();
    let result = system
        .capture(DeviceSelector::Default)
        .request(
            CaptureRequest::builder()
                .preferred_resolution(4, 2)
                .startup_timeout(Duration::from_millis(120))
                .build()
                .unwrap(),
        )
        .start()
        .await;

    assert!(matches!(result, Err(ref error) if error.kind() == camera::CameraErrorKind::Timeout));
}

#[derive(Debug)]
struct DelayedOpenProvider {
    id: BackendId,
    delay: Duration,
    fail: bool,
}

impl BackendProvider for DelayedOpenProvider {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn enumerate(&self) -> CameraResult<Vec<BackendDeviceInfo>> {
        Ok(vec![BackendDeviceInfo {
            native_id: "delayed".into(),
            name: "Delayed camera".into(),
            description: String::new(),
            stability: IdentityStability::Native,
            facing: CameraFacing::External,
            usb: None,
        }])
    }

    fn open(&self, _native_id: &str) -> CameraResult<Box<dyn BackendDevice>> {
        std::thread::sleep(self.delay);
        if self.fail {
            Err(CameraError::device_open_failed("fixture failure".into()))
        } else {
            Ok(Box::new(TestDevice {
                running: Arc::new(AtomicBool::new(false)),
            }))
        }
    }
}

#[tokio::test]
async fn preferred_backend_attempts_share_the_startup_deadline() {
    let first = BackendId::custom("com.example", "delayed-first").unwrap();
    let second = BackendId::custom("com.example", "delayed-second").unwrap();
    let system = CameraSystem::builder()
        .register_backend(DelayedOpenProvider {
            id: first.clone(),
            delay: Duration::from_millis(45),
            fail: true,
        })
        .register_backend(DelayedOpenProvider {
            id: second.clone(),
            delay: Duration::from_millis(45),
            fail: false,
        })
        .backend_policy(BackendPolicy::Prefer(vec![first, second]))
        .build()
        .unwrap();
    let result = system
        .capture(DeviceSelector::Default)
        .request(
            CaptureRequest::builder()
                .preferred_resolution(4, 2)
                .startup_timeout(Duration::from_millis(70))
                .build()
                .unwrap(),
        )
        .start()
        .await;

    assert!(
        matches!(result, Err(ref error) if error.kind() == camera::CameraErrorKind::MultipleBackendsFailed)
    );
}

#[tokio::test]
async fn external_open_and_capability_queries_have_explicit_timeouts() {
    let backend = BackendId::custom("com.example", "slow-control").unwrap();
    let dropped_devices = Arc::new(AtomicUsize::new(0));
    let system = CameraSystem::builder()
        .register_backend(SlowControlProvider {
            dropped_devices: Some(dropped_devices.clone()),
        })
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();
    let id = system.devices().await.unwrap().remove(0).id;

    let error = system
        .open_timeout(DeviceSelector::Default, Duration::from_millis(20))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), camera::CameraErrorKind::Timeout);
    assert_eq!(error.stage(), Some(camera::OperationStage::Open));
    tokio::time::timeout(Duration::from_secs(1), async {
        while dropped_devices.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let error = system
        .capabilities_timeout(&id, Duration::from_millis(20))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), camera::CameraErrorKind::Timeout);
    assert_eq!(error.stage(), Some(camera::OperationStage::Capabilities));
}

#[derive(Debug)]
struct ActivityProvider {
    emitting: Arc<AtomicBool>,
    disconnecting: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
}

impl BackendProvider for ActivityProvider {
    fn id(&self) -> BackendId {
        BackendId::custom("com.example", "activity").unwrap()
    }

    fn enumerate(&self) -> CameraResult<Vec<BackendDeviceInfo>> {
        Ok(vec![BackendDeviceInfo {
            native_id: "activity-device".into(),
            name: "Activity camera".into(),
            description: String::new(),
            stability: IdentityStability::Native,
            facing: CameraFacing::External,
            usb: None,
        }])
    }

    fn open(&self, _native_id: &str) -> CameraResult<Box<dyn BackendDevice>> {
        Ok(Box::new(ActivityDevice {
            emitting: self.emitting.clone(),
            disconnecting: self.disconnecting.clone(),
            starts: self.starts.clone(),
            running: Arc::new(AtomicBool::new(false)),
            worker: None,
        }))
    }
}

struct ActivityDevice {
    emitting: Arc<AtomicBool>,
    disconnecting: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
    running: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl BackendDevice for ActivityDevice {
    fn capabilities(&self) -> CameraResult<DeviceCapabilities> {
        Ok(DeviceCapabilities::single_native(
            CaptureFormat::Rgb8,
            2,
            1,
            FrameRate::new(30, 1).unwrap(),
        ))
    }

    fn start(&mut self, sink: FrameSink, _plan: &camera::CapturePlan) -> CameraResult<()> {
        self.stop()?;
        self.starts.fetch_add(1, Ordering::AcqRel);
        self.running.store(true, Ordering::Release);
        let running = self.running.clone();
        let emitting = self.emitting.clone();
        let disconnecting = self.disconnecting.clone();
        self.worker = Some(std::thread::spawn(move || {
            let layout = FrameLayout {
                width: 2,
                height: 1,
                format: PixelFormat::Rgb8,
                planes: vec![PlaneLayout {
                    offset: 0,
                    length: 6,
                    row_stride: 6,
                    pixel_stride: 3,
                }],
                color: Default::default(),
                orientation: Default::default(),
                bottom_up: false,
            };
            while running.load(Ordering::Acquire) {
                if disconnecting.swap(false, Ordering::AcqRel) {
                    sink.disconnect();
                    break;
                }
                if emitting.load(Ordering::Acquire) {
                    let _ = sink.publish_bytes(layout.clone(), None, &[9; 6]);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }));
        Ok(())
    }

    fn stop(&mut self) -> CameraResult<()> {
        self.running.store(false, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
        Ok(())
    }
}

#[tokio::test]
async fn pool_pressure_is_activity_but_a_true_source_stall_recovers() {
    let stall_timeout = Duration::from_millis(100);
    let emitting = Arc::new(AtomicBool::new(true));
    let disconnecting = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let backend = BackendId::custom("com.example", "activity").unwrap();
    let system = CameraSystem::builder()
        .register_backend(ActivityProvider {
            emitting: emitting.clone(),
            disconnecting,
            starts: starts.clone(),
        })
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();
    let mut device = system.open(DeviceSelector::Default).await.unwrap();
    let session = device
        .start(
            CaptureRequest::builder()
                .exact_resolution(2, 1)
                .preferred_formats([CaptureFormat::Rgb8])
                .memory_budget(MemoryBudget {
                    buffers: 2,
                    bytes: 1024,
                })
                .reconnect(camera::ReconnectPolicy {
                    max_attempts: Some(3),
                    delay: Duration::from_millis(3),
                    stall_timeout,
                })
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut frames = session.subscribe(SubscriptionOptions::latest()).unwrap();
    let first = frames
        .next_timeout(Duration::from_millis(30))
        .await
        .unwrap();
    let second = frames
        .next_timeout(Duration::from_millis(30))
        .await
        .unwrap();
    let capture_session = first.key.session;

    tokio::time::timeout(Duration::from_millis(500), async {
        while session.metrics().pool_drops == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(stall_timeout.saturating_mul(2)).await;
    assert!(session.metrics().pool_drops > 0);
    assert!(session.state().is_streaming());
    assert_eq!(starts.load(Ordering::Acquire), 1);

    drop(first);
    let resumed = frames
        .next_timeout(Duration::from_millis(30))
        .await
        .unwrap();
    assert_eq!(resumed.key.session, capture_session);
    drop((second, resumed));

    emitting.store(false, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !matches!(session.state(), camera::SessionState::Recovering { .. })
            || starts.load(Ordering::Acquire) < 2
        {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn an_explicit_backend_disconnect_starts_recovery_without_stall_timeout_delay() {
    let emitting = Arc::new(AtomicBool::new(true));
    let disconnecting = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let backend = BackendId::custom("com.example", "activity").unwrap();
    let system = CameraSystem::builder()
        .register_backend(ActivityProvider {
            emitting,
            disconnecting: disconnecting.clone(),
            starts: starts.clone(),
        })
        .backend_policy(BackendPolicy::Require(backend))
        .build()
        .unwrap();
    let mut device = system.open(DeviceSelector::Default).await.unwrap();
    let session = device
        .start(
            CaptureRequest::builder()
                .exact_resolution(2, 1)
                .preferred_formats([CaptureFormat::Rgb8])
                .reconnect(camera::ReconnectPolicy {
                    max_attempts: Some(3),
                    delay: Duration::from_secs(1),
                    stall_timeout: Duration::from_secs(5),
                })
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut frames = session.subscribe(SubscriptionOptions::latest()).unwrap();
    frames
        .next_timeout(Duration::from_millis(100))
        .await
        .unwrap();

    disconnecting.store(true, Ordering::Release);

    tokio::time::timeout(Duration::from_millis(500), async {
        while starts.load(Ordering::Acquire) < 2 || !session.state().is_streaming() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("disconnect signal should bypass the five-second stall timeout");
    frames
        .next_timeout(Duration::from_millis(100))
        .await
        .expect("the original receiver must continue on the recovered epoch");
    session.close().await.unwrap();
}

#[derive(Debug)]
struct FormatFallbackProvider {
    attempted: Arc<StdMutex<Vec<CaptureFormat>>>,
}

impl BackendProvider for FormatFallbackProvider {
    fn id(&self) -> BackendId {
        BackendId::custom("com.example", "format-fallback").unwrap()
    }

    fn enumerate(&self) -> CameraResult<Vec<BackendDeviceInfo>> {
        Ok(vec![BackendDeviceInfo {
            native_id: "same-handle".into(),
            name: "Format fallback camera".into(),
            description: String::new(),
            stability: IdentityStability::Native,
            facing: CameraFacing::External,
            usb: None,
        }])
    }

    fn open(&self, _native_id: &str) -> CameraResult<Box<dyn BackendDevice>> {
        Ok(Box::new(FormatFallbackDevice {
            attempted: self.attempted.clone(),
        }))
    }
}

struct FormatFallbackDevice {
    attempted: Arc<StdMutex<Vec<CaptureFormat>>>,
}

impl BackendDevice for FormatFallbackDevice {
    fn capabilities(&self) -> CameraResult<DeviceCapabilities> {
        DeviceCapabilities::from_modes([
            camera::CaptureMode {
                format: CaptureFormat::Mjpeg,
                width: 4,
                height: 2,
                frame_rate: FrameRate::new(30, 1).unwrap(),
            },
            camera::CaptureMode {
                format: CaptureFormat::Yuyv,
                width: 4,
                height: 2,
                frame_rate: FrameRate::new(30, 1).unwrap(),
            },
        ])
    }

    fn start(&mut self, sink: FrameSink, plan: &camera::CapturePlan) -> CameraResult<()> {
        let format = plan.selected.capture.format;
        self.attempted.lock().unwrap().push(format);
        if format == CaptureFormat::Mjpeg {
            return Err(CameraError::unsupported_format(
                "fixture rejects MJPEG".into(),
            ));
        }
        sink.publish_bytes(
            FrameLayout {
                width: 4,
                height: 2,
                format: PixelFormat::Yuyv,
                planes: vec![PlaneLayout {
                    offset: 0,
                    length: 16,
                    row_stride: 8,
                    pixel_stride: 2,
                }],
                color: Default::default(),
                orientation: Default::default(),
                bottom_up: false,
            },
            None,
            &[4; 16],
        )?;
        Ok(())
    }

    fn stop(&mut self) -> CameraResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn format_fallback_reuses_the_selected_device_identity() {
    let attempted = Arc::new(StdMutex::new(Vec::new()));
    let backend = BackendId::custom("com.example", "format-fallback").unwrap();
    let system = CameraSystem::builder()
        .register_backend(FormatFallbackProvider {
            attempted: attempted.clone(),
        })
        .backend_policy(BackendPolicy::Require(backend.clone()))
        .build()
        .unwrap();
    let mut device = system.open(DeviceSelector::Default).await.unwrap();
    let selected_id = device.device().id.clone();
    let session = device
        .start(
            CaptureRequest::builder()
                .exact_resolution(4, 2)
                .preferred_formats([CaptureFormat::Mjpeg, CaptureFormat::Yuyv])
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(session.device().id, selected_id);
    assert_eq!(session.device().id.backend(), &backend);
    assert_eq!(
        *attempted.lock().unwrap(),
        vec![CaptureFormat::Mjpeg, CaptureFormat::Yuyv]
    );
    session.close().await.unwrap();
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
    assert_eq!(error.kind(), camera::CameraErrorKind::BackendOptionMismatch);

    let error = CameraSystem::builder()
        .backend_policy(BackendPolicy::Require(BackendId::CAMERA2))
        .backend_options(camera::Camera2Options::default().max_images(1))
        .build()
        .unwrap_err();
    assert_eq!(error.kind(), camera::CameraErrorKind::InvalidArgument);
}
