#include <assert.h>
#include "../3rdparty/libuvc/src/stream.c"
static unsigned char saved[34];
static uint32_t maximum_payload = 256, negotiated_payload = 512, frame_size = 640*480*2;
static int fail_request, short_request, bad_frame, claim_failure;
int libusb_control_transfer(libusb_device_handle *h, uint8_t type, uint8_t req,
 uint16_t value, uint16_t index, unsigned char *data, uint16_t length, unsigned int timeout) {
 (void)h; (void)type; (void)value; (void)index; (void)timeout;
 if (req == fail_request) return LIBUSB_ERROR_TIMEOUT;
 if (req == short_request) return length - 1;
 if (req == UVC_SET_CUR) memcpy(saved,data,length);
 else if (req == UVC_GET_MAX) { memset(data,0,length); INT_TO_DW(maximum_payload,data+22); }
 else {
  memcpy(data,saved,length); INT_TO_DW(negotiated_payload,data+22); INT_TO_DW(frame_size,data+18);
  if (bad_frame) data[3]++;
 }
 return length;
}
uvc_error_t uvc_claim_if(uvc_device_handle_t *h, int i) { (void)h; (void)i; return claim_failure; }
int main(void) {
 uvc_device_handle_t h = {0}; uvc_device_info_t info = {0}; h.info = &info;
 uvc_streaming_interface_t stream = {0}; info.stream_ifs = &stream;
 uvc_format_desc_t format = {0}; stream.format_descs = &format; format.parent = &stream;
 memcpy(format.guidFormat,"MJPG",4); format.bFormatIndex = 1;
 uvc_frame_desc_t frame = {0}; format.frame_descs = &frame; frame.parent = &format;
 frame.bFrameIndex = 1; frame.wWidth = 640; frame.wHeight = 480;
 frame.dwDefaultFrameInterval = frame.dwMinFrameInterval = 333333;
 frame.dwMaxFrameInterval = 1000000; frame.dwFrameIntervalStep = 0;
 uvc_stream_ctrl_t ctrl = {0};
 // Any-rate selection must work for continuous intervals, including a zero step.
 assert(uvc_get_stream_ctrl_format_size(&h,&ctrl,UVC_FRAME_FORMAT_MJPEG,640,480,0) == 0);
 assert(ctrl.dwFrameInterval == 333333 && ctrl.dwMaxPayloadTransferSize == 512);
 fail_request = UVC_SET_CUR;
 assert(uvc_probe_stream_ctrl(&h,&ctrl) == UVC_ERROR_TIMEOUT);
 fail_request = UVC_GET_CUR;
 assert(uvc_probe_stream_ctrl(&h,&ctrl) == UVC_ERROR_TIMEOUT);
 fail_request = 0; short_request = UVC_GET_CUR;
 assert(uvc_probe_stream_ctrl(&h,&ctrl) == UVC_ERROR_IO);
 short_request = 0; bad_frame = 1;
 assert(uvc_probe_stream_ctrl(&h,&ctrl) == UVC_ERROR_INVALID_MODE);
 bad_frame = 0; claim_failure = UVC_ERROR_ACCESS;
 assert(uvc_get_stream_ctrl_format_size(&h,&ctrl,UVC_FRAME_FORMAT_MJPEG,640,480,30) == UVC_ERROR_ACCESS);

 // A smaller frame must not shrink the allocation and force the next frame to
 // reallocate. data_bytes and metadata_bytes still describe only valid bytes.
 unsigned char image[200], metadata[20];
 memset(image, 0x5a, sizeof(image));
 memset(metadata, 0xa5, sizeof(metadata));
 uvc_stream_handle_t handle = {0};
 handle.devh = &h;
 handle.cur_ctrl.bFormatIndex = 1;
 handle.cur_ctrl.bFrameIndex = 1;
 handle.frame_format = UVC_FRAME_FORMAT_MJPEG;
 handle.holdbuf = image;
 handle.meta_holdbuf = metadata;
 handle.hold_bytes = sizeof(image);
 handle.meta_hold_bytes = sizeof(metadata);
 assert(_uvc_populate_frame(&handle) == UVC_SUCCESS);
 assert(handle.frame_data_capacity == sizeof(image));
 assert(handle.frame_metadata_capacity == sizeof(metadata));
 assert(handle.frame.data_bytes == sizeof(image));
 assert(handle.frame.metadata_bytes == sizeof(metadata));
 assert(memcmp(handle.frame.data, image, sizeof(image)) == 0);
 assert(memcmp(handle.frame.metadata, metadata, sizeof(metadata)) == 0);

 void *image_allocation = handle.frame.data;
 void *metadata_allocation = handle.frame.metadata;
 handle.hold_bytes = 100;
 handle.meta_hold_bytes = 5;
 assert(_uvc_populate_frame(&handle) == UVC_SUCCESS);
 assert(handle.frame.data == image_allocation);
 assert(handle.frame.metadata == metadata_allocation);
 assert(handle.frame_data_capacity == sizeof(image));
 assert(handle.frame_metadata_capacity == sizeof(metadata));
 assert(handle.frame.data_bytes == 100);
 assert(handle.frame.metadata_bytes == 5);

 handle.hold_bytes = 150;
 handle.meta_hold_bytes = 12;
 assert(_uvc_populate_frame(&handle) == UVC_SUCCESS);
 assert(handle.frame.data == image_allocation);
 assert(handle.frame.metadata == metadata_allocation);
 assert(handle.frame_data_capacity == sizeof(image));
 assert(handle.frame_metadata_capacity == sizeof(metadata));
 assert(handle.frame.data_bytes == 150);
 assert(handle.frame.metadata_bytes == 12);

 handle.meta_hold_bytes = 0;
 assert(_uvc_populate_frame(&handle) == UVC_SUCCESS);
 assert(handle.frame.metadata_bytes == 0);
 free(handle.frame.data);
 free(handle.frame.metadata);
 return 0;
}
