//! Physical-device diagnostic. Android V4L2 requires access to /dev/videoN.
use camera::{
    BackendType, CameraError, CameraSystem, FrameRate, OutputFormat, StreamRequest, VideoFormat,
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
        "camera2" => BackendType::Camera2,
        "uvc" => BackendType::Uvc,
        "v4l2" => BackendType::V4l2,
        "auto" => BackendType::Auto,
        _ => return Err("unknown backend".into()),
    };
    let system = CameraSystem::with_backend(backend);
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
    let mut camera = system.open(&device.id).await?;
    let (width, height, fps) = (
        number("--width", 640),
        number("--height", 480),
        number("--fps", 30),
    );
    let frames = number("--frames", 120);
    let mut last_epoch = None;
    let mut warm_fds = None;
    for round in 0..number("--rounds", 3) {
        let mut request = StreamRequest::builder()
            .resolution(width, height)
            .frame_rate(FrameRate::new(fps, 1)?)
            .output(if flag("--native") {
                OutputFormat::Native
            } else {
                OutputFormat::Rgb8
            })
            .startup_timeout(Duration::from_secs(10));
        if let Some(format) = option("--format") {
            request = request.capture_format(match format.as_str() {
                "mjpeg" => VideoFormat::MJPEG,
                "yuyv" => VideoFormat::YUYV,
                "nv12" => VideoFormat::NV12,
                "rgb" => VideoFormat::RGB,
                _ => return Err("unknown capture format".into()),
            });
        }
        if backend == BackendType::V4l2 {
            request = request.driver_buffers(number("--driver-buffers", 4) as usize);
        }
        let session = camera.start(request.build()?).await?;
        println!("ROUND {round} NEGOTIATED {:?}", session.negotiated_config());
        let mut receiver = session.subscribe();
        let mut observer = session.subscribe();
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
        session.stop().await?;
        session.stop().await?;
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
