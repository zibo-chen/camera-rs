package com.medivh.camera;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraManager;
import android.os.Bundle;
import android.os.Debug;
import android.os.SystemClock;
import androidx.test.platform.app.InstrumentationRegistry;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.json.JSONArray;
import org.json.JSONObject;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.ByteBuffer;
import java.util.HashSet;
import java.util.Set;
import static org.junit.Assert.*;

/** Physical-device contracts. Run serially per device; no camera is selected implicitly by product ID. */
@RunWith(AndroidJUnit4.class)
public final class BackendInstrumentedTest {
    private static void record(JSONObject result) {
        Bundle status = new Bundle();
        status.putString("camera_result", result.toString());
        InstrumentationRegistry.getInstrumentation().sendStatus(0, status);
    }
    private static void mustFail(Runnable operation) {
        try { operation.run(); fail("Expected IllegalStateException"); }
        catch (IllegalStateException expected) { /* Contractually reported failure. */ }
    }
    private static JSONObject findControl(JSONArray descriptors, String id) throws Exception {
        for (int i = 0; i < descriptors.length(); i++)
            if (descriptors.getJSONObject(i).getString("id").equals(id)) return descriptors.getJSONObject(i);
        return null;
    }
    private static boolean has(int[] values, int value) {
        if (values != null) for (int candidate : values) if (candidate == value) return true;
        return false;
    }
    private static JSONArray devicesWithHotplugRetry(String backend) throws Exception {
        int attempts = backend.equals("uvc") ? 1 : 40;
        JSONArray devices = new JSONArray();
        for (int attempt = 0; attempt < attempts; attempt++) {
            devices = new JSONArray(MedivhCameraBridge.nativeDevices(backend));
            if (devices.length() > 0 || attempt + 1 == attempts) return devices;
            SystemClock.sleep(250);
        }
        return devices;
    }
    private static void validateCamera2Controls(Context context, String id, long handle, JSONArray descriptors) throws Exception {
        CameraCharacteristics c = ((CameraManager)context.getSystemService(Context.CAMERA_SERVICE)).getCameraCharacteristics(id);
        JSONObject ae = findControl(descriptors, "ExposureMode");
        if (ae != null) {
            boolean manual = ae.getJSONArray("modes").toString().contains("Manual");
            assertEquals("Camera2 advertised unsupported manual exposure", has(c.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_MODES),0), manual);
            if (!manual) mustFail(() -> MedivhCameraBridge.nativeSetControl(handle, "ExposureMode", "\"Manual\""));
        }
        JSONObject awb = findControl(descriptors, "WhiteBalanceMode");
        if (awb != null) assertEquals("Camera2 advertised unsupported manual white balance",
            has(c.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES),0), awb.getJSONArray("modes").toString().contains("Manual"));
        if (c.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE) == null)
            assertNull("Camera2 advertised a sensitivity control absent from metadata", findControl(descriptors,"Gain"));
        JSONObject af = findControl(descriptors, "FocusMode");
        if (af != null) {
            int[] modes = c.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES);
            JSONArray advertised = af.getJSONArray("modes");
            assertEquals(has(modes,1),advertised.toString().contains("Single"));
            assertEquals(has(modes,3)||has(modes,4),advertised.toString().contains("Continuous"));
            if (has(modes,1)) MedivhCameraBridge.nativeSetControl(handle,"FocusMode","\"Single\"");
            if (has(modes,0)) MedivhCameraBridge.nativeSetControl(handle,"FocusMode","\"Manual\"");
        }
    }
    private static void validateBrightness(long handle, JSONArray controls) throws Exception {
        JSONObject brightness = findControl(controls,"Brightness");
        if (brightness == null || brightness.isNull("range") || !brightness.getBoolean("readable")) return;
        JSONObject before = new JSONObject(MedivhCameraBridge.nativeControl(handle,"Brightness"));
        assertEquals("Actual",before.getString("kind"));
        double original = before.getDouble("value");
        JSONObject range = brightness.getJSONObject("range");
        double step = range.getDouble("step");
        double changed = original + step <= range.getDouble("max") ? original + step : original - step;
        if (changed < range.getDouble("min")) return;
        try {
            MedivhCameraBridge.nativeSetControl(handle,"Brightness",Double.toString(changed));
            JSONObject read = new JSONObject(MedivhCameraBridge.nativeControl(handle,"Brightness"));
            assertEquals("Actual",read.getString("kind"));
            assertEquals(changed,read.getDouble("value"),0.0);
        } finally { MedivhCameraBridge.nativeSetControl(handle,"Brightness",Double.toString(original)); }
        assertEquals(original,new JSONObject(MedivhCameraBridge.nativeControl(handle,"Brightness")).getDouble("value"),0.0);
        record(new JSONObject().put("brightnessWriteReadRestore",true));
    }
    @Test public void captureLifecycleAndBuffers() throws Exception {
        int cycles = Integer.parseInt(InstrumentationRegistry.getArguments().getString("cycles", "1"));
        Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
        Intent intent = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        assertNotNull(intent);
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        intent.putExtra("skipAutoScan", true);
        Activity activity = InstrumentationRegistry.getInstrumentation().startActivitySync(intent);
        MedivhCameraBridge.nativeInitWithContext(context);
        int baselineFds = -1;
        try {
        for (int cycle = 0; cycle < cycles; cycle++) {
            captureCycle();
            SystemClock.sleep(100);
            int fds = new File("/proc/self/fd").list().length;
            if (baselineFds < 0) baselineFds = fds;
            JSONObject types = new JSONObject();
            for (String fd : new File("/proc/self/fd").list()) {
                try {
                    String target = android.system.Os.readlink("/proc/self/fd/"+fd);
                    if (target.startsWith("socket:")) target = "socket";
                    if (target.startsWith("/dev/bus/usb/")) target = "usb";
                    types.put(target,types.optInt(target,0)+1);
                } catch (android.system.ErrnoException gone) { /* descriptor closed during sampling */ }
            }
            record(new JSONObject().put("cycle",cycle).put("fdsAfterClose",fds).put("fdTypes",types));
            assertTrue("Descriptors grow over repeated open/close: " + baselineFds + " -> " + fds, fds <= baselineFds + 8);
        }
        } finally { InstrumentationRegistry.getInstrumentation().runOnMainSync(activity::finish); }
    }
    private void captureCycle() throws Exception {
        Bundle args = InstrumentationRegistry.getArguments();
        Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
        String backend = args.getString("backend", "camera2");
        int width = Integer.parseInt(args.getString("width", "640"));
        int height = Integer.parseInt(args.getString("height", "480"));
        int fps = Integer.parseInt(args.getString("fps", "30"));
        int count = Integer.parseInt(args.getString("frames", "60"));
        int delayMs = Integer.parseInt(args.getString("delayMs", "0"));
        int rounds = Integer.parseInt(args.getString("rounds", "3"));
        boolean nativeOutput = Boolean.parseBoolean(args.getString("native", "false"));
        long handle = 0;
        try {
            int frameworkBefore = devicesWithHotplugRetry("camera2").length();
            if (Boolean.parseBoolean(args.getString("expectPermissionDenied", "false"))) {
                assertEquals("v4l2", backend);
                try { MedivhCameraBridge.nativeDevices(backend); fail("Expected denied V4L2 nodes"); }
                catch (IllegalStateException e) {
                    assertTrue(e.getMessage(), e.getMessage().toLowerCase().contains("permission"));
                    record(new JSONObject().put("backend",backend).put("expectedPermissionDenied",e.getMessage()));
                }
                return;
            }
            JSONArray devices = devicesWithHotplugRetry(backend);
            record(new JSONObject().put("backend", backend).put("devices", devices));
            assertTrue("No " + backend + " camera", devices.length() > 0);
            String id = args.getString("deviceId", devices.getJSONObject(0).getString("id"));
            if (backend.equals("uvc") && !MedivhCameraBridge.nativeHasPermission(id)) {
                MedivhCameraBridge.nativeRequestPermission(id);
                long deadline = SystemClock.elapsedRealtime() + 60000;
                while (!MedivhCameraBridge.nativeHasPermission(id) && SystemClock.elapsedRealtime() < deadline)
                    SystemClock.sleep(250);
                assertTrue("USB permission was not granted", MedivhCameraBridge.nativeHasPermission(id));
            }
            JSONObject capabilities = new JSONObject(MedivhCameraBridge.nativeCapabilities(backend, id));
            JSONArray modes = capabilities.getJSONArray("modes");
            assertTrue("No advertised capture modes for " + backend, modes.length() > 0);
            record(new JSONObject().put("backend", backend).put("capabilities", capabilities));
            handle = MedivhCameraBridge.nativeOpen(backend, id);
            assertTrue(handle > 0);
            final long activeHandle = handle;
            long lastSession = -1;
            int warmFds = -1;
            for (int round = 0; round < rounds; round++) {
                JSONObject selected = modes.getJSONObject(0);
                String format = args.getString("format", selected.getString("format"));
                JSONObject config = new JSONObject(MedivhCameraBridge.nativeStartWithFormat(handle, width, height, fps, 1, format, nativeOutput));
                int w = config.getInt("width"), h = config.getInt("height");
                assertEquals(width, w);
                assertEquals(height, h);
                mustFail(() -> MedivhCameraBridge.nativeStart(activeHandle, width, height, fps, 1, nativeOutput));
                ByteBuffer bytes = ByteBuffer.allocateDirect(Math.multiplyExact(Math.multiplyExact(w, h), 4) + 65536);
                mustFail(() -> MedivhCameraBridge.nativeNextFrame(activeHandle, ByteBuffer.allocateDirect(1), 3000));
                mustFail(() -> MedivhCameraBridge.nativeNextFrame(activeHandle, ByteBuffer.allocate(16), 3000));
                mustFail(() -> MedivhCameraBridge.nativeNextFrame(activeHandle, bytes.asReadOnlyBuffer(), 3000));
                Set<Long> hashes = new HashSet<>();
                long sequence = -1, currentSession = -1;
                long begin = SystemClock.elapsedRealtimeNanos();
                long captureEnd = begin;
                for (int frame = 0; frame < count; frame++) {
                    JSONObject metadata = new JSONObject(MedivhCameraBridge.nativeNextFrame(handle, bytes, 3000));
                    long session = metadata.getLong("session"), nextSequence = metadata.getLong("sequence");
                    if (currentSession < 0) currentSession = session;
                    assertEquals(currentSession, session);
                    assertTrue("Receiver repeated an old frame", nextSequence > sequence);
                    sequence = nextSequence;
                    assertEquals(w, metadata.getInt("width"));
                    assertEquals(h, metadata.getInt("height"));
                    int length = metadata.getInt("byteLength");
                    assertTrue(length > 0 && length <= bytes.capacity());
                    long hash = 1;
                    for (int i = 0; i < length; i += Math.max(1, length / 4096)) hash = hash * 31 + (bytes.get(i) & 255);
                    hashes.add(hash);
                    captureEnd = SystemClock.elapsedRealtimeNanos();
                    if (frame == 0) {
                        record(new JSONObject().put("backend",backend).put("round",round).put("config",config).put("firstFrame",metadata));
                    }
                    if (frame == count - 1) {
                        if (round == 0 && !nativeOutput && metadata.getString("pixelFormat").equals("Rgb8")) {
                            File output = new File(context.getFilesDir(), "probe-" + backend + ".ppm");
                            try (FileOutputStream file = new FileOutputStream(output)) {
                                file.write(("P6\n" + w + " " + h + "\n255\n").getBytes("US-ASCII"));
                                byte[] row = new byte[w * 3];
                                for (int y = 0; y < h; y++) { bytes.position(y * row.length); bytes.get(row); file.write(row); }
                                bytes.clear();
                            }
                        }
                    }
                    if (delayMs > 0) SystemClock.sleep(delayMs);
                }
                assertNotEquals("Restart reused an old capture epoch", lastSession, currentSession);
                lastSession = currentSession;
                JSONObject metrics = new JSONObject(MedivhCameraBridge.nativeMetrics(handle));
                double elapsed = (captureEnd - begin) / 1e9;
                JSONArray controls = new JSONArray(MedivhCameraBridge.nativeControls(handle));
                if (backend.equals("camera2")) validateCamera2Controls(context,id,handle,controls);
                if (backend.equals("uvc") || backend.equals("v4l2")) validateBrightness(handle,controls);
                record(new JSONObject().put("backend", backend).put("round", round).put("frames", count)
                    .put("elapsedSeconds", elapsed).put("observedFps", (count - 1) / elapsed)
                    .put("uniqueFrameHashes", hashes.size()).put("metrics", metrics).put("controls", controls)
                    .put("nativeHeapBytes", Debug.getNativeHeapAllocatedSize()));
                assertEquals("Invalid source frames", 0, metrics.getLong("conversionErrors"));
                assertTrue("Pixel pool exceeded its default budget", metrics.getLong("allocatedBytes") <= 192L * 1024 * 1024);
                java.util.concurrent.CountDownLatch waiting = new java.util.concurrent.CountDownLatch(1);
                java.util.concurrent.atomic.AtomicReference<Throwable> readerFailure = new java.util.concurrent.atomic.AtomicReference<>();
                Thread reader = new Thread(() -> {
                    waiting.countDown();
                    try {
                        while (true) MedivhCameraBridge.nativeNextFrame(activeHandle, bytes, 3000);
                    } catch (IllegalStateException stopped) {
                        if (!stopped.getMessage().toLowerCase().contains("stop") && !stopped.getMessage().toLowerCase().contains("session") && !stopped.getMessage().toLowerCase().contains("not streaming"))
                            readerFailure.set(stopped);
                    } catch (Throwable error) { readerFailure.set(error); }
                },"camera-stop-reader");
                reader.start();
                assertTrue(waiting.await(1,java.util.concurrent.TimeUnit.SECONDS));
                SystemClock.sleep(50);
                long stopStart = SystemClock.elapsedRealtime();
                MedivhCameraBridge.nativeStop(handle);
                reader.join(3000);
                assertFalse("Stop failed to wake a concurrent reader",reader.isAlive());
                assertNull("Concurrent reader failed: " + readerFailure.get(),readerFailure.get());
                record(new JSONObject().put("concurrentStopMs",SystemClock.elapsedRealtime()-stopStart));
                MedivhCameraBridge.nativeStop(handle);
                mustFail(() -> MedivhCameraBridge.nativeNextFrame(activeHandle, bytes, 100));
                int fds = new File("/proc/self/fd").list().length;
                if (warmFds < 0) warmFds = fds;
                assertTrue("Descriptors grow over repeated stop/start", fds <= warmFds + 8);
                record(new JSONObject().put("backend", backend).put("round", round).put("fdsAfterStop", fds));
            }
            MedivhCameraBridge.nativeClose(handle);
            handle = 0;
            MedivhCameraBridge.nativeClose(activeHandle);
            mustFail(() -> MedivhCameraBridge.nativeStart(activeHandle, width, height, fps, 1, nativeOutput));
            if (backend.equals("uvc") && frameworkBefore > 0) {
                long deadline = SystemClock.elapsedRealtime() + 10000;
                int frameworkAfter;
                do {
                    frameworkAfter = new JSONArray(MedivhCameraBridge.nativeDevices("camera2")).length();
                    if (frameworkAfter >= frameworkBefore) break;
                    SystemClock.sleep(100);
                } while (SystemClock.elapsedRealtime() < deadline);
                boolean requireHandoff = Boolean.parseBoolean(args.getString("requireCamera2Handoff", "true"));
                record(new JSONObject().put("backend",backend).put("camera2DevicesAfterClose",frameworkAfter)
                    .put("camera2HandoffRestored",frameworkAfter >= frameworkBefore).put("requireCamera2Handoff",requireHandoff));
                if (requireHandoff) assertTrue("UVC close did not restore the kernel/Camera2 camera", frameworkAfter >= frameworkBefore);
            }
        } finally {
            if (handle != 0) MedivhCameraBridge.nativeClose(handle);
        }
    }
}
