//! Capture a bounded number of frames; use --synthetic for hardware-free checks.
use camera::{CameraSystem, CaptureRequest, DeviceSelector, FrameRate, SubscriptionOptions};
use std::time::{Duration, Instant};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let synthetic = args.iter().any(|a| a == "--synthetic");
    let system = if synthetic {
        CameraSystem::synthetic()
    } else {
        CameraSystem::new()
    };
    let mut device = system.open(DeviceSelector::Default).await?;
    println!("Device: {} ({})", device.device().name, device.device().id);
    let session = device
        .start(
            CaptureRequest::builder()
                .preferred_resolution(640, 480)
                .preferred_frame_rate(FrameRate::new(30, 1)?)
                .startup_timeout(Duration::from_secs(10))
                .build()?,
        )
        .await?;
    println!("Negotiated: {:?}", session.negotiated());
    let mut frames = session.subscribe(SubscriptionOptions::latest())?;
    let begin = Instant::now();
    let mut first = None;
    for _ in 0..60 {
        let frame = frames.next_timeout(Duration::from_secs(3)).await?;
        if first.is_none() {
            println!("Frame: {:?}, {} bytes", frame.layout(), frame.bytes().len());
            first = Some(frame);
        }
    }
    println!(
        "60 frames in {:?}; metrics={:?}; subscriber_drops={}",
        begin.elapsed(),
        session.metrics(),
        frames.dropped_frames()
    );
    session.close().await?;
    Ok(())
}
