# Medivh Camera Android Backend Demo

这个应用是 `camera-rs` Android 后端的交互式诊断工具，不只用于展示画面。它会使用设备真实返回的数据完成以下流程：

- 在 `Camera2`、`UVC`、`V4L2` 三个后端之间显式切换，不做静默回退。
- 枚举所选后端的设备。
- 启动前查询并展示原生格式、分辨率和有理数帧率；V4L2 的连续或步进范围也会保留在 JNI 结果中。
- 按选中的格式、分辨率和帧率启动，而不是固定使用 `640x480@30`。
- 显示实时帧率、后端收帧/发布数量、池丢帧、订阅丢帧和转换耗时。
- 启动后检测控制 descriptor，区分可读、可写、数值范围、步进、默认值、模式及手动模式依赖。
- 对可写数值控制使用滑杆，对曝光/对焦/白平衡等模式控制使用下拉选择，并支持恢复设备报告的默认值。

## 后端权限边界

- `Camera2` 使用 Android `CAMERA` 运行时权限。
- `UVC` 通过 `UsbManager` 请求用户授权，能力查询和启动都使用已经授权的 USB 文件描述符。
- `V4L2` 直接打开 `/dev/video*`。普通 Android 应用通常没有设备节点 DAC/SELinux 权限；Demo 会展示结构化的 `permission_denied`，不会把它误报成“没有摄像头”。要实机测试 V4L2，需要设备镜像、ueventd/SELinux 策略或调试 root 环境允许应用访问节点。

## 构建

要求 Rust 1.88+、Android SDK、NDK 27，以及 `aarch64-linux-android` Rust target。

```bash
rustup target add aarch64-linux-android
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.0.12077973"
cd android_example
./gradlew assembleDebug
```

Gradle 会以以下特性构建 JNI：

```text
camera2,uvc,v4l2,convert-rgb,decode-mjpeg
```

也可以只构建 JNI：

```bash
./build-jni.sh
```

## JNI 能力接口

```java
JSONArray devices = MedivhCamera.devices("camera2");
String id = devices.getJSONObject(0).getString("id");
JSONObject capabilities = MedivhCamera.capabilities("camera2", id);
JSONArray modes = capabilities.getJSONArray("modes");

JSONObject mode = modes.getJSONObject(0);
try (MedivhCamera camera = new MedivhCamera("camera2", id)) {
    JSONObject negotiated = camera.start(
        mode.getString("format"),
        mode.getInt("width"),
        mode.getInt("height"),
        mode.getInt("frameRateNumerator"),
        mode.getInt("frameRateDenominator")
    );
    JSONArray controls = camera.controls();
}
```

`nativeCapabilities` 返回：

- `knowledge`: `known`、`representative` 或 `unknown`。
- `modes`: 离散/代表性格式、宽高、帧率分子分母和 FPS。
- `ranges`: V4L2 等后端报告的连续或步进尺寸/帧间隔范围。
- `nativeFormats`: 设备原生采集格式。
- `conversions`: 到 RGB 的转换是否可用及缺少的 feature。
- `limitations`: 后端发现的限制说明。

JNI 错误消息是结构化 JSON，界面会同时展示稳定的 `code`、`recovery` 和可读消息。调用方应基于 `code`/`recovery` 分支，不能解析自然语言错误文本。

## 设备测试

仓库中的 `BackendInstrumentedTest` 会验证能力查询、指定格式启动、帧唯一性、统计、控制 descriptor、停止唤醒、重复启停和文件描述符稳定性：

```bash
./scripts/test-android-device.sh DEVICE_SERIAL camera2
./scripts/test-android-device.sh DEVICE_SERIAL uvc
./scripts/test-android-device.sh DEVICE_SERIAL v4l2
```

在已授权 root 的调试设备上，可显式要求脚本在 APK 安装后临时放开一个 V4L2 节点。脚本会记录原 mode，并在测试成功或失败后恢复：

```bash
CAMERA_V4L2_ROOT=1 CAMERA_V4L2_DEVICE=/dev/video0 \
  ./scripts/test-android-device.sh DEVICE_SERIAL v4l2
```

这只处理设备节点 DAC 权限，不会修改 SELinux 状态或写入持久系统策略；在 Enforcing 设备上仍需要正确的系统策略。

同一个物理 USB 摄像头不能同时由 UVC、V4L2 和 Camera2 外接相机服务占用。切换后端前先停止并关闭当前会话。
