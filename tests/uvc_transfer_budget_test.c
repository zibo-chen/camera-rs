#include <assert.h>
#include "../3rdparty/libuvc/src/stream.c"

static int submissions, frees, fail_after;
int libusb_submit_transfer(struct libusb_transfer *transfer) {
  (void)transfer;
  return submissions++ == fail_after ? LIBUSB_ERROR_NO_MEM : 0;
}
void libusb_free_transfer(struct libusb_transfer *transfer) {
  frees++;
  free(transfer);
}
static void allocate(uvc_stream_handle_t *stream) {
  for (int i = 0; i < LIBUVC_NUM_TRANSFER_BUFS; i++) {
    stream->transfers[i] = calloc(1, sizeof(struct libusb_transfer));
    stream->transfers[i]->buffer = malloc(32);
  }
  submissions = frees = 0;
}
int main(void) {
  size_t packets = _uvc_iso_packets_per_transfer(1843200, 3072);
#ifdef __ANDROID__
  assert(packets == 5 && packets * 3072 <= 16384);
  assert(_uvc_iso_packets_per_transfer(1843200, 1024) == 16);
#else
  assert(packets == 32);
#endif
  assert(_uvc_iso_packets_per_transfer(100, 3072) == 1);
  uvc_stream_handle_t stream = {0};
  allocate(&stream); fail_after = 0;
  assert(_uvc_submit_transfers(&stream) == UVC_ERROR_NO_MEM);
  assert(submissions == 1 && frees == LIBUVC_NUM_TRANSFER_BUFS);
  for (int i = 0; i < LIBUVC_NUM_TRANSFER_BUFS; i++) assert(!stream.transfers[i]);
  allocate(&stream); fail_after = 2;
  assert(_uvc_submit_transfers(&stream) == UVC_SUCCESS);
  assert(frees == LIBUVC_NUM_TRANSFER_BUFS - 2);
  for (int i = 0; i < 2; i++) {
    free(stream.transfers[i]->buffer);
    libusb_free_transfer(stream.transfers[i]);
    stream.transfers[i] = NULL;
  }
  return 0;
}
