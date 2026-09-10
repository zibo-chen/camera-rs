use camera::{CameraSystem, ReconnectPolicy, StreamRequest};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = CameraSystem::new();
    let device = system
        .devices()
        .await?
        .into_iter()
        .next()
        .ok_or("No camera")?;
    let mut camera = system.open(&device.id).await?;
    let session = camera
        .start(
            StreamRequest::builder()
                .reconnect(ReconnectPolicy::default())
                .build()?,
        )
        .await?;
    let mut frames = session.subscribe();
    let mut events = session.events();
    loop {
        tokio::select! {frame=frames.next()=>{let frame=frame?;println!("{:?}",frame.key);},event=events.changed()=>{event?;println!("{:?}",*events.borrow_and_update());},_=tokio::signal::ctrl_c()=>break}
    }
    session.stop().await?;
    Ok(())
}
