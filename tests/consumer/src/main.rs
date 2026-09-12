use camera::{blocking, CaptureRequest, DeviceSelector, SubscriptionOptions};

fn main() -> camera::CameraResult<()> {
    let system = blocking::CameraSystem::synthetic();
    let mut device = system.open(DeviceSelector::Default)?;
    let session = device.start(CaptureRequest::builder().build()?)?;
    let mut frames = session.subscribe(SubscriptionOptions::latest())?;
    let _frame = frames.next_frame()?;
    session.close()
}
