#ifndef NDK_CAMERA2_BRIDGE_H
#define NDK_CAMERA2_BRIDGE_H

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

// ============================================================================
// Opaque handle type
// ============================================================================
typedef struct NdkCamera2 NdkCamera2;

// ============================================================================
// Camera device info
// ============================================================================
typedef struct {
    const char *id;
    int32_t facing;        // 0=BACK, 1=FRONT, 2=EXTERNAL
    int32_t orientation;   // sensor orientation in degrees
    bool available;
} NdkCameraDeviceInfo;

// ============================================================================
// Camera configuration
// ============================================================================
typedef struct {
    int32_t width;
    int32_t height;
    int32_t format;   // AIMAGE_FORMAT_YUV_420_888 = 0x23
    int32_t fps;
} NdkCameraConfig;

// ============================================================================
// Frame data passed to Rust callback
// ============================================================================
typedef struct {
    const uint8_t *rgb_data;  // RGBA/RGB plane pointer when available
    int32_t rgb_len;
    const uint8_t *y_data;
    int32_t y_len;
    const uint8_t *uv_data;   // interleaved UV for NV21
    int32_t uv_len;
    const uint8_t *v_data;
    int32_t v_len;
    int32_t row_stride_v;
    int32_t pixel_stride_v;
    int32_t width;
    int32_t height;
    int32_t row_stride_y;
    int32_t row_stride_uv;
    int32_t pixel_stride_uv;
    int32_t row_stride_rgb;
    int32_t pixel_format;     // see NdkCameraPixelFormat
    int64_t timestamp_ns;
} NdkFrameData;

typedef enum {
    NDK_CAMERA_PIXEL_FORMAT_UNKNOWN = 0,
    NDK_CAMERA_PIXEL_FORMAT_YUV420 = 1,
    NDK_CAMERA_PIXEL_FORMAT_RGBA8888 = 2,
} NdkCameraPixelFormat;



// ============================================================================
// Status codes
// ============================================================================
typedef enum {
    NDK_CAMERA_OK = 0,
    NDK_CAMERA_ERROR_INVALID_PARAM = -1,
    NDK_CAMERA_ERROR_OPEN_FAILED = -2,
    NDK_CAMERA_ERROR_SESSION_FAILED = -3,
    NDK_CAMERA_ERROR_NOT_FOUND = -4,
    NDK_CAMERA_ERROR_PERMISSION = -5,
    NDK_CAMERA_ERROR_ALREADY_STREAMING = -6,
    NDK_CAMERA_ERROR_NOT_STREAMING = -7,
    NDK_CAMERA_ERROR_INTERNAL = -8,
} NdkCameraStatus;

// ============================================================================
// Frame callback type - called from image reader thread
// ============================================================================
typedef void (*ndk_camera_frame_callback)(void *context, const NdkFrameData *frame);

// ============================================================================
// Lifecycle
// ============================================================================

/// Create a new NDK Camera2 instance. Returns NULL on failure.
NdkCamera2* ndk_camera2_create(void);

/// Destroy and release all resources.
void ndk_camera2_destroy(NdkCamera2 *cam);

/// Configure the AImageReader queue and NDK request template before streaming.
NdkCameraStatus ndk_camera2_set_options(NdkCamera2 *cam, int32_t max_images,
                                         int32_t request_template);

// ============================================================================
// Device enumeration
// ============================================================================

/// Get the number of available cameras.
NdkCameraStatus ndk_camera2_get_device_count(NdkCamera2 *cam, int32_t *out_count);

/// Get device info for a given index. Caller must NOT free the strings.
NdkCameraStatus ndk_camera2_get_device_info(NdkCamera2 *cam, int32_t index,
                                             NdkCameraDeviceInfo *out_info);

// ============================================================================
// Configuration query
// ============================================================================

/// Get number of supported output configurations for YUV_420_888 format.
NdkCameraStatus ndk_camera2_get_config_count(NdkCamera2 *cam, int32_t device_index,
                                              int32_t *out_count);

/// Get a specific output configuration.
NdkCameraStatus ndk_camera2_get_config(NdkCamera2 *cam, int32_t device_index,
                                        int32_t config_index, NdkCameraConfig *out_config);

// ============================================================================
// Streaming
// ============================================================================

/// Open camera device and start streaming.
/// frame_cb is called on the image reader thread for each frame.
NdkCameraStatus ndk_camera2_start_stream(NdkCamera2 *cam, int32_t device_index,
                                          const NdkCameraConfig *config,
                                          ndk_camera_frame_callback frame_cb,
                                          void *callback_context);

/// Stop streaming and close camera device.
NdkCameraStatus ndk_camera2_get_active_config(NdkCamera2 *cam, NdkCameraConfig *out);

NdkCameraStatus ndk_camera2_stop_stream(NdkCamera2 *cam);

/// Check if currently streaming.
bool ndk_camera2_is_streaming(NdkCamera2 *cam);

// ============================================================================
// Camera controls
// ============================================================================

/// Available mode bitmask for AE (0), AF (1), or AWB (2), from device metadata.
NdkCameraStatus ndk_camera2_get_control_modes(NdkCamera2 *cam, int32_t kind, uint32_t *mask);

/// Set exposure compensation value.
NdkCameraStatus ndk_camera2_set_exposure_compensation(NdkCamera2 *cam, int32_t value);

/// Get exposure compensation range.
NdkCameraStatus ndk_camera2_get_exposure_compensation_range(NdkCamera2 *cam,
                                                             int32_t *out_min,
                                                             int32_t *out_max);

/// Set auto-exposure mode (0=OFF, 1=ON, 2=ON_AUTO_FLASH).
NdkCameraStatus ndk_camera2_set_ae_mode(NdkCamera2 *cam, int32_t mode);

/// Set focus mode (0=OFF, 1=AUTO, 3=CONTINUOUS_VIDEO, 4=CONTINUOUS_PICTURE).
NdkCameraStatus ndk_camera2_set_af_mode(NdkCamera2 *cam, int32_t mode);

/// Set auto white balance mode (0=OFF, 1=AUTO).
NdkCameraStatus ndk_camera2_set_awb_mode(NdkCamera2 *cam, int32_t mode);

/// Set zoom using crop region (API 24+, value in 100ths, 100 = 1.0x).
NdkCameraStatus ndk_camera2_set_zoom(NdkCamera2 *cam, int32_t zoom_100);

/// Get zoom range (in 100ths).
NdkCameraStatus ndk_camera2_get_zoom_range(NdkCamera2 *cam, int32_t *out_min, int32_t *out_max);

/// Get sensor sensitivity (ISO) range.
NdkCameraStatus ndk_camera2_get_sensitivity_range(NdkCamera2 *cam,
                                                    int32_t *out_min, int32_t *out_max);

/// Set sensor sensitivity (ISO).
NdkCameraStatus ndk_camera2_set_sensitivity(NdkCamera2 *cam, int32_t iso);

#ifdef __cplusplus
}
#endif

#endif // NDK_CAMERA2_BRIDGE_H
