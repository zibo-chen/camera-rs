//! UVC context and device management.

use super::ffi;
use crate::error::{CameraError, Result, UvcErrorCode};
use crate::types::CameraDeviceInfo;
use std::ffi::CStr;
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};
use std::ptr;
use std::sync::Arc;

/// UVC context.
pub struct UvcContext {
    inner: Arc<ContextInner>,
}
struct ContextInner {
    ctx: *mut ffi::UvcContext,
}
unsafe impl Send for ContextInner {}
unsafe impl Sync for ContextInner {}

impl UvcContext {
    /// Creates a new UVC context.
    pub fn new() -> Result<Self> {
        // Android must disable libusb discovery so an authorized external file
        // descriptor can be used without root access.
        #[cfg(unix)]
        {
            log::debug!("Setting libusb NO_DEVICE_DISCOVERY option for Android");
            let result = unsafe {
                ffi::libusb_set_option(ptr::null_mut(), ffi::LibusbOption::NoDeviceDiscovery)
            };
            if result != 0 {
                log::warn!(
                    "libusb_set_option returned {}, but continuing anyway",
                    result
                );
            }
        }

        let mut ctx: *mut ffi::UvcContext = ptr::null_mut();
        let result = unsafe { ffi::uvc_init(&mut ctx, ptr::null_mut()) };

        UvcErrorCode::check(result, crate::OperationStage::Startup, "initialize UVC")?;

        Ok(UvcContext {
            inner: Arc::new(ContextInner { ctx }),
        })
    }

    /// Finds a device by VID and PID.
    pub fn find_device(&mut self, vid: i32, pid: i32) -> Result<UvcDevice> {
        let mut dev: *mut ffi::UvcDevice = ptr::null_mut();
        let result =
            unsafe { ffi::uvc_find_device(self.inner.ctx, &mut dev, vid, pid, ptr::null()) };

        UvcErrorCode::check(result, crate::OperationStage::Enumeration, "find device")?;

        Ok(UvcDevice {
            dev,
            owned: true,
            context: self.inner.clone(),
        })
    }

    /// Returns all discovered devices.
    pub fn get_device_list(&mut self) -> Result<Vec<UvcDevice>> {
        let mut list: *mut *mut ffi::UvcDevice = ptr::null_mut();
        let result = unsafe { ffi::uvc_get_device_list(self.inner.ctx, &mut list) };

        UvcErrorCode::check(result, crate::OperationStage::Enumeration, "list devices")?;

        let mut devices = Vec::new();
        if !list.is_null() {
            let mut i = 0;
            unsafe {
                while !(*list.add(i)).is_null() {
                    let dev = *list.add(i);
                    // uvc_get_device_list has already incremented every device's
                    // reference count, so take ownership without another ref.
                    devices.push(UvcDevice {
                        dev,
                        owned: true,
                        context: self.inner.clone(),
                    });
                    i += 1;
                }
                // Pass zero to retain the references now owned by UvcDevice;
                // each device releases its reference from Drop.
                ffi::uvc_free_device_list(list, 0);
            }
        }

        Ok(devices)
    }

    /// Wrap a borrowed descriptor. On Unix the handle owns a duplicate, so the
    /// caller can close its descriptor after this method returns.
    pub fn wrap_device(&mut self, fd: i32) -> Result<UvcDeviceHandle> {
        let mut devh: *mut ffi::UvcDeviceHandle = ptr::null_mut();
        #[cfg(unix)]
        let owned_fd = crate::owned_fd::duplicate(fd)?;
        #[cfg(unix)]
        let fd = owned_fd.as_raw_fd();
        let result = unsafe { ffi::uvc_wrap(fd, self.inner.ctx, &mut devh) };

        UvcErrorCode::check(result, crate::OperationStage::Open, "wrap USB descriptor")?;

        Ok(UvcDeviceHandle {
            devh,
            _context: self.inner.clone(),
            #[cfg(unix)]
            _fd: Some(owned_fd),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn as_ptr(&self) -> *mut ffi::UvcContext {
        self.inner.ctx
    }
}

impl Drop for ContextInner {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            log::debug!("释放 UVC 上下文");
            unsafe { ffi::uvc_exit(self.ctx) };
        }
    }
}

unsafe impl Send for UvcContext {}
unsafe impl Sync for UvcContext {}

/// UVC device.
pub struct UvcDevice {
    dev: *mut ffi::UvcDevice,
    owned: bool,
    context: Arc<ContextInner>,
}

impl UvcDevice {
    /// Returns the device descriptor.
    pub fn get_descriptor(&self) -> Result<UvcDeviceDescriptor> {
        let mut desc: *mut ffi::UvcDeviceDescriptor = ptr::null_mut();
        let result = unsafe { ffi::uvc_get_device_descriptor(self.dev, &mut desc) };

        UvcErrorCode::check(
            result,
            crate::OperationStage::Enumeration,
            "read device descriptor",
        )?;

        Ok(UvcDeviceDescriptor { desc })
    }

    /// Opens the device.
    pub fn open(&self) -> Result<UvcDeviceHandle> {
        let mut devh: *mut ffi::UvcDeviceHandle = ptr::null_mut();
        let result = unsafe { ffi::uvc_open(self.dev, &mut devh) };

        UvcErrorCode::check(result, crate::OperationStage::Open, "open device")?;

        Ok(UvcDeviceHandle {
            devh,
            _context: self.context.clone(),
            #[cfg(unix)]
            _fd: None,
        })
    }

    /// Converts the device into public device information.
    pub fn to_device_info(&self, index: u32) -> Result<CameraDeviceInfo> {
        let desc = self.get_descriptor()?;
        let name = desc.product().unwrap_or_else(|| {
            format!(
                "UVC Camera {:04x}:{:04x}",
                desc.vendor_id(),
                desc.product_id()
            )
        });
        let description = desc
            .manufacturer()
            .unwrap_or_else(|| "USB Video Device".to_string());

        let mut info = CameraDeviceInfo::new(index, name, description);
        info.vendor_id = Some(desc.vendor_id());
        info.product_id = Some(desc.product_id());
        info.serial_number = desc.serial_number();

        Ok(info)
    }

    #[allow(dead_code)]
    pub(crate) fn as_ptr(&self) -> *mut ffi::UvcDevice {
        self.dev
    }
}

impl Drop for UvcDevice {
    fn drop(&mut self) {
        if self.owned && !self.dev.is_null() {
            unsafe { ffi::uvc_unref_device(self.dev) };
        }
    }
}

unsafe impl Send for UvcDevice {}
unsafe impl Sync for UvcDevice {}

/// UVC device descriptor.
pub struct UvcDeviceDescriptor {
    desc: *mut ffi::UvcDeviceDescriptor,
}

impl UvcDeviceDescriptor {
    pub fn vendor_id(&self) -> u16 {
        unsafe { (*self.desc).vendor_id }
    }

    pub fn product_id(&self) -> u16 {
        unsafe { (*self.desc).product_id }
    }

    pub fn bcd_uvc(&self) -> u16 {
        unsafe { (*self.desc).bcd_uvc }
    }

    pub fn serial_number(&self) -> Option<String> {
        unsafe {
            let ptr = (*self.desc).serial_number;
            if ptr.is_null() {
                None
            } else {
                CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
            }
        }
    }

    pub fn manufacturer(&self) -> Option<String> {
        unsafe {
            let ptr = (*self.desc).manufacturer;
            if ptr.is_null() {
                None
            } else {
                CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
            }
        }
    }

    pub fn product(&self) -> Option<String> {
        unsafe {
            let ptr = (*self.desc).product;
            if ptr.is_null() {
                None
            } else {
                CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
            }
        }
    }
}

impl Drop for UvcDeviceDescriptor {
    fn drop(&mut self) {
        if !self.desc.is_null() {
            unsafe { ffi::uvc_free_device_descriptor(self.desc) };
        }
    }
}

/// UVC device handle.
pub struct UvcDeviceHandle {
    devh: *mut ffi::UvcDeviceHandle,
    _context: Arc<ContextInner>,
    // Native uvc_close runs in Drop before this descriptor is closed.
    #[cfg(unix)]
    _fd: Option<OwnedFd>,
}

impl UvcDeviceHandle {
    /// Negotiates stream parameters.
    pub fn get_stream_ctrl(
        &mut self,
        format: ffi::UvcFrameFormat,
        width: i32,
        height: i32,
        fps: i32,
    ) -> Result<StreamCtrl> {
        let ctrl = Box::new(unsafe { std::mem::zeroed::<ffi::UvcStreamCtrl>() });
        let ctrl_ptr = Box::into_raw(ctrl);

        let result = unsafe {
            ffi::uvc_get_stream_ctrl_format_size(self.devh, ctrl_ptr, format, width, height, fps)
        };

        if result != ffi::UVC_SUCCESS {
            unsafe {
                let _ = Box::from_raw(ctrl_ptr);
            }
            return Err(CameraError::uvc_backend(
                result,
                format!(
                    "Select {format:?} {width}x{height}: {}",
                    UvcErrorCode::from_code(result).message()
                ),
            )
            .with_stage(crate::OperationStage::Capabilities));
        }

        Ok(StreamCtrl { ctrl: ctrl_ptr })
    }

    pub(crate) fn probe_stream_ctrl(&mut self, ctrl: &mut StreamCtrl) -> Result<()> {
        let code = unsafe { ffi::uvc_probe_stream_ctrl(self.devh, ctrl.ctrl) };
        if code != ffi::UVC_SUCCESS {
            return Err(
                CameraError::uvc_backend(code, "Probe fractional frame interval".into())
                    .with_stage(crate::OperationStage::Startup),
            );
        }
        Ok(())
    }
    /// Starts streaming.
    pub unsafe fn start_streaming(
        &mut self,
        ctrl: &mut StreamCtrl,
        callback: ffi::UvcFrameCallback,
        user_data: *mut std::ffi::c_void,
    ) -> Result<()> {
        let result = ffi::uvc_start_streaming(self.devh, ctrl.ctrl, callback, user_data, 0);
        if result == ffi::UVC_SUCCESS {
            return Ok(());
        }
        let c = &*ctrl.ctrl;
        Err(CameraError::uvc_backend(
            result,
            format!(
                "Start format {} frame {} interval {} payload {}: {}",
                c.b_format_index,
                c.b_frame_index,
                c.dw_frame_interval,
                c.dw_max_payload_transfer_size,
                UvcErrorCode::from_code(result).message()
            ),
        )
        .with_stage(crate::OperationStage::Startup))
    }

    /// Stops streaming.
    pub fn stop_streaming(&mut self) {
        unsafe { ffi::uvc_stop_streaming(self.devh) };
    }

    #[allow(dead_code)]
    pub(crate) fn as_ptr(&self) -> *mut ffi::UvcDeviceHandle {
        self.devh
    }
}

impl Drop for UvcDeviceHandle {
    fn drop(&mut self) {
        if !self.devh.is_null() {
            log::debug!("关闭 UVC 设备句柄");
            unsafe { ffi::uvc_close(self.devh) };
        }
    }
}

unsafe impl Send for UvcDeviceHandle {}
unsafe impl Sync for UvcDeviceHandle {}

/// Stream control parameters.
pub struct StreamCtrl {
    ctrl: *mut ffi::UvcStreamCtrl,
}

impl StreamCtrl {
    pub(crate) fn set_frame_interval(&mut self, interval: u32) {
        unsafe {
            (*self.ctrl).dw_frame_interval = interval;
        }
    }
    pub(crate) fn frame_interval(&self) -> u32 {
        unsafe { (*self.ctrl).dw_frame_interval }
    }
}

impl Drop for StreamCtrl {
    fn drop(&mut self) {
        if !self.ctrl.is_null() {
            unsafe {
                let _ = Box::from_raw(self.ctrl);
            }
        }
    }
}

unsafe impl Send for StreamCtrl {}
unsafe impl Sync for StreamCtrl {}
