//! Measure real device enumeration/open/start/first-delivered-frame separately.
//! No camera is reported explicitly and is never counted as a successful sample.
use camera::{CameraSystem, CaptureRequest, DeviceSelector, SubscriptionOptions};
use std::time::{Duration, Instant};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let system = CameraSystem::new();
    let devices = system.devices().await?;
    let enumerate_ms = started.elapsed().as_millis();
    if devices.is_empty() {
        println!("{{\"status\":\"no_camera\",\"enumerate_ms\":{enumerate_ms}}}");
        return Ok(());
    }
    let mut device = system.open(DeviceSelector::Default).await?;
    let open_ms = started.elapsed().as_millis();
    let session = device.start(CaptureRequest::builder().build()?).await?;
    let ready_ms = started.elapsed().as_millis();
    let mut frames = session.subscribe(SubscriptionOptions::latest())?;
    let frame = tokio::time::timeout(Duration::from_secs(10), frames.next()).await??;
    drop(frame);
    println!("{{\"status\":\"first_frame\",\"enumerate_ms\":{enumerate_ms},\"open_since_launch_ms\":{open_ms},\"ready_since_launch_ms\":{ready_ms},\"first_frame_since_launch_ms\":{}}}", started.elapsed().as_millis());
    session.close().await?;
    Ok(())
}
