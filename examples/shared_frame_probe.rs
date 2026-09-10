//! Capture a bounded number of frames; use --synthetic for hardware-free checks.
use camera::{CameraSystem, FrameRate, OutputFormat, StreamRequest};
use std::time::{Duration, Instant};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let synthetic = args.iter().any(|a| a == "--synthetic");
    let native = args.iter().any(|a| a == "--native");
    let system = if synthetic {
        CameraSystem::synthetic()
    } else {
        CameraSystem::new()
    };
    let device = system
        .devices()
        .await?
        .into_iter()
        .next()
        .ok_or("No camera available")?;
    println!("Device: {} ({})", device.name, device.id);
    let mut camera = system.open(&device.id).await?;
    let session = camera
        .start(
            StreamRequest::builder()
                .resolution(640, 480)
                .frame_rate(FrameRate::new(30, 1)?)
                .output(if native {
                    OutputFormat::Native
                } else {
                    OutputFormat::Rgb8
                })
                .startup_timeout(Duration::from_secs(10))
                .build()?,
        )
        .await?;
    println!("Negotiated: {:?}", session.negotiated_config());
    let mut frames = session.subscribe();
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
    session.stop().await?;
    Ok(())
}
