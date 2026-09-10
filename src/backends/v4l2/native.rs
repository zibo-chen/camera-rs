//! Only this module crosses the C boundary. The worker owns all mapped buffers.
use super::convert::Plane;
use crate::{CameraError, CameraResult};
use std::{ffi::c_void, os::fd::RawFd, ptr::NonNull};

#[repr(C)]
#[derive(Default)]
pub(super) struct Info {
    pub card: [u8; 32],
    pub driver: [u8; 16],
    pub bus: [u8; 32],
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(super) struct Format {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub fps_denominator: u32,
    pub strides: [u32; 2],
    pub planes: u32,
    pub ycbcr: u32,
    pub full_range: u32,
}
#[repr(C)]
#[derive(Default)]
pub(super) struct Size {
    pub kind: u32,
    pub min_w: u32,
    pub max_w: u32,
    pub step_w: u32,
    pub min_h: u32,
    pub max_h: u32,
    pub step_h: u32,
}
#[repr(C)]
#[derive(Default)]
pub(super) struct Interval {
    pub kind: u32,
    pub min_n: u32,
    pub min_d: u32,
    pub max_n: u32,
    pub max_d: u32,
    pub step_n: u32,
    pub step_d: u32,
}
#[repr(C)]
#[derive(Default)]
pub(super) struct Control {
    pub min: i32,
    pub max: i32,
    pub step: i32,
    pub def: i32,
    pub read_only: u32,
}
#[repr(C)]
#[derive(Default)]
struct Frame {
    data: [*const u8; 2],
    lengths: [usize; 2],
    timestamp_ns: i64,
    index: u32,
    planes: u32,
    damaged: u32,
    sequence: u32,
    monotonic: u32,
}

extern "C" {
    fn camera_v4l2_probe(fd: i32, info: *mut Info) -> i32;
    fn camera_v4l2_enum_format(fd: i32, index: u32, fourcc: *mut u32) -> i32;
    fn camera_v4l2_enum_size(fd: i32, fourcc: u32, index: u32, size: *mut Size) -> i32;
    fn camera_v4l2_enum_interval(
        fd: i32,
        fourcc: u32,
        w: u32,
        h: u32,
        index: u32,
        interval: *mut Interval,
    ) -> i32;
    fn camera_v4l2_format_set(fd: i32, format: *mut Format, apply: i32) -> i32;
    fn camera_v4l2_start(fd: i32, count: u32, out: *mut *mut c_void) -> i32;
    fn camera_v4l2_next(stream: *mut c_void, wake_fd: i32, frame: *mut Frame) -> i32;
    fn camera_v4l2_release(stream: *mut c_void, index: u32) -> i32;
    fn camera_v4l2_stop(stream: *mut c_void) -> i32;
    fn camera_v4l2_query_control(fd: i32, id: u32, control: *mut Control) -> i32;
    fn camera_v4l2_get_control(fd: i32, id: u32, value: *mut i32) -> i32;
    fn camera_v4l2_set_control(fd: i32, id: u32, value: i32) -> i32;
    fn camera_v4l2_auto_exposure(fd: i32, enabled: i32) -> i32;
}

fn check(rc: i32) -> std::io::Result<()> {
    if rc < 0 {
        Err(std::io::Error::from_raw_os_error(-rc))
    } else {
        Ok(())
    }
}
fn enumerated<T>(rc: i32, value: T) -> std::io::Result<Option<T>> {
    if rc == -libc::EINVAL {
        Ok(None)
    } else {
        check(rc).map(|()| Some(value))
    }
}
pub(super) fn probe(fd: RawFd) -> std::io::Result<Info> {
    let mut info = Info::default();
    // SAFETY: out-parameters have exactly the repr(C) layout in v4l2_bridge.h.
    check(unsafe { camera_v4l2_probe(fd, &mut info) })?;
    Ok(info)
}
pub(super) fn formats(fd: RawFd, index: u32) -> std::io::Result<Option<u32>> {
    let mut code = 0;
    let rc = unsafe { camera_v4l2_enum_format(fd, index, &mut code) };
    enumerated(rc, code)
}
pub(super) fn sizes(fd: RawFd, code: u32, index: u32) -> std::io::Result<Option<Size>> {
    let mut size = Size::default();
    let rc = unsafe { camera_v4l2_enum_size(fd, code, index, &mut size) };
    enumerated(rc, size)
}
pub(super) fn intervals(
    fd: RawFd,
    code: u32,
    w: u32,
    h: u32,
    index: u32,
) -> std::io::Result<Option<Interval>> {
    let mut interval = Interval::default();
    let rc = unsafe { camera_v4l2_enum_interval(fd, code, w, h, index, &mut interval) };
    enumerated(rc, interval)
}
pub(super) fn configure(fd: RawFd, format: &mut Format, apply: bool) -> std::io::Result<()> {
    check(unsafe { camera_v4l2_format_set(fd, format, i32::from(apply)) })
}
pub(super) fn query_control(fd: RawFd, id: u32) -> std::io::Result<Control> {
    let mut control = Control::default();
    check(unsafe { camera_v4l2_query_control(fd, id, &mut control) })?;
    Ok(control)
}
pub(super) fn get_control(fd: RawFd, id: u32) -> std::io::Result<i32> {
    let mut value = 0;
    check(unsafe { camera_v4l2_get_control(fd, id, &mut value) })?;
    Ok(value)
}
pub(super) fn set_control(fd: RawFd, id: u32, value: i32) -> std::io::Result<()> {
    check(unsafe { camera_v4l2_set_control(fd, id, value) })
}
pub(super) fn auto_exposure(fd: RawFd, enabled: bool) -> std::io::Result<()> {
    check(unsafe { camera_v4l2_auto_exposure(fd, i32::from(enabled)) })
}

/// The stream borrows its file, cannot cross threads, and never exposes mapped
/// memory past the callback (which runs before QBUF returns it to the driver).
pub(super) struct Stream<'a> {
    ptr: Option<NonNull<c_void>>,
    _file: &'a std::fs::File,
}
impl<'a> Stream<'a> {
    pub fn start(file: &'a std::fs::File, count: u32) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        let mut ptr = std::ptr::null_mut();
        check(unsafe { camera_v4l2_start(file.as_raw_fd(), count, &mut ptr) })?;
        Ok(Self {
            ptr: Some(
                NonNull::new(ptr)
                    .ok_or_else(|| std::io::Error::other("V4L2 returned null stream"))?,
            ),
            _file: file,
        })
    }
    pub fn next(
        &mut self,
        wake_fd: RawFd,
        strides: [u32; 2],
        consume: impl FnOnce(&[Plane<'_>], i64, crate::ClockDomain, u64, bool) -> CameraResult<()>,
    ) -> CameraResult<()> {
        let ptr = self.ptr.unwrap().as_ptr();
        let mut frame = Frame::default();
        check(unsafe { camera_v4l2_next(ptr, wake_fd, &mut frame) })?;
        struct Requeue {
            ptr: *mut c_void,
            index: u32,
        }
        impl Drop for Requeue {
            fn drop(&mut self) {
                let _ = unsafe { camera_v4l2_release(self.ptr, self.index) };
            }
        }
        let requeue = Requeue {
            ptr,
            index: frame.index,
        };
        if !(1..=2).contains(&frame.planes) {
            return Err(CameraError::InvalidFormat(
                "Invalid V4L2 plane count".into(),
            ));
        }
        let mut planes = [Plane {
            data: &[],
            stride: 0,
        }; 2];
        for (p, &stride) in strides.iter().enumerate().take(frame.planes as usize) {
            // SAFETY: C validates bytesused/data_offset against this mmap's
            // length. The device cannot write it until the QBUF below.
            let data = unsafe { std::slice::from_raw_parts(frame.data[p], frame.lengths[p]) };
            planes[p] = Plane {
                data,
                stride: stride as usize,
            };
        }
        let result = consume(
            &planes[..frame.planes as usize],
            frame.timestamp_ns,
            if frame.monotonic != 0 {
                crate::ClockDomain::HostMonotonic
            } else {
                crate::ClockDomain::Unknown
            },
            frame.sequence as u64,
            frame.damaged != 0,
        );
        std::mem::forget(requeue);
        check(unsafe { camera_v4l2_release(ptr, frame.index) })?;
        result
    }
    pub fn stop(mut self) -> std::io::Result<()> {
        let ptr = self.ptr.take().unwrap();
        check(unsafe { camera_v4l2_stop(ptr.as_ptr()) })
    }
}
impl Drop for Stream<'_> {
    fn drop(&mut self) {
        if let Some(ptr) = self.ptr.take() {
            // SAFETY: sole owner; STREAMOFF/unmap/free also handles partial startup.
            let rc = unsafe { camera_v4l2_stop(ptr.as_ptr()) };
            if rc < 0 {
                log::debug!("V4L2 cleanup: {}", std::io::Error::from_raw_os_error(-rc));
            }
        }
    }
}
