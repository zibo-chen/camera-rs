use camera::{
    CameraSystem, CaptureRequest, ConversionRequest, DeviceSelector, RgbConverter,
    SubscriptionOptions,
};
use eframe::egui;
struct RgbImage {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}
struct Viewer {
    latest: std::sync::Arc<std::sync::Mutex<Option<RgbImage>>>,
    texture: Option<egui::TextureHandle>,
}
impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(frame) = self.latest.lock().unwrap().take() {
            let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.pixels);
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
    let latest = std::sync::Arc::new(std::sync::Mutex::new(None));
    let publisher = latest.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result: camera::CameraResult<()> = rt.block_on(async {
            let system = CameraSystem::new();
            let mut device = system.open(DeviceSelector::Default).await?;
            let session = device.start(CaptureRequest::builder().build()?).await?;
            let mut frames = session.subscribe(SubscriptionOptions::latest())?;
            let mut converter = RgbConverter::new();
            let mut pixels = Vec::new();
            while let Ok(frame) = frames.next().await {
                let request = ConversionRequest::new(frame.layout().width, frame.layout().height)?;
                pixels.resize(request.output_len()?, 0);
                converter.convert_into(&frame, request, &mut pixels)?;
                let image = RgbImage {
                    width: frame.layout().width as usize,
                    height: frame.layout().height as usize,
                    pixels,
                };
                pixels = publisher
                    .lock()
                    .unwrap()
                    .replace(image)
                    .map(|old| old.pixels)
                    .unwrap_or_default();
            }
            session.close().await
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
                latest,
                texture: None,
            }))
        }),
    )
}
