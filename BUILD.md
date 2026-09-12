# Build and verification

Use Rust 1.88+ and a platform C/C++ toolchain. Default builds select the native OS backend. The pure synthetic/core configuration needs no camera SDK:

```sh
cargo test --no-default-features
cargo test --features ndarray,backend-uvc
cargo clippy --all-targets --features ndarray,backend-uvc -- -D warnings
cargo doc --no-deps
cargo package --locked
cargo publish --dry-run --locked
cargo bench --bench color_convert
cargo run --example shared_frame_probe -- --synthetic
```

Native MJPEG support uses the `turbojpeg` Rust package, whose build needs CMake and a C compiler; x86 SIMD builds need NASM. Linux default capture is V4L2 with MMAP. UVC is opt-in and builds bundled libusb/libuvc; missing sources or native build errors fail immediately, without an automatic system-library fallback; macOS uses IOKit. Bundled UVC is supported on Linux, macOS and Android; Windows uses Media Foundation and the Windows SDK/toolchain. See `VENDORING.md` before distributing a statically linked UVC build. Do not globally hardcode an Android linker or a developer's filesystem path in Cargo configuration.

For Apple applications provide camera usage strings and the appropriate sandbox entitlements. For Android Camera2 request CAMERA permission in the application; for USB request permission through UsbManager before passing an owned/borrowed descriptor. Camera2 requires Android API 24+.

```sh
export ANDROID_NDK_HOME=/path/to/android-ndk
rustup target add aarch64-linux-android armv7-linux-androideabi
./scripts/check-android.sh aarch64-linux-android
./scripts/build-android-adapter.sh aarch64-linux-android
./scripts/build-android-adapter.sh armv7-linux-androideabi
```

The Android adapter and example commands require a repository checkout; they are separate from the core crates.io source archive. The adapter build copies `libcamera_android.so` and the ABI-matched `libc++_shared.so` to the corresponding example `jniLibs` directory. Build the Android example with `cd android_example && ./gradlew assembleDebug`. See `camera-android/README.md` for JNI buffer ownership and threading.

Cross-target `cargo check` verifies Rust bindings and target-specific native compilation, not physical camera behavior. Run hardware probes independently on each target:

```sh
cargo run --release --example shared_frame_probe
cargo run --release --example shared_frame_probe -- --native
cargo run --example probe_formats
cargo run --example resilient_camera
```

The conversion benchmark is a synthetic CPU benchmark, not an end-to-end camera/display latency measurement. CI exercises platform builds, tests, docs and packaging without requiring cameras. Package inclusion is allowlisted; test images, Android build output and unused third-party source trees are excluded.

## Android physical-device validation

`backend-v4l2` is available explicitly on Android for processes with access to
`/dev/videoN`; normal applications should use Camera2 or UsbManager-authorized UVC.
Builds never alter device permissions. The JNI example includes all three backends.

```sh
export ADB=/path/to/adb
./scripts/test-android-device.sh DEVICE_SERIAL camera2 -e rounds 3 -e frames 120
./scripts/test-android-device.sh DEVICE_SERIAL uvc -e width 1280 -e height 720
./scripts/test-android-device.sh DEVICE_SERIAL v4l2 -e expectPermissionDenied true
# Build a standalone Android NDK probe; deploy explicitly to a diagnostic device.
./scripts/check-android.sh aarch64-linux-android --release --example backend_probe
./scripts/test-native.sh
```

The instrumentation checks direct-buffer ownership/errors, session epochs,
concurrent stop, optional repeated open/close (`-e cycles 15`), FD growth,
Camera2 control metadata, USB brightness write/read/restore, frame metrics and
UVC-to-Camera2 handoff. Each device must run one camera test at a time. The runner
checks JUnit output because Android's `am instrument` can exit 0 on test failure.
It installs only the isolated demo/test packages, requires normal camera/USB
permission, and never restarts a system service to hide a failed handoff.

Per-transfer libusb and verbose descriptor diagnostics are disabled by default;
`native-debug-logs` enables them for diagnostics. This feature is unsuitable for
performance measurements. Native C lifecycle/negotiation tests run with ASan and
UBSan. Hardware-specific observations and private test logs are intentionally
kept outside the public repository.

For capture-only UVC stress on a firmware with a known HAL rediscovery defect,
`-e requireCamera2Handoff false` continues after recording a failed handoff. This
is not a passing handoff result; the default remains strict. Repeated cycles
keep one Activity alive and record FD targets to separate UI warm-up from native
USB/camera resource growth.
