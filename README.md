# camera-rs

Intent-driven, cross-platform camera capture for Rust. Version 0.5 adds one structured, backend-neutral error contract on top of the 0.4 capture API. Callers select a device, state hard constraints and preferences, then give each consumer its own bounded delivery policy. Capture always publishes immutable native-format frames; RGB conversion is an explicit consumer operation.

Rust 1.88 or newer is required.

## Platform status

| Target | Backend | Build coverage | Hardware expectations |
| --- | --- | --- | --- |
| Linux | V4L2 | CI build, tests and sanitizer bridge tests | Normal `/dev/video*` access; no root required when device permissions are configured |
| Linux/macOS/Android | UVC | CI build and native lifecycle tests | Android requires `UsbManager` permission; bundled libusb has LGPL distribution obligations |
| macOS | AVFoundation | CI build and contract tests | Camera usage description and sandbox entitlement are required |
| iOS | AVFoundation | CI cross-build | Physical-device behavior must be validated by the application |
| Windows | Media Foundation | CI build and contract tests | Uses the Windows SDK; direct libusb UVC is not supported |
| Android | Camera2 | ARM64/ARMv7 cross-build and demo instrumentation build | API 24+ and runtime CAMERA permission |
| Android | V4L2 | Cross-build and diagnostic demo | Ordinary apps normally lack `/dev/video*` permission; the library never modifies device permissions |

Build and cross-compile results are not presented as physical-camera proof. The
hardware notes identify the exact backend and device family for measured runs.

## Why the UVC backend uses libuvc and libusb

The opt-in `backend-uvc` feature intentionally retains the bundled `libuvc` and
`libusb` implementation. We evaluated replacing the USB transport with the
pure-Rust [nusb](https://github.com/kevinmehall/nusb) crate, but this is not a
drop-in dependency change:

- `nusb` is a low-level USB transport library, while `libuvc` also implements
  UVC descriptor parsing, stream negotiation, payload assembly and camera
  controls.
- UVC cameras may stream over either bulk or isochronous USB endpoints. At the
  time of this release, cross-platform isochronous transfers are not available
  through nusb's released public API; the work remains tracked in
  [nusb#47](https://github.com/kevinmehall/nusb/issues/47) and
  [nusb#178](https://github.com/kevinmehall/nusb/pull/178).
- A bulk-only replacement would exclude otherwise supported isochronous UVC
  cameras. A complete replacement would require a new Rust UVC protocol layer
  as well as safe, tested isochronous implementations for every supported USB
  platform.

For now, compatibility takes priority over removing the native dependency. We
may revisit a pure-Rust UVC backend when the transport and hardware coverage are
mature enough to preserve both bulk and isochronous device support.

Enabling `backend-uvc` statically builds the bundled LGPL-2.1-or-later libusb.
Applications distributing that build must satisfy the applicable notice,
source and relinking obligations described in
[THIRD_PARTY.md](THIRD_PARTY.md). On Android, applications should obtain USB
permission through `UsbManager` and pass the authorized file descriptor to the
adapter; root access is not required for that path.

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
| Integrations | `ndarray`, `serde` |
| Experimental backend authoring | `custom-backend` |

The default convenience set is `native` and `runtime-tokio`. RGB conversion and
MJPEG decoding stay opt-in so a basic capture build does not require a JPEG
toolchain. A backend never implies JPEG decoding. Examples:

```toml
# Linux V4L2 capture, native frames only
camera-rs = { version = "0.5", default-features = false, features = ["backend-v4l2"] }

# Core request/format/identity contracts without Tokio
camera-rs = { version = "0.5", default-features = false }
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

## Structured errors

Every public operation returns `camera::CameraError`. Branch on stable semantic fields instead of matching display text:

```rust
use camera::{CameraErrorKind, RecoveryHint};

# fn handle(error: camera::CameraError) {
match error.recovery_hint() {
    RecoveryHint::RequestPermission => request_camera_permission(),
    RecoveryHint::Retry | RecoveryHint::ReenumerateDevice => schedule_retry(),
    RecoveryHint::DropFrame => return,
    _ => report(error.code(), &error.to_string()),
}

if error.kind() == CameraErrorKind::DeviceBusy {
    show_camera_in_use();
}
# }
# fn request_camera_permission() {}
# fn schedule_retry() {}
# fn report(_: &str, _: &str) {}
# fn show_camera_in_use() {}
```

`code()`, `kind()`, `recovery_hint()`, `backend()`, `device()`, `stage()`, `operation()`, and `native_code()` are structured diagnostics. Error kinds, recovery hints, and operation stages implement `FromStr`; with the `serde` feature they serialize as the same stable snake-case codes. Aggregate failures derive an actionable recovery hint and expose their first typed cause through the standard source chain, while `backend_attempts()` and `causes()` retain every typed failure. The human-readable `Display` message is diagnostic text and is not a compatibility surface.

The Android JNI boundary raises `com.medivh.camera.CameraException`. Its
`schemaVersion`, `code`, `recovery`, `backend`, `stage`, `nativeCode` and
`diagnosticMessage` accessors are stable caller-facing fields; applications do
not need to parse exception text.

## Stability and releases

This crate follows Semantic Versioning for its documented public API. Before
1.0, minor releases may contain breaking API changes; patch releases do not.
Hardware-derived enums are treated as extensible. The `custom-backend` feature
is experimental and may change between minor releases. See
[CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).

See [API contracts](docs/API.md), [architecture](docs/ARCHITECTURE.md), [build instructions](BUILD.md), [V4L2](docs/V4L2.md), and the [Android adapter](camera-android/README.md).

## License

Copyright (c) 2026 ChenZibo. Licensed under either the MIT License or the
Apache License, Version 2.0, at your option. See [third-party notices](THIRD_PARTY.md)
for bundled dependency terms.
