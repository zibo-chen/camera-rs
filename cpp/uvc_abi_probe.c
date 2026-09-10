#include <stddef.h>
#include <stdint.h>
#include "libuvc/libuvc.h"
// Compile-time 32/64-bit checks also run in Android cross builds.
_Static_assert(offsetof(uvc_frame_desc_t, wWidth) == (sizeof(void*) == 8 ? 30 : 18), "UVC width ABI");
_Static_assert(offsetof(uvc_frame_desc_t, dwDefaultFrameInterval) == (sizeof(void*) == 8 ? 48 : 36), "UVC interval ABI");
_Static_assert(sizeof(uvc_frame_desc_t) == (sizeof(void*) == 8 ? 80 : 64), "UVC descriptor ABI");
size_t camera_uvc_abi_value(unsigned index) {
    const size_t values[] = {
        sizeof(uvc_frame_desc_t), offsetof(uvc_frame_desc_t, wWidth),
        offsetof(uvc_frame_desc_t, wHeight), offsetof(uvc_frame_desc_t, dwDefaultFrameInterval),
        offsetof(uvc_frame_desc_t, intervals), sizeof(uvc_format_desc_t),
        offsetof(uvc_format_desc_t, guidFormat), offsetof(uvc_format_desc_t, frame_descs),
        sizeof(uvc_frame_t), offsetof(uvc_frame_t, data), offsetof(uvc_frame_t, data_bytes),
        offsetof(uvc_frame_t, step), offsetof(uvc_frame_t, sequence)
    };
    return index < sizeof(values) / sizeof(values[0]) ? values[index] : 0;
}
