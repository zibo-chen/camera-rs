# camera-android

Android JNI and USB-permission adapter for camera 0.4. Camera2 is the only default backend.

```toml
camera-android = { version = "0.4", default-features = false, features = ["camera2"] }
```

Optional adapter features are `uvc`, `v4l2`, `convert-rgb`, and `decode-mjpeg`. `uvc` is required for authorized USB descriptors; `v4l2` requires device-node access beyond the normal CAMERA permission. A Camera2-only build does not compile UVC/V4L2 and does not include TurboJPEG.

Capture always starts the core library in native-output mode. The existing direct `ByteBuffer` ownership rule remains: Java exclusively owns the writable direct buffer for the JNI call, Rust copies one delivered frame into it, and a too-small buffer leaves that frame pending for retry. If Java asks for RGB, `convert-rgb` must be enabled and conversion occurs after this adapter consumer receives its frame; the converter and output storage are reused.

USB permission request, permission observation and device open are separate JNI operations. Camera2 open does not implicitly drive a USB prompt. UVC accepts an already-authorized descriptor and duplicates ownership before opening.

JSON remains the compatibility surface for low-frequency device/configuration diagnostics. High-frequency pixel bytes stay in the direct buffer. Session close waits for native cleanup; dropping/cancelling a Rust future cannot force an OS call already executing to stop immediately.
