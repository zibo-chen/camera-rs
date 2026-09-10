use camera::CameraSystem;
use std::time::Duration;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut watcher = CameraSystem::new()
        .watch_devices(Duration::from_secs(1))
        .await?;
    println!("{:?}", watcher.snapshot());
    loop {
        tokio::select! {update=watcher.changed()=>println!("{:?}",update?),_=tokio::signal::ctrl_c()=>break}
    }
    watcher.stop().await?;
    Ok(())
}
