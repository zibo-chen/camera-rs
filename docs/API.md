# Camera API 0.3

This release keeps the owned-session model introduced in 0.2 and deliberately replaces the old `RgbConverter` call shape with `ConversionRequest`. Rust 1.88 or newer is required. The core requires a Tokio runtime with time support; the Android JNI adapter owns its runtime. Applications own logging and platform permission prompts.

## Capture

1. Create `CameraSystem::new()` (platform native backend), `with_backend(...)` (strict selection), or `synthetic()` (hardware-free test pattern).
2. Enumerate `devices().await` and retain the returned opaque `DeviceId`. Open with `open(&id).await`.
3. Build a `StreamRequest` and call `Camera::start`. This returns an owning `CaptureSession` only after a valid first frame arrives.
4. Call `session.subscribe()` once per consumer, then `receiver.next().await`. Each receiver has an independent cursor and bounded delivery queue.
5. Call `session.stop().await` to wait for native cleanup. Dropping a session requests shutdown immediately. Receivers and frames do not own capture lifetime.

`devices_blocking` and `capabilities_blocking` are for synchronous worker threads. Async enumeration, opening and native control work are kept off executor workers. Do not run blocking inventory calls on a GUI or async executor thread.

`start`'s timeout covers configuration discovery and first-frame startup. Cancelling its future requests cleanup; an already executing OS call cannot be forcibly interrupted. During cleanup the device remains busy. A successful `stop().await` is the explicit synchronization point for restarting. Old sessions and their receivers cannot consume or stop a subsequent session.

## Device identity and recovery

Indices are display/enumeration positions, not persisted device IDs. IDs are scoped to a backend. AVFoundation and Camera2 use native IDs, Media Foundation uses its symbolic link, V4L2 prefers `/dev/v4l/by-id` then `/dev/v4l/by-path`, and UVC uses VID/PID plus serial when available. Ambiguous IDs fail. No VID/PID-only fallback selects the first matching camera.

`IdentityStability::EnumerationOnly` means automatic reconnect is unsafe. Native identity is an OS/driver identity, not a globally unique physical-device guarantee; changing ports, permissions or driver state can change it. Explicit unavailable backends return `BackendUnavailable`.

An optional `ReconnectPolicy` reopens the same identity after a frame stall. Existing subscribers survive recovery, and `FrameKey.session` changes after native capture restarts. `events()` is a coalescing watch channel over session state; slow observers can miss intermediate transitions. Recovery validates the actual dimensions, capture format and rational rate against the original negotiated mode, then validates the first frame dimensions. A mismatch stops that attempt; final recovery failure terminates frame waits. USB permission descriptors require the application to reauthorize/reopen explicitly.

`DeviceWatcher` owns its polling task. `snapshot()` returns the latest inventory; `changed()` waits for a changed inventory or scan error. Drop cancels; `stop().await` joins. Scans are serialized and do not overlap. An OS enumeration already running can finish after cancellation, but cannot publish a stale snapshot.

## Request versus negotiated capture versus delivered frame

`StreamRequest` expresses dimensions, rational `FrameRate`, optional capture format, `SelectionPolicy`, delivery policy and memory budget. `Exact` checks the driver's reported dimensions, format and rational frame rate. `Closest` records adjustments in `NegotiatedConfig`. Advertised configuration lists include representative rates for continuous ranges; unlisted rates on advertised dimensions can be tried and validated by the driver. Nominal negotiated FPS is not measured sustained throughput.

`NegotiatedConfig.capture` is the device capture mode. `FrameLayout.format` describes the delivered bytes. For example NV12 capture can deliver RGB8. `OutputFormat::Native` copies native planes/encoded payload into owned pooled memory without RGB conversion. It does not promise a borrowed driver buffer or a GPU texture. Platform support still depends on the advertised capture format. H264 requires native output and an external decoder.

Synthetic capture accepts positive dimensions and rational rates, generates packed RGB, and has no device controls. It cannot validate native backend behavior.

## Memory and delivery

The default is `Latest`, six storage slots, and a 192 MiB pixel-storage limit. Configure these with `DeliveryPolicy` and `MemoryBudget`. `Buffered` has a finite capacity and either `DropOldest` or `DropNewest`. Callbacks never block waiting for a consumer. Queue capacity must be smaller than the pool capacity; multiple consumers can collectively retain more slots, so size the budget for the workload.

Frames are immutable `Arc<CapturedFrame>`. Retaining them or shared ndarray allocations keeps pool slots occupied. Exhaustion drops incoming frames without modifying any retained pixels. Copy deliberately with `copy_to`/`to_owned_bytes` for archival workloads. The byte budget includes reserved native `Vec` capacities, including variable-sized JPEG payloads; it excludes OS/driver queues, decoder scratch space, frame metadata, and application copies. Conversion runs outside the pool mutex, and late callbacks are rejected by a capture epoch check.

The `ndarray` feature exposes borrowed views and shared RGB allocations without copying. Native frames require explicit conversion first. `RgbConverter` writes into caller-owned RGB storage and reuses its JPEG decoder and scratch buffer. Its required `ConversionRequest` carries the target dimensions and optional color override. Same-size YUYV/UYVY, NV12/NV21 and I420 conversions use configured block kernels for BT.601/709/2020 and SMPTE 240M, in both Full and Limited range. ARM64 uses explicit NEON and reuses chroma work across each pair of I420/NV12/NV21 luma rows. x86_64 runtime-dispatches packed YUYV/UYVY, planar I420 and full/half-size NV12/NV21 rows to AVX2 when available; other paths retain scalar/compiler-vectorized kernels. BGRA/RGBA/ARGB channel reordering uses NEON on ARM64 and runtime-selected AVX2 or SSSE3 on x86_64, with exact-length RGB24 stores. Smaller RGB outputs are sampled directly from raw frames without allocating a full-size RGB intermediate; half-size NV12/NV21 has a dedicated block kernel. MJPEG selects the smallest supported TurboJPEG DCT scale that still covers the requested size, then performs a final nearest-neighbor resize only when required. All SIMD paths preserve the configured integer coefficient and rounding contract. Orientation, tone mapping and transfer-function/primaries conversion are not applied.

To avoid converting frames that a slower consumer will never observe, capture `OutputFormat::Native` with `DeliveryPolicy::Latest`; call `receiver.next()` after each processing cycle and convert that returned frame. Keep direct RGB capture for consumers that genuinely need every frame, because forcing Native capture there can add a copy before conversion.

## Metadata and measurements

Planes carry byte offsets, valid lengths, row strides and pixel strides. Final rows can omit trailing padding. Color matrix, range, primaries and transfer function use explicit `Unknown` when unavailable. Rotation/mirroring describe the stored image orientation, not a universal display transform based on phone posture. `bottom_up` identifies bottom-origin row storage.

`captured_at` is host monotonic processing entry time; `timestamp_ns` is the host wall-clock timestamp at that entry. `source_timestamp()` includes an explicit clock domain and must not be subtracted from a different clock. Sensor/exposure timestamps are not invented. Source sequence and library publication sequence are separate.

`metrics()` exposes admitted frames, published frames, pool drops, conversion errors, allocated/retained storage, conversion total/max and p50/p95 over the latest 256 published frames. Native-output timing measures copying. `FrameReceiver::dropped_frames()` counts receiver queue overflow; `queued_frames()` shows its depth. `Frame::age()` uses the host monotonic clock. `NegotiatedConfig::first_frame_latency` includes startup work. Counters restart with each native capture epoch, including reconnect; they are not lifetime totals. Driver drops before entering the library are only observable when a driver supplies source sequence numbers. `stats().buffer_size` is the shared pool capacity; `driver_buffers` is a separate V4L2 setting.

## Controls

Query `controls().await` before displaying controls. The descriptor has units, optional range/default/step, supported modes, a readable flag and optional writable flag (`None` means the driver adapter cannot establish writability). Unsupported controls are omitted. Do not interpret missing metadata as a zero range/default.

`ExposureMode`, `ExposureTime` and `ExposureCompensation` are distinct. UVC/V4L2 exposure time uses 100 microseconds; Media Foundation uses log2 seconds; AVFoundation exposure bias uses milli-EV; Camera2 compensation uses the native compensation index. Focus and white balance similarly separate mode from numeric settings. AVFoundation lists only modes reported supported by the active device. V4L2 exposes read-only status. Camera2 readback is `Requested` after a successful write or `Unknown` until then, because capture-result readback is not implemented. Camera2 range queries do not establish device defaults; it never reports cached requests as measured values.

`control()` returns `Actual`, `Requested` or `Unknown`. Numeric setters validate finite representable values and known ranges/steps. `set_exposure_time` rejects durations not exactly representable by the native unit. `CameraError::Native` retains Camera2 bridge status codes and Media Foundation HRESULTs with operation context. Native setters can still fail despite enumeration (device state, automatic mode, permissions, removal). Some numeric native setters disable their associated automatic mode. `set_controls` and `reset_controls` return one outcome per operation; they are sequential, non-transactional, and never turn partial failure into success.

## Migration

| Pre-0.3 pattern | 0.3 replacement |
| --- | --- |
| Public backend factory and `*_arc` methods | `CameraSystem`, `Camera::start` |
| Raw numeric device index persisted | Enumerate and retain `DeviceId` |
| `wait_for_frame` / manual frame keys | Persistent `FrameReceiver::next` |
| Implicit per-consumer `Array3` copy | Shared frame; optional `ndarray()` / `shared_ndarray()` |
| Public `FrameHub`, recovery traits/wrappers | Owned `CaptureSession` with reconnect policy |
| Restartable global device monitor | Owned `DeviceWatcher` |
| Integer FPS | Reduced nonzero `FrameRate` fraction |
| `RgbConverter` plus a separate color argument | `ConversionRequest` with target size and optional color override |
| Ambiguous brightness/exposure integers | Typed control IDs, modes and units |
| JNI and Android context in core crate | Separate `camera-android` package |

The retired public backend/trait/recovery modules are internal or removed. No compatibility aliases, fake GPU preferences, fake parallel-conversion controls, implicit logger initialization, or process-wide camera singleton remain in the core API.

## Android backend details

Camera2 mode descriptors come from the selected camera's AE/AF/AWB metadata.
AF `Single` triggers one autofocus operation; `Continuous` selects an advertised
continuous video/picture mode. Numeric controls absent from metadata are not
advertised. FPS requests select a supported AE range, preferring a fixed range
when available; a request outside all ranges is clamped and surfaced through
negotiated configuration/selection policy. Listed rates are nominal AE targets,
not a guarantee of delivery under long exposures. Camera2 uses integer rates.

Android can opt into `backend-v4l2` for privileged integrations. CAMERA permission
alone does not unlock V4L2 nodes. UVC uses an already-authorized USB descriptor;
closing waits for status transfers to quiesce and restores only drivers detached
by that handle. Switching between UVC and Camera2 also depends on the vendor HAL
rediscovering its kernel camera. The library does not restart system services.
See [the physical-device report](ANDROID_HARDWARE_20260910.md) before relying on
cross-backend handoff on vendor Android images.
