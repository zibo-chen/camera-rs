//! FFI 绑定到 libuvc C 库

use libc::{c_char, c_int, c_void, size_t, timespec, timeval};

// ============================================================================
// 错误码定义
// ============================================================================

pub type UvcError = c_int;

pub const UVC_SUCCESS: UvcError = 0;
pub const UVC_ERROR_IO: UvcError = -1;
pub const UVC_ERROR_INVALID_PARAM: UvcError = -2;
pub const UVC_ERROR_ACCESS: UvcError = -3;
pub const UVC_ERROR_NO_DEVICE: UvcError = -4;
pub const UVC_ERROR_NOT_FOUND: UvcError = -5;
pub const UVC_ERROR_BUSY: UvcError = -6;
pub const UVC_ERROR_TIMEOUT: UvcError = -7;
pub const UVC_ERROR_OVERFLOW: UvcError = -8;
pub const UVC_ERROR_PIPE: UvcError = -9;
pub const UVC_ERROR_INTERRUPTED: UvcError = -10;
pub const UVC_ERROR_NO_MEM: UvcError = -11;
pub const UVC_ERROR_NOT_SUPPORTED: UvcError = -12;

// ============================================================================
// 帧格式枚举
// 必须与 libuvc/include/libuvc/libuvc.h 中的 enum uvc_frame_format 保持一致
// 经过 C 编译器验证的正确值：
//   UNKNOWN=0, UNCOMPRESSED=1, COMPRESSED=2, YUYV=3, UYVY=4,
//   RGB=5, BGR=6, MJPEG=7, H264=8, GRAY8=9, GRAY16=10
// ============================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvcFrameFormat {
    Unknown = 0,
    // Any = 0,  // 与 Unknown 相同
    Uncompressed = 1,
    Compressed = 2,
    Yuyv = 3,
    Uyvy = 4,
    Rgb = 5,
    Bgr = 6,
    Mjpeg = 7,
    H264 = 8,
    Gray8 = 9,
    Gray16 = 10,
    // 以下是 Bayer 格式
    By8 = 11,
    Ba81 = 12,
    Sgrbg8 = 13,
    Sgbrg8 = 14,
    Srggb8 = 15,
    Sbggr8 = 16,
    // YUV420
    Nv12 = 17,
    P010 = 18,
    // Count (用于边界检查)
    // Count = 19,
}

// ============================================================================
// UVC 请求代码枚举
// ============================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvcReqCode {
    Undefined = 0x00,
    SetCur = 0x01,
    GetCur = 0x81,
    GetMin = 0x82,
    GetMax = 0x83,
    GetRes = 0x84,
    GetLen = 0x85,
    GetInfo = 0x86,
    GetDef = 0x87,
}

// ============================================================================
// 不透明结构体（仅声明）
// ============================================================================

#[repr(C)]
pub struct UvcContext {
    _private: [u8; 0],
}

#[repr(C)]
pub struct UvcDevice {
    _private: [u8; 0],
}

#[repr(C)]
pub struct UvcDeviceHandle {
    _private: [u8; 0],
}

/// 流控制结构体
/// 必须与 libuvc 中的 uvc_stream_ctrl_t 完全匹配
/// 总大小: 40 字节 (经 C 编译器验证)
#[repr(C)]
pub struct UvcStreamCtrl {
    pub bm_hint: u16,            // offset 0
    pub b_format_index: u8,      // offset 2
    pub b_frame_index: u8,       // offset 3
    pub dw_frame_interval: u32,  // offset 4
    pub w_key_frame_rate: u16,   // offset 8
    pub w_p_frame_rate: u16,     // offset 10
    pub w_comp_quality: u16,     // offset 12
    pub w_comp_window_size: u16, // offset 14
    pub w_delay: u16,            // offset 16
    // implicit 2-byte padding here        // offset 18-19
    pub dw_max_video_frame_size: u32,      // offset 20
    pub dw_max_payload_transfer_size: u32, // offset 24
    pub dw_clock_frequency: u32,           // offset 28
    pub bm_framing_info: u8,               // offset 32
    pub b_preferred_version: u8,           // offset 33
    pub b_min_version: u8,                 // offset 34
    pub b_max_version: u8,                 // offset 35
    pub b_interface_number: u8,            // offset 36
                                           // implicit 3-byte tail padding        // offset 37-39
}

#[repr(C)]
pub struct LibusbContext {
    _private: [u8; 0],
}

// ============================================================================
// 格式和帧描述符
// ============================================================================

#[repr(C)]
pub struct UvcFormatDesc {
    pub parent: *mut c_void,
    pub prev: *mut UvcFormatDesc,
    pub next: *mut UvcFormatDesc,
    pub b_descriptor_subtype: u32, // enum in C is 4 bytes
    pub b_format_index: u8,
    pub b_num_frame_descriptors: u8,
    pub guid_format: [u8; 16],
    pub b_bits_per_pixel: u8,
    pub b_default_frame_index: u8,
    pub b_aspect_ratio_x: u8,
    pub b_aspect_ratio_y: u8,
    pub bm_interlace_flags: u8,
    pub b_copy_protect: u8,
    pub b_variable_size: u8,
    pub frame_descs: *mut UvcFrameDesc,
    pub still_frame_desc: *mut c_void,
}

#[repr(C)]
pub struct UvcFrameDesc {
    pub parent: *mut UvcFormatDesc,
    pub prev: *mut UvcFrameDesc,
    pub next: *mut UvcFrameDesc,
    pub b_descriptor_subtype: u32, // enum in C is 4 bytes
    pub b_frame_index: u8,
    pub bm_capabilities: u8,
    // _padding: u8, // No explicit padding needed if we use u32 for subtype, but alignment might add it
    pub w_width: u16,
    pub w_height: u16,
    pub dw_min_bit_rate: u32,
    pub dw_max_bit_rate: u32,
    pub dw_max_video_frame_buffer_size: u32,
    pub dw_default_frame_interval: u32,
    pub dw_min_frame_interval: u32,
    pub dw_max_frame_interval: u32,
    pub dw_frame_interval_step: u32,
    pub b_frame_interval_type: u8,
    // _padding2: [u8; 3],
    pub dw_bytes_per_line: u32,
    pub intervals: *mut u32,
}

// ============================================================================
// 设备描述符
// ============================================================================

#[repr(C)]
pub struct UvcDeviceDescriptor {
    pub vendor_id: u16,
    pub product_id: u16,
    pub bcd_uvc: u16,
    pub serial_number: *const c_char,
    pub manufacturer: *const c_char,
    pub product: *const c_char,
}

// ============================================================================
// 帧结构体
// 必须与 libuvc/include/libuvc/libuvc.h 中的 uvc_frame 结构体完全匹配
// 在 macOS ARM64 上：
//   - size_t = 8 bytes
//   - uint32_t = 4 bytes
//   - timeval = 16 bytes (tv_sec: i64, tv_usec: i32 + 4 padding)
//   - timespec = 16 bytes (tv_sec: i64, tv_nsec: i64)
//   - 指针 = 8 bytes
// ============================================================================

#[repr(C)]
pub struct UvcFrame {
    /// Image data for this frame
    pub data: *mut c_void, // offset 0, 8 bytes
    /// Size of image data buffer
    pub data_bytes: size_t, // offset 8, 8 bytes
    /// Width of image in pixels
    pub width: u32, // offset 16, 4 bytes
    /// Height of image in pixels
    pub height: u32, // offset 20, 4 bytes
    /// Pixel data format
    pub frame_format: UvcFrameFormat, // offset 24, 4 bytes (enum)
    /// Padding for alignment
    _pad1: u32, // offset 28, 4 bytes padding
    /// Number of bytes per horizontal line (undefined for compressed format)
    pub step: size_t, // offset 32, 8 bytes
    /// Frame number (may skip, but is strictly monotonically increasing)
    pub sequence: u32, // offset 40, 4 bytes
    /// Padding for alignment before timeval
    _pad2: u32, // offset 44, 4 bytes padding
    /// Estimate of system time when the device started capturing the image
    pub capture_time: timeval, // offset 48, 16 bytes
    /// Estimate of system time when the device finished receiving the image
    pub capture_time_finished: timespec, // offset 64, 16 bytes
    /// Handle on the device that produced the image
    pub source: *mut UvcDeviceHandle, // offset 80, 8 bytes
    /// Is the data buffer owned by the library?
    pub library_owns_data: u8, // offset 88, 1 byte
    /// Padding for alignment
    _pad3: [u8; 7], // offset 89, 7 bytes padding
    /// Metadata for this frame if available
    pub metadata: *mut c_void, // offset 96, 8 bytes
    /// Metadata size in bytes
    pub metadata_bytes: size_t, // offset 104, 8 bytes
                                // Total size: 112 bytes
}

// ============================================================================
// 流控制结构体
// ============================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UvcStreamCtrlParams {
    pub bm_hint: u16,
    pub b_format_index: u8,
    pub b_frame_index: u8,
    pub dw_frame_interval: u32,
    pub w_key_frame_rate: u16,
    pub w_p_frame_rate: u16,
    pub w_comp_quality: u16,
    pub w_comp_window_size: u16,
    pub w_delay: u16,
    pub dw_max_video_frame_size: u32,
    pub dw_max_payload_transfer_size: u32,
}

// ============================================================================
// 回调函数类型
// ============================================================================

pub type UvcFrameCallback = extern "C" fn(frame: *mut UvcFrame, user_ptr: *mut c_void);

pub type UvcStatusCallback = extern "C" fn(
    status_class: c_int,
    event: c_int,
    selector: c_int,
    status_attribute: c_int,
    data: *mut c_void,
    data_len: size_t,
    user_ptr: *mut c_void,
);

pub type UvcButtonCallback = extern "C" fn(button: c_int, state: c_int, user_ptr: *mut c_void);

// ============================================================================
// libusb 选项（用于 Android 支持）
// ============================================================================

/// libusb 选项枚举
#[repr(C)]
pub enum LibusbOption {
    /// 使用 UsbDk 后端（仅 Windows）
    UseUsbdk = 1,
    /// 不进行设备发现（Android 需要）
    NoDeviceDiscovery = 2,
}

// ============================================================================
// FFI 函数声明
// ============================================================================

extern "C" {
    // libusb 选项设置（Android 需要）
    /// 设置 libusb 选项
    /// 当 ctx 为 NULL 时，设置全局默认选项
    pub fn libusb_set_option(ctx: *mut LibusbContext, option: LibusbOption, ...) -> c_int;

    // 上下文管理
    pub fn uvc_init(ctx: *mut *mut UvcContext, usb_ctx: *mut LibusbContext) -> UvcError;
    pub fn uvc_init2(ctx: *mut *mut UvcContext, usb_ctx: *mut LibusbContext) -> UvcError;
    pub fn uvc_exit(ctx: *mut UvcContext);

    // 设备发现
    pub fn uvc_find_device(
        ctx: *mut UvcContext,
        dev: *mut *mut UvcDevice,
        vid: c_int,
        pid: c_int,
        sn: *const c_char,
    ) -> UvcError;

    pub fn uvc_get_device_list(ctx: *mut UvcContext, list: *mut *mut *mut UvcDevice) -> UvcError;

    pub fn uvc_free_device_list(list: *mut *mut UvcDevice, unref_devices: u8);

    // 设备描述符
    pub fn uvc_get_device_descriptor(
        dev: *mut UvcDevice,
        desc: *mut *mut UvcDeviceDescriptor,
    ) -> UvcError;

    pub fn uvc_free_device_descriptor(desc: *mut UvcDeviceDescriptor);

    // 设备引用
    pub fn uvc_ref_device(dev: *mut UvcDevice);
    pub fn uvc_unref_device(dev: *mut UvcDevice);

    // 设备打开/关闭
    pub fn uvc_open(dev: *mut UvcDevice, devh: *mut *mut UvcDeviceHandle) -> UvcError;
    pub fn uvc_close(devh: *mut UvcDeviceHandle);

    // Android 文件描述符支持
    pub fn uvc_wrap(fd: c_int, ctx: *mut UvcContext, devh: *mut *mut UvcDeviceHandle) -> UvcError;

    // 流控制
    pub fn uvc_get_stream_ctrl_format_size(
        devh: *mut UvcDeviceHandle,
        ctrl: *mut UvcStreamCtrl,
        format: UvcFrameFormat,
        width: c_int,
        height: c_int,
        fps: c_int,
    ) -> UvcError;

    pub fn uvc_probe_stream_ctrl(devh: *mut UvcDeviceHandle, ctrl: *mut UvcStreamCtrl) -> c_int;
    pub fn uvc_start_streaming(
        devh: *mut UvcDeviceHandle,
        ctrl: *mut UvcStreamCtrl,
        cb: UvcFrameCallback,
        user_ptr: *mut c_void,
        flags: u8,
    ) -> UvcError;

    pub fn uvc_stop_streaming(devh: *mut UvcDeviceHandle);

    // 格式枚举
    pub fn uvc_get_format_descs(devh: *mut UvcDeviceHandle) -> *const UvcFormatDesc;

    // 帧管理
    pub fn uvc_allocate_frame(data_bytes: size_t) -> *mut UvcFrame;
    pub fn uvc_free_frame(frame: *mut UvcFrame);

    // 注意：MJPEG 解码现在使用 Rust 的 turbojpeg crate
    // 不再需要 uvc_mjpeg2rgb, uvc_yuyv2rgb 等 C 函数

    // ==================== 摄像头控制参数 ====================

    // 自动曝光模式
    pub fn uvc_get_ae_mode(devh: *mut UvcDeviceHandle, mode: *mut u8, req_code: u8) -> UvcError;

    pub fn uvc_set_ae_mode(devh: *mut UvcDeviceHandle, mode: u8) -> UvcError;

    // 曝光时间（绝对值）
    pub fn uvc_get_exposure_abs(
        devh: *mut UvcDeviceHandle,
        time: *mut u32,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_exposure_abs(devh: *mut UvcDeviceHandle, time: u32) -> UvcError;

    // 焦距（绝对值）
    pub fn uvc_get_focus_abs(devh: *mut UvcDeviceHandle, focus: *mut u16, req_code: u8)
        -> UvcError;

    pub fn uvc_set_focus_abs(devh: *mut UvcDeviceHandle, focus: u16) -> UvcError;

    // 自动对焦
    pub fn uvc_get_focus_auto(devh: *mut UvcDeviceHandle, state: *mut u8, req_code: u8)
        -> UvcError;

    pub fn uvc_set_focus_auto(devh: *mut UvcDeviceHandle, state: u8) -> UvcError;

    // 变焦（绝对值）
    pub fn uvc_get_zoom_abs(
        devh: *mut UvcDeviceHandle,
        focal_length: *mut u16,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_zoom_abs(devh: *mut UvcDeviceHandle, focal_length: u16) -> UvcError;

    // 亮度
    pub fn uvc_get_brightness(
        devh: *mut UvcDeviceHandle,
        brightness: *mut i16,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_brightness(devh: *mut UvcDeviceHandle, brightness: i16) -> UvcError;

    // 对比度
    pub fn uvc_get_contrast(
        devh: *mut UvcDeviceHandle,
        contrast: *mut u16,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_contrast(devh: *mut UvcDeviceHandle, contrast: u16) -> UvcError;

    // 饱和度
    pub fn uvc_get_saturation(
        devh: *mut UvcDeviceHandle,
        saturation: *mut u16,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_saturation(devh: *mut UvcDeviceHandle, saturation: u16) -> UvcError;

    // 色调
    pub fn uvc_get_hue(devh: *mut UvcDeviceHandle, hue: *mut i16, req_code: u8) -> UvcError;

    pub fn uvc_set_hue(devh: *mut UvcDeviceHandle, hue: i16) -> UvcError;

    // 锐度
    pub fn uvc_get_sharpness(
        devh: *mut UvcDeviceHandle,
        sharpness: *mut u16,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_sharpness(devh: *mut UvcDeviceHandle, sharpness: u16) -> UvcError;

    // 伽马值
    pub fn uvc_get_gamma(devh: *mut UvcDeviceHandle, gamma: *mut u16, req_code: u8) -> UvcError;

    pub fn uvc_set_gamma(devh: *mut UvcDeviceHandle, gamma: u16) -> UvcError;

    // 白平衡温度
    pub fn uvc_get_white_balance_temperature(
        devh: *mut UvcDeviceHandle,
        temperature: *mut u16,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_white_balance_temperature(
        devh: *mut UvcDeviceHandle,
        temperature: u16,
    ) -> UvcError;

    // 自动白平衡
    pub fn uvc_get_white_balance_temperature_auto(
        devh: *mut UvcDeviceHandle,
        state: *mut u8,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_white_balance_temperature_auto(
        devh: *mut UvcDeviceHandle,
        state: u8,
    ) -> UvcError;

    // 增益
    pub fn uvc_get_gain(devh: *mut UvcDeviceHandle, gain: *mut u16, req_code: u8) -> UvcError;

    pub fn uvc_set_gain(devh: *mut UvcDeviceHandle, gain: u16) -> UvcError;

    // 背光补偿
    pub fn uvc_get_backlight_compensation(
        devh: *mut UvcDeviceHandle,
        backlight_compensation: *mut u16,
        req_code: u8,
    ) -> UvcError;

    pub fn uvc_set_backlight_compensation(
        devh: *mut UvcDeviceHandle,
        backlight_compensation: u16,
    ) -> UvcError;

    // 其他工具函数
    pub fn uvc_perror(err: UvcError, msg: *const c_char);
    pub fn uvc_strerror(err: UvcError) -> *const c_char;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(UVC_SUCCESS, 0);
        assert_eq!(UVC_ERROR_IO, -1);
    }

    #[test]
    fn test_frame_format() {
        assert_eq!(UvcFrameFormat::Mjpeg as i32, 7);
        assert_eq!(UvcFrameFormat::Yuyv as i32, 3);
    }
}

// Check the layout on every target, including cross-compiled Android ARMv7.
const _: () = {
    assert!(
        std::mem::size_of::<UvcFrameDesc>()
            == if cfg!(target_pointer_width = "64") {
                80
            } else {
                64
            }
    );
    assert!(
        std::mem::offset_of!(UvcFrameDesc, w_width)
            == if cfg!(target_pointer_width = "64") {
                30
            } else {
                18
            }
    );
    assert!(
        std::mem::offset_of!(UvcFrameDesc, dw_default_frame_interval)
            == if cfg!(target_pointer_width = "64") {
                48
            } else {
                36
            }
    );
};
#[cfg(test)]
mod abi_tests {
    use super::*;
    use std::mem::{offset_of, size_of};
    extern "C" {
        fn camera_uvc_abi_value(index: u32) -> usize;
    }
    #[test]
    fn rust_layout_matches_the_compiled_c_headers() {
        let rust = [
            size_of::<UvcFrameDesc>(),
            offset_of!(UvcFrameDesc, w_width),
            offset_of!(UvcFrameDesc, w_height),
            offset_of!(UvcFrameDesc, dw_default_frame_interval),
            offset_of!(UvcFrameDesc, intervals),
            size_of::<UvcFormatDesc>(),
            offset_of!(UvcFormatDesc, guid_format),
            offset_of!(UvcFormatDesc, frame_descs),
            size_of::<UvcFrame>(),
            offset_of!(UvcFrame, data),
            offset_of!(UvcFrame, data_bytes),
            offset_of!(UvcFrame, step),
            offset_of!(UvcFrame, sequence),
        ];
        for (index, value) in rust.into_iter().enumerate() {
            assert_eq!(
                value,
                unsafe { camera_uvc_abi_value(index as u32) },
                "ABI field {index}"
            );
        }
    }
}
