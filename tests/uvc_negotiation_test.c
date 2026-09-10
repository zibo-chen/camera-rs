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
 return 0;
}
