package com.medivh.camera.demo;

import android.Manifest;
import android.app.Activity;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.os.Bundle;
import android.os.SystemClock;
import android.view.View;
import android.widget.ArrayAdapter;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.RadioGroup;
import android.widget.SeekBar;
import android.widget.Spinner;
import android.widget.Switch;
import android.widget.TextView;

import com.medivh.camera.MedivhCamera;
import com.medivh.camera.CameraException;

import org.json.JSONArray;
import org.json.JSONObject;

import java.nio.ByteBuffer;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

/** Interactive backend probe. Blocking JNI work never runs on the UI thread. */
public final class MainActivity extends Activity {
    private static final int CAMERA_PERMISSION_REQUEST = 1;
    private static final int CONTROL_SCALE = 1000;

    private final ExecutorService captureWorker = Executors.newSingleThreadExecutor();
    private final ExecutorService commandWorker = Executors.newSingleThreadExecutor();
    private final AtomicBoolean running = new AtomicBoolean(false);
    private final AtomicBoolean previewPending = new AtomicBoolean(false);
    private final AtomicBoolean previewDisabled = new AtomicBoolean(false);
    private final AtomicInteger scanGeneration = new AtomicInteger();

    private RadioGroup backendGroup;
    private Spinner deviceSpinner, configSpinner, controlSpinner, controlModeSpinner;
    private SeekBar controlSeekBar;
    private Switch disablePreview;
    private ImageView preview;
    private View previewPlaceholder, statsOverlay;
    private LinearLayout controlsPanel;
    private TextView deviceCount, streamStatus, stats;
    private TextView overlayFps, overlayResolution, overlayFrames;
    private TextView controlRange, controlValue;
    private ImageButton startButton, stopButton, resetControlButton, resetAllButton;

    private volatile MedivhCamera activeCamera;
    private JSONArray inventory = new JSONArray();
    private JSONArray captureModes = new JSONArray();
    private JSONArray controlDescriptors = new JSONArray();
    private boolean selectingDevice;
    private boolean modeSelectionTouched;
    private boolean autoScanEnabled;
    private String lastCapabilityKey;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        setContentView(R.layout.activity_main);
        MedivhCamera.initWithContext(this);
        autoScanEnabled = !getIntent().getBooleanExtra("skipAutoScan", false);
        bindViews();
        bindActions();
        disablePreview.setChecked(false);
        disablePreview.setOnCheckedChangeListener((button, checked) -> previewDisabled.set(checked));
        setStreamingUi(false);
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.CAMERA}, CAMERA_PERMISSION_REQUEST);
        } else if (autoScanEnabled) {
            scanDevices();
        }
    }

    private void bindViews() {
        backendGroup = findViewById(R.id.rg_backend);
        deviceSpinner = findViewById(R.id.spinner_device);
        configSpinner = findViewById(R.id.spinner_config);
        controlSpinner = findViewById(R.id.spinner_control);
        controlModeSpinner = findViewById(R.id.spinner_control_mode);
        controlSeekBar = findViewById(R.id.seekbar_control);
        disablePreview = findViewById(R.id.switch_preview_render);
        preview = findViewById(R.id.image_preview);
        previewPlaceholder = findViewById(R.id.preview_placeholder);
        statsOverlay = findViewById(R.id.overlay_stats);
        controlsPanel = findViewById(R.id.layout_controls);
        deviceCount = findViewById(R.id.tv_device_count);
        streamStatus = findViewById(R.id.tv_stream_status);
        stats = findViewById(R.id.tv_stats);
        overlayFps = findViewById(R.id.tv_overlay_fps);
        overlayResolution = findViewById(R.id.tv_overlay_resolution);
        overlayFrames = findViewById(R.id.tv_overlay_frames);
        controlRange = findViewById(R.id.tv_control_range);
        controlValue = findViewById(R.id.tv_control_value);
        startButton = findViewById(R.id.btn_start_stream);
        stopButton = findViewById(R.id.btn_stop_stream);
        resetControlButton = findViewById(R.id.btn_reset_control);
        resetAllButton = findViewById(R.id.btn_reset_all);
    }

    private void bindActions() {
        findViewById(R.id.btn_refresh).setOnClickListener(view -> scanDevices());
        startButton.setOnClickListener(view -> startCapture());
        stopButton.setOnClickListener(view -> stopCapture());
        backendGroup.setOnCheckedChangeListener((group, checkedId) -> {
            if (!running.get()) scanDevices();
        });
        deviceSpinner.setOnItemSelectedListener(new SimpleItemSelectedListener(position -> {
            if (!selectingDevice && position >= 0) loadCapabilities(position);
        }));
        controlSpinner.setOnItemSelectedListener(new SimpleItemSelectedListener(position -> {
            if (position >= 0) showControl(position);
        }));
        controlModeSpinner.setOnTouchListener((view, event) -> {
            modeSelectionTouched = true;
            return false;
        });
        controlModeSpinner.setOnItemSelectedListener(new SimpleItemSelectedListener(position -> {
            if (!modeSelectionTouched || position < 0) return;
            modeSelectionTouched = false;
            JSONObject descriptor = selectedControl();
            if (descriptor != null) setControlValue(descriptor, controlModeSpinner.getSelectedItem().toString());
        }));
        controlSeekBar.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override public void onProgressChanged(SeekBar bar, int progress, boolean fromUser) {
                JSONObject descriptor = selectedControl();
                if (descriptor != null && fromUser) controlValue.setText(formatNumber(controlNumber(descriptor, progress)));
            }
            @Override public void onStartTrackingTouch(SeekBar bar) { }
            @Override public void onStopTrackingTouch(SeekBar bar) {
                JSONObject descriptor = selectedControl();
                if (descriptor != null) setControlValue(descriptor, controlNumber(descriptor, bar.getProgress()));
            }
        });
        resetControlButton.setOnClickListener(view -> resetSelectedControl());
        resetAllButton.setOnClickListener(view -> resetAllControls());
    }

    private String selectedBackend() {
        int selected = backendGroup.getCheckedRadioButtonId();
        if (selected == R.id.rb_camera2) return "camera2";
        if (selected == R.id.rb_v4l2) return "v4l2";
        return "uvc";
    }

    private void scanDevices() {
        if (running.get()) return;
        String backend = selectedBackend();
        int generation = scanGeneration.incrementAndGet();
        showStatus("正在枚举 " + backend + " 设备…");
        startButton.setEnabled(false);
        captureModes = new JSONArray();
        lastCapabilityKey = null;
        configSpinner.setAdapter(emptyAdapter());
        controlsPanel.setVisibility(View.GONE);
        captureWorker.execute(() -> {
            try {
                JSONArray found = devicesWithHandoffRetry(backend, generation);
                if (generation != scanGeneration.get()) return;
                String[] labels = new String[found.length()];
                for (int index = 0; index < labels.length; index++) {
                    JSONObject device = found.getJSONObject(index);
                    labels[index] = device.optString("name", device.getString("id")) + "\n" + device.getString("id");
                }
                runOnUiThread(() -> {
                    if (generation != scanGeneration.get() || !backend.equals(selectedBackend())) return;
                    inventory = found;
                    selectingDevice = true;
                    deviceSpinner.setAdapter(adapter(labels));
                    selectingDevice = false;
                    deviceCount.setText(getString(R.string.found_devices, labels.length));
                    if (labels.length == 0) showStatus(getString(R.string.no_devices));
                    else loadCapabilities(0);
                });
            } catch (Throwable error) {
                if (generation == scanGeneration.get()) showError("枚举 " + backend + " 失败", error);
            }
        });
    }

    /** Camera2/V4L2 external-camera nodes reappear asynchronously after UVC releases usbfs. */
    private JSONArray devicesWithHandoffRetry(String backend, int generation) throws Exception {
        int attempts = backend.equals("uvc") ? 1 : 16;
        JSONArray found = new JSONArray();
        for (int attempt = 0; attempt < attempts && generation == scanGeneration.get(); attempt++) {
            found = MedivhCamera.devices(backend);
            if (found.length() > 0) break;
            if (attempt + 1 < attempts) SystemClock.sleep(250);
        }
        return found;
    }

    private void loadCapabilities(int devicePosition) {
        if (running.get() || devicePosition < 0 || devicePosition >= inventory.length()) return;
        final String backend = selectedBackend();
        final String id;
        try { id = inventory.getJSONObject(devicePosition).getString("id"); }
        catch (Exception error) { showError("读取设备信息失败", error); return; }
        String queryKey = backend + "\u0000" + id;
        if (queryKey.equals(lastCapabilityKey)) return;
        lastCapabilityKey = queryKey;
        if (backend.equals("uvc") && !MedivhCamera.hasUsbPermission(id)) {
            MedivhCamera.requestUsbPermission(id);
            showStatus("请允许 USB 访问，然后点击刷新以读取 UVC 能力");
            return;
        }
        showStatus("正在读取分辨率、格式和帧率…");
        startButton.setEnabled(false);
        captureWorker.execute(() -> {
            try {
                JSONObject capabilities = MedivhCamera.capabilities(backend, id);
                JSONArray modes = capabilities.getJSONArray("modes");
                String[] labels = new String[modes.length()];
                for (int index = 0; index < labels.length; index++) labels[index] = captureModeLabel(modes.getJSONObject(index));
                runOnUiThread(() -> {
                    captureModes = modes;
                    configSpinner.setAdapter(adapter(labels));
                    if (labels.length > 0) configSpinner.setSelection(preferredModeIndex(modes));
                    startButton.setEnabled(labels.length > 0);
                    JSONArray ranges = capabilities.optJSONArray("ranges");
                    stats.setText(String.format(Locale.US, "%s：%d 个模式，%d 个范围描述\n原生格式：%s%s",
                            capabilities.optString("knowledge", "unknown"), labels.length,
                            ranges == null ? 0 : ranges.length(), capabilities.optJSONArray("nativeFormats"), limitations(capabilities)));
                    showStatus(labels.length > 0 ? "能力检测完成，可开始测试" : "设备未返回离散采集模式");
                });
            } catch (Throwable error) {
                runOnUiThread(() -> lastCapabilityKey = null);
                showError("读取设备能力失败", error);
            }
        });
    }

    private String limitations(JSONObject capabilities) {
        JSONArray values = capabilities.optJSONArray("limitations");
        return values == null || values.length() == 0 ? "" : "\n限制：" + values;
    }

    private String captureModeLabel(JSONObject mode) throws Exception {
        int numerator = mode.getInt("frameRateNumerator"), denominator = mode.getInt("frameRateDenominator");
        return String.format(Locale.US, "%s  %dx%d  %.3f FPS (%d/%d)", mode.getString("format"),
                mode.getInt("width"), mode.getInt("height"), numerator / (double) denominator, numerator, denominator);
    }

    /** Prefer the common performance target instead of the backend's lowest enumerated mode. */
    private int preferredModeIndex(JSONArray modes) {
        int best = 0;
        double bestScore = Double.POSITIVE_INFINITY;
        for (int index = 0; index < modes.length(); index++) {
            JSONObject mode = modes.optJSONObject(index);
            if (mode == null) continue;
            int width = mode.optInt("width"), height = mode.optInt("height");
            double fps = mode.optDouble("framesPerSecond",
                    mode.optDouble("frameRateNumerator") / Math.max(1.0, mode.optDouble("frameRateDenominator")));
            double score = Math.abs(width - 1920) * 1080.0 + Math.abs(height - 1080) * 1920.0
                    + Math.abs(fps - 30.0) * 10000.0;
            if (score < bestScore) { best = index; bestScore = score; }
        }
        return best;
    }

    private void startCapture() {
        if (!running.compareAndSet(false, true)) return;
        final String backend = selectedBackend();
        final JSONObject mode;
        final String id;
        try {
            id = inventory.getJSONObject(deviceSpinner.getSelectedItemPosition()).getString("id");
            mode = captureModes.getJSONObject(configSpinner.getSelectedItemPosition());
        } catch (Exception error) {
            running.set(false);
            showError("请先选择设备和采集模式", error);
            return;
        }
        if (backend.equals("uvc") && !MedivhCamera.hasUsbPermission(id)) {
            running.set(false);
            MedivhCamera.requestUsbPermission(id);
            showStatus("需要 USB 权限，授权后请重试");
            return;
        }
        setStreamingUi(true);
        showStatus("正在通过 " + backend + " 启动…");
        captureWorker.execute(() -> captureLoop(backend, id, mode));
    }

    private void captureLoop(String backend, String id, JSONObject mode) {
        MedivhCamera camera = null;
        try {
            camera = new MedivhCamera(backend, id);
            activeCamera = camera;
            JSONObject negotiated = camera.start(mode.getString("format"), mode.getInt("width"), mode.getInt("height"),
                    mode.getInt("frameRateNumerator"), mode.getInt("frameRateDenominator"));
            int width = negotiated.getInt("width"), height = negotiated.getInt("height");
            ByteBuffer buffer = ByteBuffer.allocateDirect(Math.multiplyExact(Math.multiplyExact(width, height), 4));
            Bitmap[] bitmaps = {
                    Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888),
                    Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            };
            int bitmapIndex = 0;
            JSONArray controls = camera.controls();
            runOnUiThread(() -> {
                showControls(controls);
                showStatus(backend + " · " + negotiated.optString("captureFormat") + " · 运行中");
                overlayResolution.setText(width + " × " + height);
            });
            long started = SystemClock.elapsedRealtimeNanos(), lastStats = started, frames = 0;
            while (running.get()) {
                camera.nextFrameArgb(buffer, 1500);
                if (!running.get()) break;
                frames++;
                if (!previewDisabled.get() && previewPending.compareAndSet(false, true)) {
                    Bitmap bitmap = bitmaps[bitmapIndex];
                    bitmapIndex ^= 1;
                    buffer.rewind();
                    bitmap.copyPixelsFromBuffer(buffer);
                    runOnUiThread(() -> {
                        if (running.get()) { preview.setImageBitmap(bitmap); previewPlaceholder.setVisibility(View.GONE); }
                        previewPending.set(false);
                    });
                }
                long now = SystemClock.elapsedRealtimeNanos();
                if (now - lastStats >= 500_000_000L) {
                    JSONObject metrics = camera.metrics();
                    publishStats(frames, frames / Math.max((now - started) / 1_000_000_000.0, 0.001), metrics, width, height);
                    lastStats = now;
                }
            }
        } catch (Throwable error) {
            if (running.get()) showError("采集失败", error);
        } finally {
            running.set(false);
            activeCamera = null;
            if (camera != null) {
                try { camera.stop(); } catch (Throwable ignored) { }
                try { camera.close(); } catch (Throwable ignored) { }
            }
            runOnUiThread(() -> { setStreamingUi(false); controlsPanel.setVisibility(View.GONE); showStatus("已停止"); });
        }
    }

    private void publishStats(long frames, double fps, JSONObject metrics, int width, int height) {
        String details = String.format(Locale.US, "后端帧: %d / 发布: %d\n池丢帧: %d / 订阅丢帧: %d\n转换错误: %d / P95: %.2f ms",
                metrics.optLong("received"), metrics.optLong("published"), metrics.optLong("poolDrops"),
                metrics.optLong("subscriberDrops"), metrics.optLong("conversionErrors"), metrics.optLong("conversionP95Ns") / 1_000_000.0);
        runOnUiThread(() -> {
            overlayFps.setText(String.format(Locale.US, "FPS: %.1f", fps));
            overlayFrames.setText("Frames: " + frames);
            overlayResolution.setText(width + " × " + height);
            stats.setText(details);
        });
    }

    private void stopCapture() {
        if (!running.getAndSet(false)) return;
        showStatus("正在停止…");
        MedivhCamera camera = activeCamera;
        if (camera != null) commandWorker.execute(() -> { try { camera.stop(); } catch (Throwable ignored) { } });
    }

    private void showControls(JSONArray descriptors) {
        controlDescriptors = descriptors;
        if (descriptors.length() == 0) { controlsPanel.setVisibility(View.GONE); return; }
        String[] labels = new String[descriptors.length()];
        for (int index = 0; index < labels.length; index++) {
            JSONObject descriptor = descriptors.optJSONObject(index);
            Object writable = descriptor == null ? null : descriptor.opt("writable");
            labels[index] = descriptor.optString("id") + " · " + descriptor.optString("unit")
                    + " · " + (Boolean.TRUE.equals(writable) ? "可写" : "只读/未知");
        }
        controlSpinner.setAdapter(adapter(labels));
        controlsPanel.setVisibility(View.VISIBLE);
        showControl(0);
    }

    private JSONObject selectedControl() {
        int position = controlSpinner.getSelectedItemPosition();
        return position < 0 ? null : controlDescriptors.optJSONObject(position);
    }

    private void showControl(int position) {
        JSONObject descriptor = controlDescriptors.optJSONObject(position);
        if (descriptor == null) return;
        JSONArray modes = descriptor.optJSONArray("modes");
        JSONObject range = descriptor.optJSONObject("range");
        Object writableValue = descriptor.opt("writable");
        boolean writable = writableValue == null || writableValue == JSONObject.NULL || descriptor.optBoolean("writable");
        if (modes != null && modes.length() > 0) {
            String[] values = new String[modes.length()];
            for (int index = 0; index < values.length; index++) values[index] = modes.optString(index);
            controlModeSpinner.setAdapter(adapter(values));
            controlModeSpinner.setVisibility(View.VISIBLE);
            controlSeekBar.setVisibility(View.GONE);
            controlRange.setText("模式: " + modes + requirement(descriptor));
        } else if (range != null) {
            controlModeSpinner.setVisibility(View.GONE);
            controlSeekBar.setVisibility(View.VISIBLE);
            controlSeekBar.setMax(CONTROL_SCALE);
            controlRange.setText(String.format(Locale.US, "范围: %s … %s，步进 %s%s", formatNumber(range.optDouble("min")),
                    formatNumber(range.optDouble("max")), formatNumber(range.optDouble("step", 1.0)), requirement(descriptor)));
        } else {
            controlModeSpinner.setVisibility(View.GONE);
            controlSeekBar.setVisibility(View.GONE);
            controlRange.setText("设备未报告可调范围" + requirement(descriptor));
        }
        controlModeSpinner.setEnabled(writable);
        controlSeekBar.setEnabled(writable);
        resetControlButton.setEnabled(writable && range != null && range.has("default") && !range.isNull("default"));
        refreshControl(descriptor);
    }

    private String requirement(JSONObject descriptor) {
        String required = descriptor.optString("requiresManual", "");
        return required.isEmpty() ? "" : "；需要 " + required + "=Manual";
    }

    private void refreshControl(JSONObject descriptor) {
        if (!descriptor.optBoolean("readable")) { controlValue.setText("不可读"); return; }
        MedivhCamera camera = activeCamera;
        if (camera == null) return;
        String id = descriptor.optString("id");
        commandWorker.execute(() -> {
            try {
                JSONObject readback = camera.control(id);
                runOnUiThread(() -> applyControlReadback(descriptor, readback));
            } catch (Throwable error) { showError("读取 " + id + " 失败", error); }
        });
    }

    private void applyControlReadback(JSONObject descriptor, JSONObject readback) {
        Object value = readback.opt("value");
        if (value == null || value == JSONObject.NULL) { controlValue.setText(readback.optString("kind", "Unknown")); return; }
        controlValue.setText(String.valueOf(value) + " (" + readback.optString("kind") + ")");
        if (value instanceof Number && descriptor.optJSONObject("range") != null) {
            controlSeekBar.setProgress(controlProgress(descriptor, ((Number) value).doubleValue()));
        } else if (value instanceof String && controlModeSpinner.getAdapter() instanceof ArrayAdapter) {
            @SuppressWarnings("unchecked") ArrayAdapter<String> values = (ArrayAdapter<String>) controlModeSpinner.getAdapter();
            int position = values.getPosition((String) value);
            if (position >= 0) controlModeSpinner.setSelection(position);
        }
    }

    private double controlNumber(JSONObject descriptor, int progress) {
        JSONObject range = descriptor.optJSONObject("range");
        if (range == null) return 0;
        double min = range.optDouble("min"), max = range.optDouble("max");
        double step = Math.max(range.optDouble("step", 1.0), 0.000001);
        double raw = min + (max - min) * progress / CONTROL_SCALE;
        return Math.min(max, min + Math.round((raw - min) / step) * step);
    }

    private int controlProgress(JSONObject descriptor, double value) {
        JSONObject range = descriptor.optJSONObject("range");
        if (range == null) return 0;
        double min = range.optDouble("min"), max = range.optDouble("max");
        return max <= min ? 0 : (int) Math.round((value - min) * CONTROL_SCALE / (max - min));
    }

    private void setControlValue(JSONObject descriptor, Object value) {
        MedivhCamera camera = activeCamera;
        if (camera == null) return;
        String id = descriptor.optString("id");
        commandWorker.execute(() -> {
            try {
                camera.setControl(id, value);
                refreshControl(descriptor);
                showStatus("已设置 " + id + " = " + value);
            } catch (Throwable error) { showError("设置 " + id + " 失败", error); }
        });
    }

    private void resetSelectedControl() {
        JSONObject descriptor = selectedControl();
        if (descriptor == null) return;
        JSONObject range = descriptor.optJSONObject("range");
        if (range != null && range.has("default") && !range.isNull("default")) setControlValue(descriptor, range.opt("default"));
    }

    private void resetAllControls() {
        MedivhCamera camera = activeCamera;
        if (camera == null) return;
        JSONObject selected = selectedControl();
        commandWorker.execute(() -> {
            int reset = 0;
            for (int index = 0; index < controlDescriptors.length(); index++) {
                JSONObject descriptor = controlDescriptors.optJSONObject(index);
                JSONObject range = descriptor == null ? null : descriptor.optJSONObject("range");
                if (range == null || !range.has("default") || range.isNull("default")) continue;
                try { camera.setControl(descriptor.optString("id"), range.opt("default")); reset++; }
                catch (Throwable error) { showError("重置 " + descriptor.optString("id") + " 失败", error); }
            }
            showStatus("已恢复 " + reset + " 个具有默认值的控制项");
            if (selected != null) refreshControl(selected);
        });
    }

    private void setStreamingUi(boolean streaming) {
        startButton.setEnabled(!streaming && captureModes.length() > 0);
        stopButton.setEnabled(streaming);
        startButton.setAlpha(streaming ? 0.5f : 1f);
        stopButton.setAlpha(streaming ? 1f : 0.5f);
        for (int index = 0; index < backendGroup.getChildCount(); index++) backendGroup.getChildAt(index).setEnabled(!streaming);
        deviceSpinner.setEnabled(!streaming);
        configSpinner.setEnabled(!streaming);
        statsOverlay.setVisibility(streaming ? View.VISIBLE : View.GONE);
        if (!streaming) {
            previewPending.set(false);
            preview.setImageDrawable(null);
            previewPlaceholder.setVisibility(View.VISIBLE);
        }
    }

    private void showStatus(String message) { runOnUiThread(() -> streamStatus.setText(message)); }
    private void showError(String prefix, Throwable error) { showStatus(prefix + "：" + structuredMessage(error)); }

    private String structuredMessage(Throwable error) {
        if (error instanceof CameraException) {
            CameraException cameraError = (CameraException) error;
            return cameraError.code() + " / " + cameraError.recovery()
                    + "：" + cameraError.diagnosticMessage();
        }
        String message = error.getMessage() == null ? error.toString() : error.getMessage();
        try {
            JSONObject payload = new JSONObject(message);
            return payload.optString("code", "error") + " / " + payload.optString("recovery", "none")
                    + "：" + payload.optString("message", message);
        } catch (Exception ignored) { return message; }
    }

    private String formatNumber(double value) {
        return Math.rint(value) == value ? String.format(Locale.US, "%.0f", value) : String.format(Locale.US, "%.3f", value);
    }
    private ArrayAdapter<String> adapter(String[] values) {
        return new ArrayAdapter<>(this, android.R.layout.simple_spinner_dropdown_item, values);
    }
    private ArrayAdapter<String> emptyAdapter() { return adapter(new String[0]); }

    @Override public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(request, permissions, results);
        if (request != CAMERA_PERMISSION_REQUEST) return;
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            if (autoScanEnabled) scanDevices();
        } else {
            showStatus(getString(R.string.camera_permission_required));
        }
    }
    @Override protected void onStop() { stopCapture(); super.onStop(); }
    @Override protected void onDestroy() {
        stopCapture();
        captureWorker.shutdown();
        commandWorker.shutdown();
        super.onDestroy();
    }

    private interface SelectionAction { void accept(int position); }
    private static final class SimpleItemSelectedListener implements android.widget.AdapterView.OnItemSelectedListener {
        private final SelectionAction action;
        SimpleItemSelectedListener(SelectionAction action) { this.action = action; }
        @Override public void onItemSelected(android.widget.AdapterView<?> parent, View view, int position, long id) { action.accept(position); }
        @Override public void onNothingSelected(android.widget.AdapterView<?> parent) { }
    }
}
