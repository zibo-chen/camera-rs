// Fault injection against the actual vendored close and status callback paths.
#include <assert.h>
#include <stdatomic.h>
#include <time.h>
#include "../3rdparty/libuvc/src/device.c"
static pthread_t callback_thread;
static atomic_int callback_done, released, attached, closed;
static int detach_result, claim_result, submit_result, submissions;
int libusb_submit_transfer(struct libusb_transfer *t) { (void)t; submissions++; return submit_result; }
static void *complete_cancel(void *ptr) {
  struct timespec delay = {0, 20000000};
  nanosleep(&delay, NULL);
  struct libusb_transfer *t = ptr;
  // Completion may win the race with cancel; closing must still prevent resubmit.
  t->status = LIBUSB_TRANSFER_COMPLETED;
  atomic_store(&callback_done, 1);
  _uvc_status_callback(t);
  return NULL;
}
int libusb_cancel_transfer(struct libusb_transfer *t) {
  assert(pthread_create(&callback_thread, NULL, complete_cancel, t) == 0); return 0;
}
int libusb_set_interface_alt_setting(libusb_device_handle *h, int i, int a) { (void)h; (void)i; (void)a; return 0; }
int libusb_release_interface(libusb_device_handle *h, int i) {
  (void)h; (void)i; assert(atomic_load(&callback_done)); atomic_fetch_add(&released, 1); return 0;
}
int libusb_attach_kernel_driver(libusb_device_handle *h, int i) { (void)h; (void)i; atomic_fetch_add(&attached, 1); return 0; }
int libusb_detach_kernel_driver(libusb_device_handle *h, int i) { (void)h; (void)i; return detach_result; }
int libusb_claim_interface(libusb_device_handle *h, int i) { (void)h; (void)i; return claim_result; }
void libusb_close(libusb_device_handle *h) { (void)h; assert(atomic_load(&callback_done)); atomic_fetch_add(&closed, 1); }
void libusb_unref_device(libusb_device *d) { (void)d; }
void libusb_free_config_descriptor(struct libusb_config_descriptor *d) { free(d); }
void libusb_free_transfer(struct libusb_transfer *t) { assert(atomic_load(&callback_done)); free(t); }
void uvc_stop_streaming(uvc_device_handle_t *d) { (void)d; assert(0); }
int main(void) {
  uvc_context_t ctx = {0};
  uvc_device_handle_t *h = calloc(1, sizeof(*h));
  h->dev = calloc(1, sizeof(*h->dev)); h->dev->ctx = &ctx; h->dev->ref = 1;
  h->info = calloc(1, sizeof(*h->info));
  h->status_xfer = calloc(1, sizeof(*h->status_xfer)); h->status_xfer->user_data = h;
  h->status_submitted = 1; h->claimed = h->detached = 1;
  pthread_mutex_init(&h->status_mutex, NULL); pthread_cond_init(&h->status_cond, NULL);
  h->prev = h; ctx.open_devices = h;
  uvc_close(h);
  pthread_join(callback_thread, NULL);
  assert(submissions == 0 && released == 1 && attached == 1 && closed == 1 && ctx.open_devices == NULL);
  // Claiming an interface without a kernel driver must not attach a new one on release.
  uvc_device_handle_t stack = {0}; detach_result = LIBUSB_ERROR_NOT_FOUND;
  assert(uvc_claim_if(&stack, 1) == 0 && stack.detached == 0);
  assert(uvc_release_if(&stack, 1) == 0 && attached == 1);
  // A failed claim must undo the successful detach.
  detach_result = 0; claim_result = LIBUSB_ERROR_BUSY;
  assert(uvc_claim_if(&stack, 0) == UVC_ERROR_BUSY && attached == 2 && stack.detached == 0);
  assert(uvc_claim_if(&stack, 32) == UVC_ERROR_INVALID_PARAM);
  assert(uvc_release_if(&stack, -1) == UVC_ERROR_INVALID_PARAM);
  return 0;
}
