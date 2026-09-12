use camera::{BackendPolicy, CameraSystem};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let inventory = CameraSystem::new();
    for backend in inventory.available_backends() {
        let system = CameraSystem::builder()
            .backend_policy(BackendPolicy::Require(backend.clone()))
            .build()?;
        match system.devices().await {
            Ok(devices) => {
                for device in devices {
                    println!(
                        "{} {} {:?}",
                        device.id,
                        device.name,
                        system.capabilities(&device.id).await
                    );
                }
            }
            Err(error) => eprintln!("{backend}: {error}"),
        }
    }
    Ok(())
}
