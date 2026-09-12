package com.medivh.camera;

import android.content.Context;
import java.nio.ByteBuffer;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/** Owned camera handle. Use try-with-resources on a worker thread. */
public final class MedivhCamera implements AutoCloseable {
    private long handle;

    public static void initWithContext(Context context) {
        MedivhCameraBridge.nativeInitWithContext(context.getApplicationContext());
    }
    public static JSONArray devices(String backend) throws JSONException {
        return new JSONArray(MedivhCameraBridge.nativeDevices(backend));
    }
    public static JSONObject capabilities(String backend, String deviceId) throws JSONException {
        return new JSONObject(MedivhCameraBridge.nativeCapabilities(backend, deviceId));
    }
    public static boolean hasUsbPermission(String path) {
        return MedivhCameraBridge.nativeHasPermission(path);
    }
    public static void requestUsbPermission(String path) {
        MedivhCameraBridge.nativeRequestPermission(path);
    }
    public MedivhCamera(String backend, String deviceId) {
        handle = MedivhCameraBridge.nativeOpen(backend, deviceId);
    }
    public JSONObject start(int width, int height, int fps) throws JSONException {
        return new JSONObject(MedivhCameraBridge.nativeStart(handle, width, height, fps, 1, false));
    }
    public JSONObject start(String format, int width, int height, int fpsNumerator,
                            int fpsDenominator) throws JSONException {
        return new JSONObject(MedivhCameraBridge.nativeStartWithFormat(
                handle, width, height, fpsNumerator, fpsDenominator, format, false));
    }
    public JSONObject nextFrame(ByteBuffer buffer, int timeoutMs) throws JSONException {
        return new JSONObject(MedivhCameraBridge.nativeNextFrame(handle, buffer, timeoutMs));
    }
    public JSONObject nextFrameArgb(ByteBuffer buffer, int timeoutMs) throws JSONException {
        return new JSONObject(MedivhCameraBridge.nativeNextFrameArgb(handle, buffer, timeoutMs));
    }
    public JSONObject control(String id) throws JSONException {
        return new JSONObject(MedivhCameraBridge.nativeControl(handle, id));
    }
    public JSONObject metrics() throws JSONException {
        return new JSONObject(MedivhCameraBridge.nativeMetrics(handle));
    }
    public JSONArray controls() throws JSONException {
        return new JSONArray(MedivhCameraBridge.nativeControls(handle));
    }
    public void setControl(String id, Object value) {
        String encoded = value instanceof String ? JSONObject.quote((String) value) : value.toString();
        MedivhCameraBridge.nativeSetControl(handle, id, encoded);
    }
    public void stop() {
        if (handle != 0) MedivhCameraBridge.nativeStop(handle);
    }
    @Override public void close() {
        if (handle != 0) {
            long old = handle;
            handle = 0;
            MedivhCameraBridge.nativeClose(old);
        }
    }
}
