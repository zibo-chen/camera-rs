package com.medivh.camera;

import android.content.Context;
import java.nio.ByteBuffer;

/** JNI adapter. All capture and control calls are blocking and belong on a worker thread. */
public final class MedivhCameraBridge {
    static { System.loadLibrary("camera_android"); }
    private MedivhCameraBridge() {}

    public static native void nativeInitWithContext(Context context);
    public static native String nativeDevices(String backend);
    public static native String nativeCapabilities(String backend, String deviceId);
    public static native boolean nativeHasPermission(String usbPath);
    public static native void nativeRequestPermission(String usbPath);
    public static native long nativeOpen(String backend, String deviceId);
    public static native long nativeOpenUsbFd(int borrowedFd);
    public static native String nativeStart(long handle, int width, int height,
            int fpsNumerator, int fpsDenominator, boolean nativeOutput);
    public static native String nativeStartWithFormat(long handle, int width, int height,
            int fpsNumerator, int fpsDenominator, String format, boolean nativeOutput);
    /** Buffer must be direct, writable and exclusively owned during this call. */
    public static native String nativeNextFrame(long handle, ByteBuffer buffer, int timeoutMs);
    /** Writes native-endian ARGB words for a reusable Bitmap.Config.ARGB_8888 preview. */
    public static native String nativeNextFrameArgb(long handle, ByteBuffer buffer, int timeoutMs);
    public static native void nativeStop(long handle);
    public static native void nativeClose(long handle);
    public static native String nativeControl(long handle, String id);
    public static native String nativeControls(long handle);
    public static native String nativeMetrics(long handle);
    /** Value is a JSON number in the descriptor's unit, or a quoted mode name. */
    public static native void nativeSetControl(long handle, String id, String jsonValue);
}
