use camera::{CameraSystem, CaptureRequest, DeviceSelector, ReconnectPolicy, SubscriptionOptions};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = CameraSystem::new();
    let mut device = system.open(DeviceSelector::Default).await?;
    let session = device
        .start(
            CaptureRequest::builder()
                .reconnect(ReconnectPolicy::default())
                .build()?,
        )
        .await?;
    let mut frames = session.subscribe(SubscriptionOptions::latest())?;
    let mut events = session.events();
    loop {
        tokio::select! {frame=frames.next()=>{let frame=frame?;println!("{:?}",frame.key);},event=events.next()=>{println!("{:?}",event?);},_=tokio::signal::ctrl_c()=>break}
    }
    session.close().await?;
    Ok(())
}
