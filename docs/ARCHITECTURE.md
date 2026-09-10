# Architecture

`CameraSystem` handles backend selection and device identity. `Camera` is an opened device. `CaptureSession` owns startup, capture, serialized controls, optional recovery and shutdown. `FrameReceiver` owns delivery state; `CapturedFrame` owns immutable shared storage. Public callers do not interact with backend factories, Arc wiring, native callback objects or recovery wrappers.

Native adapters retain platform threading rules: Media Foundation uses a COM worker, AVFoundation uses capture/delegate queues, UVC stops and joins callbacks before releasing callback state, Camera2 owns NDK images/requests, and V4L2 returns dequeued driver buffers with a scope guard. Async entry points offload native blocking work. Cancellation requests shutdown and preserves exclusive lifecycle ownership until outstanding native work settles.

All backends publish through one internal `FrameHub`. It reserves a bounded slot under the pool lock, releases that lock during conversion/copying, checks the capture epoch again, and then publishes one immutable snapshot to independent bounded subscriber queues. A lease returns storage even when conversion fails or panics. Strong frame/ndarray references keep slots occupied; weak references cannot cause mutation of shared storage or an `Arc::get_mut` panic.

The core stores copied native frames in pooled memory. This deliberately avoids exposing native pointers whose lifetimes end with callbacks. Platform GPU buffers/textures and decoder plugins require separate ownership, synchronization and release-thread contracts; they are not represented by no-op preferences.

The optional ndarray adapter is isolated from core frame storage. Android USB permission/context/JNI code lives in a separate `camera-android` package. JNI uses a per-handle lifecycle gate, catches Rust panics at the boundary, copies to caller-owned writable direct buffers, and reports actual operation failures. Core Camera2 capture does not require a Java context.

See [API contracts](API.md) for cancellation, negotiated configuration, identity, backpressure, metadata and control semantics. Native format conversion, hardware capture throughput and end-to-end display latency are measured separately.
