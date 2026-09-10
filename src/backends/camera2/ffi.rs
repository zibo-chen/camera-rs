//! FFI bindings for the NDK Camera2 C++ bridge layer.

use std::os::raw::c_void;

/// Opaque handle to the native camera2 instance.
#[repr(C)]
pub struct NdkCamera2 {
    _opaque: [u8; 0],
}

/// Camera device info returned from C.
#[repr(C)]
#[derive(Debug)]
pub struct NdkCameraDeviceInfo {
    pub id: *const libc::c_char,
    pub facing: i32,
    pub orientation: i32,
    pub available: bool,
}

/// Camera configuration.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct NdkCameraConfig {
    pub width: i32,
    pub height: i32,
    pub format: i32,
    pub fps: i32,
}

/// Frame data passed from C to Rust callback.
#[repr(C)]
#[derive(Debug)]
pub struct NdkFrameData {
    pub rgb_data: *const u8,
    pub rgb_len: i32,
    pub y_data: *const u8,
    pub y_len: i32,
    pub uv_data: *const u8,
    pub uv_len: i32,
    pub v_data: *const u8,
    pub v_len: i32,
    pub row_stride_v: i32,
    pub pixel_stride_v: i32,
    pub width: i32,
    pub height: i32,
    pub row_stride_y: i32,
    pub row_stride_uv: i32,
    pub pixel_stride_uv: i32,
    pub row_stride_rgb: i32,
    pub pixel_format: i32,
    pub timestamp_ns: i64,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NdkCameraPixelFormat {
    Unknown = 0,
    Yuv420 = 1,
    Rgba8888 = 2,
}

/// Status codes matching the C enum.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NdkCameraStatus {
    Ok = 0,
    ErrorInvalidParam = -1,
    ErrorOpenFailed = -2,
    ErrorSessionFailed = -3,
    ErrorNotFound = -4,
    ErrorPermission = -5,
    ErrorAlreadyStreaming = -6,
    ErrorNotStreaming = -7,
    ErrorInternal = -8,
}

impl NdkCameraStatus {
    pub fn error(self, operation: impl Into<String>) -> crate::CameraError {
        crate::CameraError::Native {
            backend: crate::BackendType::Camera2,
            operation: operation.into(),
            code: self as i32 as i64,
            message: self.to_error_string().into(),
        }
    }

    pub fn is_ok(self) -> bool {
        self == NdkCameraStatus::Ok
    }

    pub fn to_error_string(self) -> &'static str {
        match self {
            NdkCameraStatus::Ok => "OK",
            NdkCameraStatus::ErrorInvalidParam => "Invalid parameter",
            NdkCameraStatus::ErrorOpenFailed => "Failed to open camera",
            NdkCameraStatus::ErrorSessionFailed => "Failed to create capture session",
            NdkCameraStatus::ErrorNotFound => "Camera not found",
            NdkCameraStatus::ErrorPermission => "Permission denied",
            NdkCameraStatus::ErrorAlreadyStreaming => "Already streaming",
            NdkCameraStatus::ErrorNotStreaming => "Not streaming",
            NdkCameraStatus::ErrorInternal => "Internal error",
        }
    }
}

/// Frame callback type.
pub type NdkCameraFrameCallback =
    unsafe extern "C" fn(context: *mut c_void, frame: *const NdkFrameData);

extern "C" {
    // Lifecycle
    pub fn ndk_camera2_create() -> *mut NdkCamera2;
    pub fn ndk_camera2_destroy(cam: *mut NdkCamera2);

    // Device enumeration
    pub fn ndk_camera2_get_device_count(
        cam: *mut NdkCamera2,
        out_count: *mut i32,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_get_device_info(
        cam: *mut NdkCamera2,
        index: i32,
        out_info: *mut NdkCameraDeviceInfo,
    ) -> NdkCameraStatus;

    // Configuration query
    pub fn ndk_camera2_get_config_count(
        cam: *mut NdkCamera2,
        device_index: i32,
        out_count: *mut i32,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_get_config(
        cam: *mut NdkCamera2,
        device_index: i32,
        config_index: i32,
        out_config: *mut NdkCameraConfig,
    ) -> NdkCameraStatus;

    // Streaming
    pub fn ndk_camera2_start_stream(
        cam: *mut NdkCamera2,
        device_index: i32,
        config: *const NdkCameraConfig,
        frame_cb: NdkCameraFrameCallback,
        callback_context: *mut c_void,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_get_active_config(
        cam: *mut NdkCamera2,
        out: *mut NdkCameraConfig,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_stop_stream(cam: *mut NdkCamera2) -> NdkCameraStatus;

    pub fn ndk_camera2_is_streaming(cam: *mut NdkCamera2) -> bool;

    // Camera controls
    pub fn ndk_camera2_get_control_modes(
        cam: *mut NdkCamera2,
        kind: i32,
        mask: *mut u32,
    ) -> NdkCameraStatus;
    pub fn ndk_camera2_set_exposure_compensation(
        cam: *mut NdkCamera2,
        value: i32,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_get_exposure_compensation_range(
        cam: *mut NdkCamera2,
        out_min: *mut i32,
        out_max: *mut i32,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_set_ae_mode(cam: *mut NdkCamera2, mode: i32) -> NdkCameraStatus;

    pub fn ndk_camera2_set_af_mode(cam: *mut NdkCamera2, mode: i32) -> NdkCameraStatus;

    pub fn ndk_camera2_set_awb_mode(cam: *mut NdkCamera2, mode: i32) -> NdkCameraStatus;

    pub fn ndk_camera2_set_zoom(cam: *mut NdkCamera2, zoom_100: i32) -> NdkCameraStatus;

    pub fn ndk_camera2_get_zoom_range(
        cam: *mut NdkCamera2,
        out_min: *mut i32,
        out_max: *mut i32,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_get_sensitivity_range(
        cam: *mut NdkCamera2,
        out_min: *mut i32,
        out_max: *mut i32,
    ) -> NdkCameraStatus;

    pub fn ndk_camera2_set_sensitivity(cam: *mut NdkCamera2, iso: i32) -> NdkCameraStatus;
}
