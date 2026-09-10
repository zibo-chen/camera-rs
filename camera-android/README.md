# camera-android

Android USB permission integration and JNI adapter for `camera` 0.2. Licensed MIT OR Apache-2.0. Requires Android API 24+, Rust 1.88+ and the Android NDK. The core Camera2 backend can be used without this package or JNI.

Run `../scripts/build-android-adapter.sh aarch64-linux-android` with `ANDROID_NDK_HOME` set. Java examples are in `../android_example/src/main/java/com/medivh/camera`. Package both `libcamera_android.so` and the matching NDK `libc++_shared.so` (the build script copies both). Load `camera_android`, initialize with an application Context, obtain CAMERA/USB permission, enumerate devices, open by the returned native ID, and start. Close every handle explicitly with the Java AutoCloseable wrapper.

JNI calls are blocking. Run enumeration/start/nextFrame/stop/close on workers. Start returns the actual negotiated configuration in JSON. Each handle owns one frame reader. Supply a writable direct ByteBuffer exclusively owned for the duration of nextFrame; Rust copies the bytes and returns plane/format/timestamp metadata in JSON. A short buffer throws while retaining the pending frame, allowing resize/retry. The adapter never returns a dangling pointer into a driver callback.

Native errors throw IllegalStateException; callers must handle them. Concurrent stop wakes frame waits, start/stop/close are serialized, and a closed handle cannot start again. USB file descriptors are duplicated before Java connections close. Duplicate VID/PID devices are addressed by UsbManager device path, not the first matching product. Permission grant is asynchronous: retry opening after Android reports the grant.

Control descriptors carry native units and actual/requested/unknown readback semantics inherited from the core. `control(id)` reports Actual/Requested/Unknown with its value; `metrics()` reports bounded-pool, conversion and receiver statistics. Batching can be used through Rust. It does not initialize a process-global logger.

Backends are `camera2`, `uvc`, and `v4l2`. V4L2 requires device-node access and is primarily for privileged integration; CAMERA permission alone is insufficient. See [physical-device tests](../docs/ANDROID_HARDWARE_20260910.md) and [the runner](../scripts/test-android-device.sh).
