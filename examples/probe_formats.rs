use camera::CameraSystem;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for backend in CameraSystem::available_backends() {
        let system = CameraSystem::with_backend(backend);
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
