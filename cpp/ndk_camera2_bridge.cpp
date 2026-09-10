// NDK Camera2 Bridge Implementation
// Provides a C API for Rust FFI to interact with Android Camera2 NDK APIs.
// Designed for high-performance video streaming with minimal copies.

#include "ndk_camera2_bridge.h"
#include "camera2_metadata.h"

#include <camera/NdkCameraDevice.h>
#include <camera/NdkCameraError.h>
#include <camera/NdkCameraManager.h>
#include <camera/NdkCameraMetadataTags.h>
#include <camera/NdkCameraCaptureSession.h>
#include <media/NdkImageReader.h>
#include <android/log.h>

#include <string>
#include <vector>
#include <atomic>
#include <mutex>

#define LOG_TAG "ndk_camera2"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO,  LOG_TAG, __VA_ARGS__)
#define LOGW(...) __android_log_print(ANDROID_LOG_WARN,  LOG_TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

// Maximum number of images in AImageReader queue
static constexpr int MAX_IMAGE_BUFFER_COUNT = 4;

// ============================================================================
// Internal camera device info
// ============================================================================
struct CameraDeviceEntry {
    std::string id;
    int32_t facing;
    int32_t orientation;
    bool available;
};

// ============================================================================
// Internal output config
// ============================================================================
struct OutputConfig {
    int32_t width;
    int32_t height;
    int32_t format;
    int32_t fps;
};

// ============================================================================
// NdkCamera2 implementation
// ============================================================================
struct NdkCamera2 {
    ACameraManager *camera_mgr = nullptr;

    // Enumerated devices
    std::vector<CameraDeviceEntry> devices;

    // Active streaming state
    std::atomic<bool> streaming{false};
    ACameraDevice *camera_device = nullptr;
    AImageReader *image_reader = nullptr;
    ANativeWindow *image_window = nullptr;
    ACaptureSessionOutputContainer *output_container = nullptr;
    ACaptureSessionOutput *session_output = nullptr;
    ACameraOutputTarget *output_target = nullptr;
    ACaptureRequest *capture_request = nullptr;
    ACameraCaptureSession *capture_session = nullptr;

    // Frame callback
    std::mutex callback_mutex;
    ndk_camera_frame_callback frame_cb = nullptr;
    void *cb_context = nullptr;

    // Current config
    NdkCameraConfig current_config = {};
    int32_t active_device_index = -1;
    int32_t output_image_format = AIMAGE_FORMAT_YUV_420_888;

    // Cached configs per device
    std::vector<OutputConfig> cached_configs;
    int32_t cached_config_device = -1;

    // Mutex for session state changes
    std::mutex session_mutex;

    // Callback structs are stored as members to ensure they outlive the session
    ACameraDevice_StateCallbacks device_cbs = {};
    ACameraCaptureSession_stateCallbacks session_cbs = {};

    NdkCamera2() {
        camera_mgr = ACameraManager_create();
        if (camera_mgr) {
            enumerate_cameras();
        }
    }

    ~NdkCamera2() {
        stop_streaming();
        if (camera_mgr) {
            ACameraManager_delete(camera_mgr);
            camera_mgr = nullptr;
        }
    }

    void enumerate_cameras() {
        devices.clear();
        ACameraIdList *id_list = nullptr;
        if (ACameraManager_getCameraIdList(camera_mgr, &id_list) != ACAMERA_OK || !id_list) {
            LOGE("Failed to get camera id list");
            return;
        }

        for (int i = 0; i < id_list->numCameras; i++) {
            const char *id = id_list->cameraIds[i];
            ACameraMetadata *metadata = nullptr;
            if (ACameraManager_getCameraCharacteristics(camera_mgr, id, &metadata) != ACAMERA_OK) {
                continue;
            }

            CameraDeviceEntry entry;
            entry.id = id;
            entry.facing = 1; // default BACK (ACAMERA_LENS_FACING_BACK = 1)
            entry.orientation = 0;
            entry.available = true;

            ACameraMetadata_const_entry lens_entry;
            if (ACameraMetadata_getConstEntry(metadata, ACAMERA_LENS_FACING, &lens_entry) == ACAMERA_OK) {
                entry.facing = lens_entry.data.u8[0];
            }

            ACameraMetadata_const_entry orient_entry;
            if (ACameraMetadata_getConstEntry(metadata, ACAMERA_SENSOR_ORIENTATION, &orient_entry) == ACAMERA_OK) {
                entry.orientation = orient_entry.data.i32[0];
            }

            ACameraMetadata_free(metadata);
            devices.push_back(entry);
        }

        ACameraManager_deleteCameraIdList(id_list);
        LOGI("Enumerated %zu cameras", devices.size());
    }

    void query_output_configs(int32_t device_index) {
        if (cached_config_device == device_index && !cached_configs.empty()) {
            return; // already cached
        }
        cached_configs.clear();
        cached_config_device = device_index;

        if (device_index < 0 || device_index >= (int32_t)devices.size()) {
            return;
        }

        ACameraMetadata *metadata = nullptr;
        if (ACameraManager_getCameraCharacteristics(
                camera_mgr, devices[device_index].id.c_str(), &metadata) != ACAMERA_OK) {
            return;
        }

        ACameraMetadata_const_entry entry;
        if (ACameraMetadata_getConstEntry(metadata,
                ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS, &entry) == ACAMERA_OK) {
            // Data format: format, width, height, input/output (0=output)
            for (uint32_t i = 0; i + 3 < entry.count; i += 4) {
                int32_t format = entry.data.i32[i + 0];
                int32_t width  = entry.data.i32[i + 1];
                int32_t height = entry.data.i32[i + 2];
                int32_t input  = entry.data.i32[i + 3];

                // Only output configurations, YUV_420_888 format
                if (input == 0 && format == AIMAGE_FORMAT_YUV_420_888) {
                    OutputConfig cfg;
                    cfg.width = width;
                    cfg.height = height;
                    cfg.format = format;
                    ACameraMetadata_const_entry rates;
                    if (ACameraMetadata_getConstEntry(metadata, ACAMERA_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES, &rates) == ACAMERA_OK) {
                        for (uint32_t r = 0; r + 1 < rates.count; r += 2) {
                            cfg.fps = rates.data.i32[r + 1];
                            if (cfg.fps > 0) cached_configs.push_back(cfg);
                        }
                    }
                }
            }
        }

        ACameraMetadata_free(metadata);
        LOGI("Device %d has %zu YUV output configs", device_index, cached_configs.size());
    }

    // ========================================================================
    // Streaming
    // ========================================================================

    NdkCameraStatus start_streaming(int32_t device_index, const NdkCameraConfig *config,
                                     ndk_camera_frame_callback cb, void *ctx) {
        if (device_index < 0 || device_index >= (int32_t)devices.size()) {
            return NDK_CAMERA_ERROR_NOT_FOUND;
        }
        if (!config || !cb) {
            return NDK_CAMERA_ERROR_INVALID_PARAM;
        }

        std::lock_guard<std::mutex> lock(session_mutex);

        if (streaming.load()) {
            return NDK_CAMERA_ERROR_ALREADY_STREAMING;
        }

        cleanup_streaming();

        {
            std::lock_guard<std::mutex> callback_lock(callback_mutex);
            frame_cb = cb;
            cb_context = ctx;
        }
        current_config = *config;
        active_device_index = device_index;

        // 1. Create AImageReader.
        //
        // External Camera2 devices commonly expose only YUV_420_888 output.
        // Requesting RGBA_8888 can create the reader but fail later when the
        // capture session is configured, so use YUV and let Rust convert it.
        int32_t requested_format = AIMAGE_FORMAT_YUV_420_888;

        media_status_t mstatus = AImageReader_new(
            config->width, config->height,
            requested_format,
            MAX_IMAGE_BUFFER_COUNT,
            &image_reader);

        if ((mstatus != AMEDIA_OK || !image_reader)
            && requested_format != AIMAGE_FORMAT_YUV_420_888) {
            LOGW("RGBA reader unavailable, fallback to YUV_420_888");
            image_reader = nullptr;
            requested_format = AIMAGE_FORMAT_YUV_420_888;
            mstatus = AImageReader_new(
                config->width,
                config->height,
                requested_format,
                MAX_IMAGE_BUFFER_COUNT,
                &image_reader);
        }

        if (mstatus != AMEDIA_OK || !image_reader) {
            LOGE("Failed to create AImageReader: %d", mstatus);
            cleanup_streaming();
            return NDK_CAMERA_ERROR_INTERNAL;
        }
        output_image_format = requested_format;

        // Set image available callback
        AImageReader_ImageListener listener = {
            .context = this,
            .onImageAvailable = on_image_available,
        };
        AImageReader_setImageListener(image_reader, &listener);

        // Get native window from image reader
        AImageReader_getWindow(image_reader, &image_window);
        if (!image_window) {
            LOGE("Failed to get AImageReader window");
            cleanup_streaming();
            return NDK_CAMERA_ERROR_INTERNAL;
        }
        ANativeWindow_acquire(image_window);

        // 2. Open camera device
        // Callback structs are member variables (see above) so they outlive the session.
        // This also ensures 'this' is always the correct live instance.
        device_cbs.context = this;
        device_cbs.onDisconnected = on_device_disconnected;
        device_cbs.onError = on_device_error;

        camera_status_t status = ACameraManager_openCamera(
            camera_mgr, devices[device_index].id.c_str(),
            &device_cbs, &camera_device);
        if (status != ACAMERA_OK || !camera_device) {
            LOGE("Failed to open camera %s: %d", devices[device_index].id.c_str(), status);
            cleanup_streaming();
            return NDK_CAMERA_ERROR_OPEN_FAILED;
        }

        // 3. Create capture session output container
        ACaptureSessionOutputContainer_create(&output_container);

        // Create session output from image reader window
        ACaptureSessionOutput_create(image_window, &session_output);
        ACaptureSessionOutputContainer_add(output_container, session_output);

        // Create output target
        ACameraOutputTarget_create(image_window, &output_target);

        // Create capture request with TEMPLATE_PREVIEW for best frame rate
        ACameraDevice_createCaptureRequest(camera_device, TEMPLATE_PREVIEW, &capture_request);
        ACaptureRequest_addTarget(capture_request, output_target);

        // A target range must be one of the advertised pairs (many USB HALs
        // advertise [15,30] only). Never manufacture [requested,requested].
        ACameraMetadata *metadata = nullptr;
        ACameraMetadata_const_entry rates;
        int32_t fps_range[2];
        bool selected = ACameraManager_getCameraCharacteristics(camera_mgr,
            devices[device_index].id.c_str(), &metadata) == ACAMERA_OK &&
            ACameraMetadata_getConstEntry(metadata, ACAMERA_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES, &rates) == ACAMERA_OK &&
            camera2_select_fps(rates.data.i32, rates.count, config->fps, fps_range);
        if (metadata) ACameraMetadata_free(metadata);
        if (!selected || ACaptureRequest_setEntry_i32(capture_request,
                ACAMERA_CONTROL_AE_TARGET_FPS_RANGE, 2, fps_range) != ACAMERA_OK) {
            cleanup_streaming();
            return NDK_CAMERA_ERROR_INVALID_PARAM;
        }
        current_config.fps = camera2_clamp_fps(config->fps, fps_range);

        // 4. Create capture session
        // Callback struct is a member variable to ensure correct lifetime and 'this'.
        session_cbs.context = this;
        session_cbs.onClosed = on_session_closed;
        session_cbs.onReady = on_session_ready;
        session_cbs.onActive = on_session_active;

        status = ACameraDevice_createCaptureSession(
            camera_device, output_container, &session_cbs, &capture_session);
        if (status != ACAMERA_OK || !capture_session) {
            LOGE("Failed to create capture session: %d", status);
            cleanup_streaming();
            return NDK_CAMERA_ERROR_SESSION_FAILED;
        }

        // 5. Start repeating request
        status = ACameraCaptureSession_setRepeatingRequest(
            capture_session, nullptr, 1, &capture_request, nullptr);
        if (status != ACAMERA_OK) {
            LOGE("Failed to start repeating request: %d", status);
            cleanup_streaming();
            return NDK_CAMERA_ERROR_SESSION_FAILED;
        }

        streaming.store(true);
        LOGI("Camera %s streaming at %dx%d @ %dfps",
             devices[device_index].id.c_str(),
             config->width, config->height, config->fps);
        return NDK_CAMERA_OK;
    }

    void stop_streaming() {
        bool was_streaming = streaming.exchange(false);
        LOGI("Stopping camera stream");

        std::lock_guard<std::mutex> lock(session_mutex);

        bool has_resources = capture_session || capture_request || output_target ||
            session_output || output_container || camera_device || image_window || image_reader;
        if (!was_streaming && !has_resources) {
            return;
        }

        if (capture_session) {
            ACameraCaptureSession_stopRepeating(capture_session);
            ACameraCaptureSession_close(capture_session);
            capture_session = nullptr;
        }

        cleanup_streaming();
        LOGI("Camera stream stopped");
    }

    void cleanup_streaming() {
        streaming.store(false);
        // Drain an active Rust callback before releasing native images. Never
        // hold this mutex while deleting the reader (which joins its thread).
        {
            std::lock_guard<std::mutex> callback_lock(callback_mutex);
            frame_cb = nullptr;
            cb_context = nullptr;
        }
        if (capture_session) {
            ACameraCaptureSession_stopRepeating(capture_session);
            ACameraCaptureSession_close(capture_session);
            capture_session = nullptr;
        }

        if (capture_request) {
            if (output_target) {
                ACaptureRequest_removeTarget(capture_request, output_target);
            }
            ACaptureRequest_free(capture_request);
            capture_request = nullptr;
        }

        if (output_target) {
            ACameraOutputTarget_free(output_target);
            output_target = nullptr;
        }

        if (session_output && output_container) {
            ACaptureSessionOutputContainer_remove(output_container, session_output);
        }
        if (session_output) {
            ACaptureSessionOutput_free(session_output);
            session_output = nullptr;
        }
        if (output_container) {
            ACaptureSessionOutputContainer_free(output_container);
            output_container = nullptr;
        }

        if (camera_device) {
            ACameraDevice_close(camera_device);
            camera_device = nullptr;
        }

        if (image_window) {
            ANativeWindow_release(image_window);
            image_window = nullptr;
        }

        if (image_reader) {
            AImageReader_delete(image_reader);
            image_reader = nullptr;
        }

        active_device_index = -1;
    }

    // ========================================================================
    // Camera controls - applied by updating capture request
    // ========================================================================

    NdkCameraStatus update_repeating_request() {
        if (!streaming.load() || !capture_session || !capture_request) {
            return NDK_CAMERA_ERROR_NOT_STREAMING;
        }
        // Stop and re-start repeating request with updated parameters
        ACameraCaptureSession_stopRepeating(capture_session);
        camera_status_t status = ACameraCaptureSession_setRepeatingRequest(
            capture_session, nullptr, 1, &capture_request, nullptr);
        if (status != ACAMERA_OK) {
            LOGE("Failed to update repeating request: %d", status);
            return NDK_CAMERA_ERROR_INTERNAL;
        }
        return NDK_CAMERA_OK;
    }

    NdkCameraStatus get_metadata_range_i32(int32_t device_index, uint32_t tag,
                                            int32_t *out_min, int32_t *out_max) {
        if (device_index < 0 || device_index >= (int32_t)devices.size()) {
            return NDK_CAMERA_ERROR_NOT_FOUND;
        }
        ACameraMetadata *metadata = nullptr;
        if (ACameraManager_getCameraCharacteristics(
                camera_mgr, devices[device_index].id.c_str(), &metadata) != ACAMERA_OK) {
            return NDK_CAMERA_ERROR_INTERNAL;
        }
        ACameraMetadata_const_entry entry;
        NdkCameraStatus ret = NDK_CAMERA_ERROR_INTERNAL;
        if (ACameraMetadata_getConstEntry(metadata, tag, &entry) == ACAMERA_OK && entry.count >= 2) {
            *out_min = entry.data.i32[0];
            *out_max = entry.data.i32[1];
            ret = NDK_CAMERA_OK;
        }
        ACameraMetadata_free(metadata);
        return ret;
    }

    // ========================================================================
    // Static callbacks
    // ========================================================================

    static void on_image_available(void *context, AImageReader *reader) {
        auto *self = reinterpret_cast<NdkCamera2*>(context);
        std::lock_guard<std::mutex> callback_lock(self->callback_mutex);
        if (!self->streaming.load() || !self->frame_cb) {
            // Drain the image
            AImage *image = nullptr;
            if (AImageReader_acquireLatestImage(reader, &image) == AMEDIA_OK && image) {
                AImage_delete(image);
            }
            return;
        }

        AImage *image = nullptr;
        // Use acquireLatestImage to skip stale frames for lower latency
        media_status_t status = AImageReader_acquireLatestImage(reader, &image);
        if (status != AMEDIA_OK || !image) {
            return;
        }

        int32_t width = 0, height = 0;
        AImage_getWidth(image, &width);
        AImage_getHeight(image, &height);

        uint8_t *rgb_data = nullptr;
        int32_t rgb_len = 0;
        int32_t rgb_row_stride = 0;

        uint8_t *y_data = nullptr;
        int32_t y_len = 0;
        uint8_t *uv_data = nullptr;
        int32_t uv_len = 0;
        uint8_t *v_data = nullptr;
        int32_t v_len = 0, v_row_stride = 0, v_pixel_stride = 0;
        int32_t y_row_stride = 0, uv_row_stride = 0, uv_pixel_stride = 0;
        int32_t pixel_format = NDK_CAMERA_PIXEL_FORMAT_UNKNOWN;

        if (self->output_image_format == AIMAGE_FORMAT_RGBA_8888) {
            AImage_getPlaneData(image, 0, &rgb_data, &rgb_len);
            AImage_getPlaneRowStride(image, 0, &rgb_row_stride);
            if (rgb_data && rgb_len > 0) {
                pixel_format = NDK_CAMERA_PIXEL_FORMAT_RGBA8888;
            }
        }

        if (pixel_format == NDK_CAMERA_PIXEL_FORMAT_UNKNOWN) {
            AImage_getPlaneData(image, 0, &y_data, &y_len);
            AImage_getPlaneData(image, 1, &uv_data, &uv_len);
            AImage_getPlaneData(image, 2, &v_data, &v_len);
            AImage_getPlaneRowStride(image, 2, &v_row_stride);
            AImage_getPlanePixelStride(image, 2, &v_pixel_stride);
            AImage_getPlaneRowStride(image, 0, &y_row_stride);
            AImage_getPlaneRowStride(image, 1, &uv_row_stride);
            AImage_getPlanePixelStride(image, 1, &uv_pixel_stride);
            pixel_format = NDK_CAMERA_PIXEL_FORMAT_YUV420;
        }

        int64_t timestamp = 0;
        AImage_getTimestamp(image, &timestamp);

        NdkFrameData frame = {};
        frame.rgb_data = rgb_data;
        frame.rgb_len = rgb_len;
        frame.y_data = y_data;
        frame.y_len = y_len;
        frame.uv_data = uv_data;
        frame.uv_len = uv_len;
        frame.v_data = v_data;
        frame.v_len = v_len;
        frame.row_stride_v = v_row_stride;
        frame.pixel_stride_v = v_pixel_stride;
        frame.width = width;
        frame.height = height;
        frame.row_stride_y = y_row_stride;
        frame.row_stride_uv = uv_row_stride;
        frame.pixel_stride_uv = uv_pixel_stride;
        frame.row_stride_rgb = rgb_row_stride;
        frame.pixel_format = pixel_format;
        frame.timestamp_ns = timestamp;

        self->frame_cb(self->cb_context, &frame);

        AImage_delete(image);
    }

    static void on_device_disconnected(void *context, ACameraDevice *device) {
        auto *self = reinterpret_cast<NdkCamera2*>(context);
        LOGW("Camera device disconnected");
        std::lock_guard<std::mutex> callback_lock(self->callback_mutex);
        if (self->streaming.exchange(false) && self->frame_cb) {
            self->frame_cb(self->cb_context, nullptr);
        }
    }

    static void on_device_error(void *context, ACameraDevice *device, int error) {
        auto *self = reinterpret_cast<NdkCamera2*>(context);
        LOGE("Camera device error: %d", error);
        std::lock_guard<std::mutex> callback_lock(self->callback_mutex);
        if (self->streaming.exchange(false) && self->frame_cb) {
            self->frame_cb(self->cb_context, nullptr);
        }
    }

    static void on_session_closed(void *context, ACameraCaptureSession *session) {
        LOGI("Capture session closed");
    }

    static void on_session_ready(void *context, ACameraCaptureSession *session) {
        LOGI("Capture session ready");
    }

    static void on_session_active(void *context, ACameraCaptureSession *session) {
        LOGI("Capture session active");
    }
};

// ============================================================================
// C API implementation
// ============================================================================

extern "C" {

NdkCamera2* ndk_camera2_create(void) {
    auto *cam = new (std::nothrow) NdkCamera2();
    if (!cam || !cam->camera_mgr) {
        delete cam;
        return nullptr;
    }
    return cam;
}

void ndk_camera2_destroy(NdkCamera2 *cam) {
    delete cam;
}

NdkCameraStatus ndk_camera2_get_device_count(NdkCamera2 *cam, int32_t *out_count) {
    if (!cam || !out_count) return NDK_CAMERA_ERROR_INVALID_PARAM;
    *out_count = (int32_t)cam->devices.size();
    return NDK_CAMERA_OK;
}

NdkCameraStatus ndk_camera2_get_device_info(NdkCamera2 *cam, int32_t index,
                                             NdkCameraDeviceInfo *out_info) {
    if (!cam || !out_info) return NDK_CAMERA_ERROR_INVALID_PARAM;
    if (index < 0 || index >= (int32_t)cam->devices.size()) return NDK_CAMERA_ERROR_NOT_FOUND;

    const auto &dev = cam->devices[index];
    out_info->id = dev.id.c_str();
    out_info->facing = dev.facing;
    out_info->orientation = dev.orientation;
    out_info->available = dev.available;
    return NDK_CAMERA_OK;
}

NdkCameraStatus ndk_camera2_get_config_count(NdkCamera2 *cam, int32_t device_index,
                                              int32_t *out_count) {
    if (!cam || !out_count) return NDK_CAMERA_ERROR_INVALID_PARAM;
    cam->query_output_configs(device_index);
    *out_count = (int32_t)cam->cached_configs.size();
    return NDK_CAMERA_OK;
}

NdkCameraStatus ndk_camera2_get_config(NdkCamera2 *cam, int32_t device_index,
                                        int32_t config_index, NdkCameraConfig *out_config) {
    if (!cam || !out_config) return NDK_CAMERA_ERROR_INVALID_PARAM;
    cam->query_output_configs(device_index);
    if (config_index < 0 || config_index >= (int32_t)cam->cached_configs.size()) {
        return NDK_CAMERA_ERROR_NOT_FOUND;
    }
    const auto &cfg = cam->cached_configs[config_index];
    out_config->width = cfg.width;
    out_config->height = cfg.height;
    out_config->format = cfg.format;
    out_config->fps = cfg.fps; // Advertised AE upper bound; exposure can lower delivery rate.
    return NDK_CAMERA_OK;
}

NdkCameraStatus ndk_camera2_start_stream(NdkCamera2 *cam, int32_t device_index,
                                          const NdkCameraConfig *config,
                                          ndk_camera_frame_callback frame_cb,
                                          void *callback_context) {
    if (!cam) return NDK_CAMERA_ERROR_INVALID_PARAM;
    return cam->start_streaming(device_index, config, frame_cb, callback_context);
}

NdkCameraStatus ndk_camera2_get_active_config(NdkCamera2 *cam, NdkCameraConfig *out) {
    if (!cam || !out) return NDK_CAMERA_ERROR_INVALID_PARAM;
    if (!cam->streaming.load()) return NDK_CAMERA_ERROR_NOT_STREAMING;
    *out = cam->current_config;
    return NDK_CAMERA_OK;
}

NdkCameraStatus ndk_camera2_stop_stream(NdkCamera2 *cam) {
    if (!cam) return NDK_CAMERA_ERROR_INVALID_PARAM;
    cam->stop_streaming();
    return NDK_CAMERA_OK;
}

bool ndk_camera2_is_streaming(NdkCamera2 *cam) {
    if (!cam) return false;
    return cam->streaming.load();
}



NdkCameraStatus ndk_camera2_get_control_modes(NdkCamera2 *cam, int32_t kind, uint32_t *mask) {
    if (!cam || !mask || kind < 0 || kind > 2) return NDK_CAMERA_ERROR_INVALID_PARAM;
    *mask = 0;
    int32_t idx = cam->active_device_index >= 0 ? cam->active_device_index : 0;
    if (idx >= (int32_t)cam->devices.size()) return NDK_CAMERA_ERROR_NOT_FOUND;
    ACameraMetadata *metadata = nullptr;
    if (ACameraManager_getCameraCharacteristics(cam->camera_mgr,cam->devices[idx].id.c_str(),&metadata) != ACAMERA_OK)
        return NDK_CAMERA_ERROR_INTERNAL;
    uint32_t tags[] = {ACAMERA_CONTROL_AE_AVAILABLE_MODES, ACAMERA_CONTROL_AF_AVAILABLE_MODES, ACAMERA_CONTROL_AWB_AVAILABLE_MODES};
    ACameraMetadata_const_entry entry;
    if (ACameraMetadata_getConstEntry(metadata,tags[kind],&entry) == ACAMERA_OK)
        for (uint32_t i = 0; i < entry.count; i++)
            if (entry.data.u8[i] < 32) *mask |= 1u << entry.data.u8[i];
    ACameraMetadata_free(metadata);
    return NDK_CAMERA_OK;
}

static NdkCameraStatus set_mode(NdkCamera2 *cam, int32_t kind, int32_t mode) {
    if (!cam || !cam->capture_request) return NDK_CAMERA_ERROR_NOT_STREAMING;
    uint32_t mask = 0;
    NdkCameraStatus result = ndk_camera2_get_control_modes(cam,kind,&mask);
    if (result != NDK_CAMERA_OK) return result;
    if (mode < 0 || mode >= 32 || !(mask & (1u << mode))) return NDK_CAMERA_ERROR_INVALID_PARAM;
    const uint32_t tags[] = {ACAMERA_CONTROL_AE_MODE, ACAMERA_CONTROL_AF_MODE, ACAMERA_CONTROL_AWB_MODE};
    uint8_t value = (uint8_t)mode;
    if (ACaptureRequest_setEntry_u8(cam->capture_request,tags[kind],1,&value) != ACAMERA_OK)
        return NDK_CAMERA_ERROR_INVALID_PARAM;
    if (kind == 1 && mode == ACAMERA_CONTROL_AF_MODE_AUTO) {
        uint8_t trigger = ACAMERA_CONTROL_AF_TRIGGER_START;
        if (ACaptureRequest_setEntry_u8(cam->capture_request,ACAMERA_CONTROL_AF_TRIGGER,1,&trigger) != ACAMERA_OK)
            return NDK_CAMERA_ERROR_INVALID_PARAM;
        camera_status_t status = ACameraCaptureSession_capture(cam->capture_session,nullptr,1,&cam->capture_request,nullptr);
        trigger = ACAMERA_CONTROL_AF_TRIGGER_IDLE;
        ACaptureRequest_setEntry_u8(cam->capture_request,ACAMERA_CONTROL_AF_TRIGGER,1,&trigger);
        if (status != ACAMERA_OK) return NDK_CAMERA_ERROR_SESSION_FAILED;
    }
    return cam->update_repeating_request();
}

NdkCameraStatus ndk_camera2_set_exposure_compensation(NdkCamera2 *cam, int32_t value) {
    if (!cam || !cam->capture_request) return NDK_CAMERA_ERROR_NOT_STREAMING;
    if (ACaptureRequest_setEntry_i32(cam->capture_request,
            ACAMERA_CONTROL_AE_EXPOSURE_COMPENSATION, 1, &value) != ACAMERA_OK)
        return NDK_CAMERA_ERROR_INVALID_PARAM;
    return cam->update_repeating_request();
}

NdkCameraStatus ndk_camera2_get_exposure_compensation_range(NdkCamera2 *cam,
                                                             int32_t *out_min,
                                                             int32_t *out_max) {
    if (!cam || !out_min || !out_max) return NDK_CAMERA_ERROR_INVALID_PARAM;
    int32_t idx = cam->active_device_index >= 0 ? cam->active_device_index : 0;
    return cam->get_metadata_range_i32(idx,
        ACAMERA_CONTROL_AE_COMPENSATION_RANGE, out_min, out_max);
}

NdkCameraStatus ndk_camera2_set_ae_mode(NdkCamera2 *cam, int32_t mode) {
    return set_mode(cam,0,mode);
}

NdkCameraStatus ndk_camera2_set_af_mode(NdkCamera2 *cam, int32_t mode) {
    return set_mode(cam,1,mode);
}

NdkCameraStatus ndk_camera2_set_awb_mode(NdkCamera2 *cam, int32_t mode) {
    return set_mode(cam,2,mode);
}

NdkCameraStatus ndk_camera2_set_zoom(NdkCamera2 *cam, int32_t zoom_100) {
    if (!cam || !cam->capture_request) return NDK_CAMERA_ERROR_NOT_STREAMING;

    // Use SCALER_CROP_REGION for zoom (compatible with API 24+)
    int32_t idx = cam->active_device_index;
    if (idx < 0 || idx >= (int32_t)cam->devices.size()) return NDK_CAMERA_ERROR_INTERNAL;

    ACameraMetadata *metadata = nullptr;
    if (ACameraManager_getCameraCharacteristics(
            cam->camera_mgr, cam->devices[idx].id.c_str(), &metadata) != ACAMERA_OK) {
        return NDK_CAMERA_ERROR_INTERNAL;
    }

    ACameraMetadata_const_entry sensor_entry;
    if (ACameraMetadata_getConstEntry(metadata,
            ACAMERA_SENSOR_INFO_ACTIVE_ARRAY_SIZE, &sensor_entry) != ACAMERA_OK ||
        sensor_entry.count < 4) {
        ACameraMetadata_free(metadata);
        return NDK_CAMERA_ERROR_INTERNAL;
    }

    int32_t crop_region[4];
    bool valid = camera2_zoom_crop(sensor_entry.data.i32, zoom_100, crop_region);
    ACameraMetadata_free(metadata);
    if (!valid || ACaptureRequest_setEntry_i32(cam->capture_request,
            ACAMERA_SCALER_CROP_REGION, 4, crop_region) != ACAMERA_OK)
        return NDK_CAMERA_ERROR_INVALID_PARAM;

    return cam->update_repeating_request();
}

NdkCameraStatus ndk_camera2_get_zoom_range(NdkCamera2 *cam, int32_t *out_min, int32_t *out_max) {
    if (!cam || !out_min || !out_max) return NDK_CAMERA_ERROR_INVALID_PARAM;
    int32_t idx = cam->active_device_index >= 0 ? cam->active_device_index : 0;
    if (idx >= (int32_t)cam->devices.size()) return NDK_CAMERA_ERROR_NOT_FOUND;

    ACameraMetadata *metadata = nullptr;
    if (ACameraManager_getCameraCharacteristics(
            cam->camera_mgr, cam->devices[idx].id.c_str(), &metadata) != ACAMERA_OK) {
        return NDK_CAMERA_ERROR_INTERNAL;
    }

    ACameraMetadata_const_entry entry;
    *out_min = 100; // 1.0x
    if (ACameraMetadata_getConstEntry(metadata,
            ACAMERA_SCALER_AVAILABLE_MAX_DIGITAL_ZOOM, &entry) == ACAMERA_OK && entry.count >= 1) {
        *out_max = (int32_t)(entry.data.f[0] * 100.0f);
    } else {
        *out_max = 100; // no zoom
    }

    ACameraMetadata_free(metadata);
    return NDK_CAMERA_OK;
}

NdkCameraStatus ndk_camera2_get_sensitivity_range(NdkCamera2 *cam,
                                                    int32_t *out_min, int32_t *out_max) {
    if (!cam || !out_min || !out_max) return NDK_CAMERA_ERROR_INVALID_PARAM;
    int32_t idx = cam->active_device_index >= 0 ? cam->active_device_index : 0;
    return cam->get_metadata_range_i32(idx,
        ACAMERA_SENSOR_INFO_SENSITIVITY_RANGE, out_min, out_max);
}

NdkCameraStatus ndk_camera2_set_sensitivity(NdkCamera2 *cam, int32_t iso) {
    if (!cam || !cam->capture_request) return NDK_CAMERA_ERROR_NOT_STREAMING;
    // Must disable AE to set ISO manually
    uint8_t ae_off = ACAMERA_CONTROL_AE_MODE_OFF;
    uint32_t modes = 0;
    if (ndk_camera2_get_control_modes(cam, 0, &modes) != NDK_CAMERA_OK || !(modes & 1u))
        return NDK_CAMERA_ERROR_INVALID_PARAM;
    if (ACaptureRequest_setEntry_u8(cam->capture_request,
            ACAMERA_CONTROL_AE_MODE, 1, &ae_off) != ACAMERA_OK ||
        ACaptureRequest_setEntry_i32(cam->capture_request,
            ACAMERA_SENSOR_SENSITIVITY, 1, &iso) != ACAMERA_OK)
        return NDK_CAMERA_ERROR_INVALID_PARAM;
    return cam->update_repeating_request();
}

} // extern "C"
