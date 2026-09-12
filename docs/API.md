# Camera API 0.4

0.4 intentionally removes the pre-0.4 public request, backend enum and session call shapes. There is one capture implementation exposed at three levels: a convenient owning `Capture`, a complete `Device`/`Session` API, and stable external-backend traits.

## Simple capture

`CameraSystem::capture(selector)` returns a builder. `start()` resolves the device, negotiates a native mode, validates the first frame and returns `Capture`, which owns the session plus a latest-frame receiver. `close().await` requests and waits for cleanup; `Drop` only requests cleanup. An OS call already running in a blocking worker cannot be forcibly cancelled by dropping its Rust future.

## Device and backend selection

`DeviceSelector` supports the platform default, an exact `DeviceId`, facing, USB VID/PID/serial and name matching. Exact IDs never change device after failure. Multiple matches are an error. A persisted ID preserves its backend namespace, while `IdentityStability::EnumerationOnly` warns that the native portion is not durable across enumeration cycles.

`BackendPolicy` has strict semantics:

| Policy | Meaning |
| --- | --- |
| `PlatformDefault` | Use the documented platform preference; it does not claim runtime benchmarking. |
| `Require(id)` | Only this backend; never fall back. |
| `Prefer(ids)` | For default-device capture, try in order and return an attempt report if all fail. Permission failures stop fallback. |

`backend_availability` separates `NotCompiled`, `UnsupportedTarget` and `Available`. Opening can still fail with permission, busy, removal or driver errors.

## Request, plan and negotiated result

`CaptureRequest` expresses preferred or exact resolution, preferred and minimum rational frame rate, accepted formats in preference order, optional required aspect ratio, an optimization priority, memory budget, startup deadline and recovery policy. `CaptureProfile` supplies useful starting points without creating a second capture path.

`plan()` ranks advertised modes and reports alternatives and estimated pool memory. It is advisory. `start()` performs the same ranking, asks the native backend to start, reads the actual mode and validates the first frame. Driver changes that violate hard constraints fail. The final `NegotiatedCapture` is authoritative for the started session.

Capabilities are conditional. `DeviceCapabilities` distinguishes known modes from unknown discovery, lists native formats, and reports whether each RGB conversion is available or which feature/external decoder is required. Unknown YUV color information remains unknown; conversion requires an explicit override instead of silently choosing a matrix or range.

## Native frames and conversion

All capture requests deliver `CapturedFrame` in the selected native format. The data path is:

```text
backend -> bounded immutable frame pool -> per-consumer queue/rate limit -> optional conversion
```

`frame.plane(index)`, `rgb_view()` and `ndarray_view()` derive views from `FrameLayout`, stride and byte length. They do not inspect an internal storage enum, convert or copy. A native RGB frame can therefore be viewed even when stored as generic bytes.

`RgbConverter::convert_into` writes into reusable caller storage. `ConversionRequest` specifies the output dimensions and optional color override. The existing scalar, NEON, SSSE3 and AVX2 kernels remain the implementation; MJPEG decoding is available only with `decode-mjpeg`. H264 remains a native payload requiring an external decoder.

Native frames may be copied from platform/driver buffers into the library pool. A future driver-buffer lease would be a separate advanced capability because retaining driver buffers can stall capture.

## Independent subscribers and memory

```rust
# use camera::{FrameRate, OverflowPolicy, SubscriptionOptions};
let preview = SubscriptionOptions::latest();
let recording = SubscriptionOptions::buffered(8)
    .overflow(OverflowPolicy::DropOldest);
let inference = SubscriptionOptions::latest()
    .max_rate(FrameRate::new(10, 1)?);
# Ok::<(), camera::CameraError>(())
```

`max_rate` limits that receiver, not the camera. Queues are finite and never called “lossless.” Queue depth must fit within the shared memory budget. Multiple consumers can still exhaust a shared pool by retaining frames; copy explicitly for long-term ownership. Pool drops and per-receiver drops are separate metrics.

## State, events and diagnostics

`state()` is the current value. `watch_state()` is a coalescing state watch. `events()` is a bounded event stream; lag is returned as `SessionEvent::Missed { count }`. Lifecycle changes, negotiated adjustments and resource pressure use explicit event variants.

Frame metadata includes plane offsets/lengths/strides, color metadata, orientation, host capture time, optional source timestamp with clock domain, and source/publication sequence distinction. Nominal FPS is not measured throughput. Compare CPU, allocations, copies and frame age/P95/P99 under the same hardware, input and build flags.

## Backend options and controls

Backend options are typed and rejected when used with a different selected backend. 0.4 exposes implemented settings only: `V4l2Options::mmap_buffers`, `Camera2Options::{max_images, request_template}`, and `AvFoundationOptions::late_frames`. Camera2 validates its reader depth before streaming and passes the selected NDK request template into native request creation; AVFoundation passes its late-frame policy to `AVCaptureVideoDataOutput`. UVC’s authorized-descriptor open path remains an explicit system method. Media Foundation does not publish placeholder fields.

Controls retain the generic descriptor/value API and add `Exposure`, `WhiteBalance`, `Focus` and `Kelvin` helpers. Helpers perform mode ordering and unit conversion, but do not promise unsupported hardware precision. Batch operations are sequential and report every result; they are not atomic. Unsupported controls are omitted, while unexpected descriptor-query failures now fail the query instead of making a control silently disappear.

## External backends

Register a `BackendProvider` on `CameraSystem::builder()`. `BackendDevice` handles capabilities/start/stop, while `FrameSink` publishes through the same bounded epoch-protected frame pool. `WritableFrameLease` borrows writable storage directly from that pool, becomes immutable on `commit`, and returns its slot without publication when dropped. Control calls may use dynamic dispatch; frame callbacks should request a writable lease or publish directly rather than spawning one async task per frame. Unknown formats may be transported under a namespaced backend, but generic conversion is never implied.

## Runtime boundaries

Without default features, format/request/capability/identity contracts compile without Tokio. `runtime-tokio` enables asynchronous systems, sessions, backends and frame receivers. `blocking` exposes `camera::blocking` and reuses one persistent multi-thread runtime; it does not create a runtime per method call.

## Migration boundary

There is no compatibility layer for `BackendType`, `StreamRequest`, `OutputFormat`, `SelectionPolicy`, `Camera`, `CaptureSession`, parameterless `subscribe()`, `negotiated_config()` or `stop()`. Update applications, examples and adapters to the 0.4 types in one change.
