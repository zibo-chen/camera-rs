# Medivh Camera Android Example

这是 `medivh-camera` 库的 Android 示例应用，展示如何在 Android 平台上使用 UVC 摄像头。

## 功能

- 📷 **设备枚举**: 列出所有连接的 UVC 摄像头
- ⚙️ **配置选择**: 查看和选择摄像头支持的分辨率、帧率、格式
- 🎥 **实时预览**: 显示摄像头视频流
- 📊 **统计信息**: 显示帧率、丢帧率、缓冲区使用情况等
- 🎛️ **参数控制**: 调整亮度、对比度、曝光、对焦等摄像头参数
- 🔄 **热插拔支持**: 自动检测设备连接和断开

## 构建步骤

### 1. 环境准备

确保已安装：
- Android Studio
- Android NDK (推荐 27.0.12077973)
- Rust 和 cargo-ndk
- 目标平台工具链：`rustup target add aarch64-linux-android`

### 2. 编译 Rust JNI 库

```bash
cd medivh-camera/android_example
chmod +x build-jni.sh
./build-jni.sh
```

这将编译 `libmedivh_camera.so` 并放置到 `src/main/jniLibs/arm64-v8a/` 目录。

### 3. 使用 Android Studio 构建

1. 打开 Android Studio
2. 选择 "Open an Existing Project"
3. 选择 `UVCAndroid` 根目录
4. 等待 Gradle 同步完成
5. 选择 `medivh-camera.android_example` 模块
6. 点击 Run 按钮

## 项目结构

```
android_example/
├── build.gradle                    # Gradle 配置
├── build-jni.sh                    # JNI 库构建脚本
├── proguard-rules.pro              # ProGuard 规则
├── README.md                       # 本文件
└── src/main/
    ├── AndroidManifest.xml         # Android 清单
    ├── java/com/medivh/camera/
    │   ├── MedivhCamera.java       # 高层 API 封装
    │   ├── MedivhCameraBridge.java # JNI 桥接
    │   └── demo/
    │       ├── MainActivity.java   # 主界面
    │       └── MedivhCameraApp.java
    ├── jniLibs/arm64-v8a/          # 编译后的 .so 文件
    │   └── libmedivh_camera.so
    └── res/
        ├── layout/activity_main.xml
        ├── values/strings.xml
        ├── values/colors.xml
        ├── values/styles.xml
        └── xml/device_filter.xml   # USB 设备过滤器
```

## API 使用示例

### 初始化

```java
// 在 Application 中初始化
MedivhCamera.init();
```

### 列出设备

```java
List<MedivhCamera.DeviceInfo> devices = MedivhCamera.listDevices();
for (MedivhCamera.DeviceInfo device : devices) {
    Log.d(TAG, "Found: " + device);
}
```

### 获取支持的配置

```java
List<MedivhCamera.CameraConfig> configs = MedivhCamera.getSupportedConfigs(0);
for (MedivhCamera.CameraConfig config : configs) {
    Log.d(TAG, "Config: " + config);
}
```

### 打开摄像头并预览

```java
MedivhCamera camera = new MedivhCamera();

// 打开设备
if (camera.open(0)) {
    // 设置帧回调
    camera.setFrameCallback(frame -> {
        Bitmap bitmap = frame.toBitmap();
        // 更新 UI
        runOnUiThread(() -> imageView.setImageBitmap(bitmap));
    });
    
    // 启动流
    MedivhCamera.CameraConfig config = new MedivhCamera.CameraConfig(
        MedivhCamera.VideoFormat.MJPEG, 640, 480, 30
    );
    camera.startStream(config);
}
```

### 获取统计信息

```java
MedivhCamera.StreamStats stats = camera.getStats();
Log.d(TAG, "FPS: " + stats.currentFps);
Log.d(TAG, "Dropped: " + stats.droppedFrames);
```

### 控制摄像头参数

```java
// 获取支持的控制类型
List<MedivhCamera.ControlType> controls = camera.getSupportedControls();

// 获取范围
MedivhCamera.ControlRange range = camera.getControlRange(MedivhCamera.ControlType.BRIGHTNESS);
Log.d(TAG, "Brightness range: " + range.min + " - " + range.max);

// 设置值
camera.setControl(MedivhCamera.ControlType.BRIGHTNESS, 128);

// 重置
camera.resetControl(MedivhCamera.ControlType.BRIGHTNESS);
camera.resetAllControls();
```

### 清理

```java
camera.stopStream();
camera.close();
```

## 注意事项

1. **USB 权限**: 首次连接 USB 摄像头时需要用户授权
2. **设备兼容性**: 仅支持 UVC 兼容的 USB 摄像头
3. **最小 SDK**: API 21 (Android 5.0)
4. **架构支持**: 目前仅支持 arm64-v8a

## 故障排除

### 找不到设备

1. 确保摄像头已正确连接
2. 检查是否已授予 USB 权限
3. 尝试重新插拔摄像头

### 启动流失败

1. 尝试选择不同的配置（YUYV 格式通常兼容性更好）
2. 降低分辨率或帧率
3. 查看 logcat 输出获取详细错误信息

### 库加载失败

1. 确保已运行 `build-jni.sh` 构建 JNI 库
2. 检查 `jniLibs/arm64-v8a/libmedivh_camera.so` 是否存在
3. 确保设备是 arm64 架构

## 许可证

MIT License
