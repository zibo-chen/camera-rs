use camera::{CameraSystem, Frame, StreamRequest};
use eframe::egui;
struct Viewer {
    latest: std::sync::mpsc::Receiver<Frame>,
    texture: Option<egui::TextureHandle>,
}
impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut latest = None;
        while let Ok(frame) = self.latest.try_recv() {
            latest = Some(frame);
        }
        if let Some(frame) = latest {
            let image = egui::ColorImage::from_rgb(
                [
                    frame.layout().width as usize,
                    frame.layout().height as usize,
                ],
                frame.bytes(),
            );
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture =
                    Some(ctx.load_texture("camera", image, egui::TextureOptions::LINEAR));
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(texture) = &self.texture {
                ui.add(egui::Image::new(texture).max_size(ui.available_size()));
            } else {
                ui.label("Waiting for camera…");
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}
fn main() -> eframe::Result<()> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result: camera::CameraResult<()> = rt.block_on(async {
            let system = CameraSystem::new();
            let device = system
                .devices()
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| camera::CameraError::DeviceNotFound("No camera".into()))?;
            let mut camera = system.open(&device.id).await?;
            let session = camera.start(StreamRequest::builder().build()?).await?;
            let mut frames = session.subscribe();
            while let Ok(frame) = frames.next().await {
                match tx.try_send(frame) {
                    Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
                }
            }
            session.stop().await
        });
        if let Err(e) = result {
            eprintln!("Camera: {e}");
        }
    });
    eframe::run_native(
        "Camera",
        eframe::NativeOptions::default(),
        Box::new(|_| {
            Ok(Box::new(Viewer {
                latest: rx,
                texture: None,
            }))
        }),
    )
}
