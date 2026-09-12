# camera-rs-android

Android JNI and USB-permission adapter for camera 0.5. The default feature set
enables the Camera2 backend and RGB delivery required by the Java `start()` API.

```toml
camera-rs-android = "0.5"
```

The crate archive includes the Java facade under `java/com/medivh/camera`.
Android builds should compile that source directory together with the generated
`libcamera_android.so`. Native failures are raised as `CameraException`, whose
typed accessors expose the versioned error payload without parsing display text.

Optional adapter features are `uvc`, `v4l2`, `convert-rgb`, and `decode-mjpeg`. `uvc` is required for authorized USB descriptors; `v4l2` requires device-node access beyond the normal CAMERA permission. A custom native-output-only Camera2 build may disable default features and enable only `camera2`; it does not compile UVC/V4L2, RGB conversion, or TurboJPEG.

Capture always starts the core library in native-output mode. The existing direct `ByteBuffer` ownership rule remains: Java exclusively owns the writable direct buffer for the JNI call, Rust copies one delivered frame into it, and a too-small buffer leaves that frame pending for retry. If Java asks for RGB, `convert-rgb` must be enabled and conversion occurs after this adapter consumer receives its frame; the converter and output storage are reused.

USB permission request, permission observation and device open are separate JNI operations. Camera2 open does not implicitly drive a USB prompt. UVC accepts an already-authorized descriptor and duplicates ownership before opening.

JSON remains the compatibility surface for low-frequency device/configuration diagnostics. High-frequency pixel bytes stay in the direct buffer. Session close waits for native cleanup; dropping/cancelling a Rust future cannot force an OS call already executing to stop immediately.

JNI failures use `IllegalStateException` with a JSON message rather than unstructured prose. The object contains stable `code`, `recovery`, `backend`, `stage`, optional `nativeCode`, and a human-readable `message`; Kotlin/Flutter callers should branch on `code` or `recovery` and only display/log `message`. Core `camera::CameraError` metadata and native codes pass through unchanged, while JNI/adapter failures are normalized to the same codes. Rust-side adapter callers can use `code()` and `is_retryable()` directly.

Example payload:

```json
{"code":"permission_denied","recovery":"request_permission","backend":"uvc","stage":"open","nativeCode":-3,"message":"Permission denied: USB camera"}
```
