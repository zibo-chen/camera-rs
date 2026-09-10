# camera-rs

Cross-platform asynchronous camera capture for Rust. The default `native` feature selects AVFoundation on macOS/iOS, Camera2 on Android, Media Foundation on Windows, and V4L2 on Linux. `backend-uvc` is opt-in USB capture on Linux, macOS and Android. Requires Rust 1.88+.

```rust
use camera::{CameraSystem, StreamRequest};
#[tokio::main(flavor = "current_thread")]
async fn main() -> camera::CameraResult<()> {
let system = CameraSystem::new();
let device = system.devices().await?.into_iter().next()
    .ok_or_else(|| camera::CameraError::DeviceNotFound("No camera".into()))?;
let mut camera = system.open(&device.id).await?;
let session = camera.start(StreamRequest::builder().resolution(1280, 720).build()?).await?;
let mut frames = session.subscribe();
let frame = frames.next().await?;
println!("{:?}, {} bytes", frame.layout(), frame.bytes().len());
session.stop().await?;
Ok(())
}
```

`CaptureSession` owns capture lifetime. `FrameReceiver` owns an independent cursor and bounded queue. Frames are immutable and shared; copying is explicit. Dropping the session requests shutdown; `stop().await` waits for native cleanup. Native driver calls already in progress cannot be forcibly cancelled.

Use `CameraSystem::synthetic()` for hardware-free development, `OutputFormat::Native` to avoid RGB conversion, `FrameRate::new(30000, 1001)` for fractional rates, and `SelectionPolicy::Exact` to reject driver adjustments. Inspect `negotiated_config()` for the actual mode. Retained frames consume the configured `MemoryBudget`; an exhausted pool drops incoming frames without overwriting retained bytes. Driver buffers are a separate V4L2 option.

Optional features: `ndarray` for zero-copy RGB views, `jpeg` for explicit MJPEG conversion, and individual `backend-*` features when default features are disabled. Android may explicitly enable `backend-v4l2` for privileged device-node access; Camera2 remains the default. `native-debug-logs` enables verbose USB diagnostics. The library does not initialize a global logger. Applications supply Tokio and camera permissions; Android USB descriptors must already be authorized.

Native planes include stride, pixel stride, timestamps and known color metadata. Unknown metadata stays unknown. `RgbConverter` converts into caller-owned storage; H264 decoding and native GPU texture leases are not provided.

Licensed under MIT OR Apache-2.0. See [third-party notices](THIRD_PARTY.md), especially when enabling UVC.

See [API and migration guide](docs/API.md), [build instructions](BUILD.md), and the [Android adapter](camera-android/README.md).
