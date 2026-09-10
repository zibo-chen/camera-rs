//! Linux Video4Linux2 capture. Device indices are `/dev/videoN` suffixes.
//! Uses the kernel driver directly; no libusb/libuvc/libv4l2 is required.
#[cfg(camera_v4l2)]
mod camera;
mod convert;
#[cfg(camera_v4l2)]
mod native;
#[cfg(camera_v4l2)]
pub use camera::V4l2Camera;
#[cfg(test)]
mod tests;
