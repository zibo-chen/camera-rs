package com.medivh.camera.demo;

import android.app.Application;
import android.util.Log;

import com.medivh.camera.MedivhCamera;

/**
 * Medivh Camera Demo Application
 */
public class MedivhCameraApp extends Application {
    private static final String TAG = "MedivhCameraApp";
    
    @Override
    public void onCreate() {
        super.onCreate();
        
        // 初始化 Medivh Camera（使用带 Context 的新方法）
        try {
            MedivhCamera.initWithContext(this);
            Log.i(TAG, "Medivh Camera library initialized with context");
        } catch (UnsatisfiedLinkError e) {
            Log.e(TAG, "Failed to load native library: " + e.getMessage());
        }
    }
}
