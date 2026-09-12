//! Physical-device diagnostic. Android V4L2 requires access to /dev/videoN.
use camera::{
    BackendId, BackendPolicy, CameraError, CameraSystem, CaptureFormat, CaptureRequest,
    DeviceSelector, FrameRate, SubscriptionOptions, V4l2Options,
};
use std::time::{Duration, Instant};

fn option(name: &str) -> Option<String> {
    let args: Vec<_> = std::env::args().collect();
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}
fn number(name: &str, default: u32) -> u32 {
    option(name).map_or(default, |v| v.parse().expect("integer argument"))
}
fn flag(name: &str) -> bool {
    std::env::args().any(|v| v == name)
}
fn fds() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = match option("--backend").as_deref().unwrap_or("auto") {
        "camera2" => Some(BackendId::CAMERA2),
        "uvc" => Some(BackendId::UVC),
        "v4l2" => Some(BackendId::V4L2),
        "auto" => None,
        _ => return Err("unknown backend".into()),
    };
    let mut system = CameraSystem::builder().backend_policy(match backend.clone() {
        Some(backend) => BackendPolicy::Require(backend),
        None => BackendPolicy::PlatformDefault,
    });
    if backend.as_ref() == Some(&BackendId::V4L2) {
        system = system.backend_options(
            V4l2Options::default().mmap_buffers(number("--driver-buffers", 4) as usize),
        );
    }
    let system = system.build()?;
    let devices = system.devices().await?;
    for device in &devices {
        println!("DEVICE {device:?}");
        if flag("--list") {
            println!("CAPABILITIES {:?}", system.capabilities(&device.id).await);
        }
    }
    if flag("--list") {
        return Ok(());
    }
    let selected = option("--device");
    let device = devices
        .iter()
        .find(|d| selected.as_ref().is_none_or(|id| d.id.native_id() == id))
        .ok_or("requested camera missing; run --list")?;
    let mut camera = system.open(DeviceSelector::Id(device.id.clone())).await?;
    let (width, height, fps) = (
        number("--width", 640),
        number("--height", 480),
        number("--fps", 30),
    );
    let frames = number("--frames", 120);
    let mut last_epoch = None;
    let mut warm_fds = None;
    for round in 0..number("--rounds", 3) {
        let mut request = CaptureRequest::builder()
            .preferred_resolution(width, height)
            .preferred_frame_rate(FrameRate::new(fps, 1)?)
            .startup_timeout(Duration::from_secs(10));
        if let Some(format) = option("--format") {
            request = request.preferred_formats([match format.as_str() {
                "mjpeg" => CaptureFormat::Mjpeg,
                "yuyv" => CaptureFormat::Yuyv,
                "nv12" => CaptureFormat::Nv12,
                "rgb" => CaptureFormat::Rgb8,
                _ => return Err("unknown capture format".into()),
            }]);
        }
        let session = camera.start(request.build()?).await?;
        println!("ROUND {round} NEGOTIATED {:?}", session.negotiated());
        let mut receiver = session.subscribe(SubscriptionOptions::latest())?;
        let mut observer = session.subscribe(SubscriptionOptions::latest())?;
        let mut previous = None;
        let begin = Instant::now();
        for index in 0..frames {
            let frame = receiver.next_timeout(Duration::from_secs(3)).await?;
            assert_ne!(previous, Some(frame.key));
            previous = Some(frame.key);
            if index == 0 {
                assert_ne!(last_epoch, Some(frame.key.session));
                last_epoch = Some(frame.key.session);
                println!("FRAME {:?} bytes={}", frame.layout(), frame.bytes().len());
            }
            if index.is_multiple_of(10) {
                observer.next_timeout(Duration::from_secs(3)).await?;
            }
            if let Some(delay) = option("--delay-ms") {
                tokio::time::sleep(Duration::from_millis(delay.parse()?)).await;
            }
        }
        let metrics = session.metrics();
        println!("RESULT round={round} frames={frames} elapsed={:?} metrics={metrics:?} subscriber_drops={} observer_drops={}",
            begin.elapsed(),receiver.dropped_frames(),observer.dropped_frames());
        assert_eq!(metrics.conversion_errors, 0);
        assert!(metrics.allocated_bytes <= 192 * 1024 * 1024);
        for control in session.controls().await? {
            println!(
                "CONTROL {:?} readback={:?}",
                control,
                session.control(control.id).await
            );
        }
        let stop = Instant::now();
        session.close().await?;
        session.close().await?;
        assert!(matches!(
            receiver.next().await,
            Err(CameraError::StreamStopped)
        ));
        let count = fds();
        if let Some(warm) = warm_fds {
            assert!(count <= warm + 4, "descriptor growth after restart");
        } else {
            warm_fds = Some(count);
        }
        println!("STOP elapsed={:?} fds={count}", stop.elapsed());
    }
    drop(camera);
    println!("PASS backend={backend:?} final_fds={}", fds());
    Ok(())
}
