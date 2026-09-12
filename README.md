# camera-rs

Intent-driven, cross-platform camera capture for Rust. Version 0.4 is a breaking API redesign: callers select a device, state hard constraints and preferences, then give each consumer its own bounded delivery policy. Capture always publishes immutable native-format frames; RGB conversion is an explicit consumer operation.

Rust 1.88 or newer is required.

```rust
use camera::{CameraSystem, CaptureProfile, DeviceSelector};

#[tokio::main(flavor = "current_thread")]
async fn main() -> camera::CameraResult<()> {
    let system = CameraSystem::new();
    let mut capture = system
        .capture(DeviceSelector::Default)
        .profile(CaptureProfile::Preview)
        .resolution(1280, 720)
        .start()
        .await?;

    let frame = capture.next_frame().await?;
    println!("{:?}, {} native bytes", frame.layout(), frame.bytes().len());
    println!("negotiated: {:?}", capture.negotiated());
    capture.close().await
}
```

`Capture` owns both the session and its default receiver, so a convenient call cannot accidentally drop capture lifetime. For preview, recognition and recording at the same time, open a `Device`, start one `Session`, then create independent `SubscriptionOptions` for every consumer.

## Feature model

Features decide what is compiled; `BackendPolicy` decides runtime behavior.

| Purpose | Feature |
| --- | --- |
| Platform convenience bundle | `native` |
| Capture backends | `backend-v4l2`, `backend-avfoundation`, `backend-camera2`, `backend-mf`, `backend-uvc` |
| Async integration | `runtime-tokio` |
| Persistent synchronous facade | `blocking` |
| Raw/RGB conversion kernels | `convert-rgb` |
| MJPEG decoding through TurboJPEG | `decode-mjpeg` |
| Integrations | `ndarray`, `serde`, `viewer` |

The default convenience set is `native`, `runtime-tokio`, `convert-rgb`, and `decode-mjpeg`. A backend never implies JPEG decoding. Examples:

```toml
# Linux V4L2 capture, native frames only
camera = { version = "0.4", default-features = false, features = ["backend-v4l2"] }

# Core request/format/identity contracts without Tokio
camera = { version = "0.4", default-features = false }
```

Android’s adapter defaults to Camera2 only. UVC, V4L2, RGB conversion and MJPEG decoding are opt-in adapter features.
The `serde` feature covers persisted device/backend identifiers as well as capture requests, selectors, policies, memory/recovery settings and typed backend options.

## Consumer-side conversion

Use one converter and reuse caller-owned output storage. Slow consumers only convert frames they actually receive.

```rust
use camera::{ConversionRequest, RgbConverter};

# fn convert(frame: &camera::CapturedFrame) -> camera::CameraResult<()> {
let request = ConversionRequest::new(640, 360)?;
let mut rgb = vec![0; request.output_len()?];
let mut converter = RgbConverter::new();
converter.convert_into(frame, request, &mut rgb)?;
# Ok(()) }
```

Native output means no format conversion. Platform planes or encoded payloads may still be copied into the library’s bounded immutable pool; this is not a promise of a borrowed driver buffer or GPU texture. Retained frames keep pool storage alive, and resource pressure is observable rather than overwriting bytes still held by consumers.

See [API contracts](docs/API.md), [architecture](docs/ARCHITECTURE.md), [build instructions](BUILD.md), [V4L2](docs/V4L2.md), and the [Android adapter](camera-android/README.md). Licensed under MIT OR Apache-2.0; see [third-party notices](THIRD_PARTY.md).
