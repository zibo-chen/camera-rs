//! Stable extension surface for application-provided camera backends.
use crate::frame::NativeWriteLease;
use crate::{
    BackendId, CameraFacing, CameraResult, CapturePlan, DeviceCapabilities, FrameHub, FrameLayout,
    IdentityStability, SourceTimestamp, UsbIdentity,
};
use std::{fmt, sync::Arc};

#[derive(Clone, Debug)]
pub struct BackendDeviceInfo {
    pub native_id: String,
    pub name: String,
    pub description: String,
    pub stability: IdentityStability,
    pub facing: CameraFacing,
    pub usb: Option<UsbIdentity>,
}

pub trait BackendProvider: fmt::Debug + Send + Sync + 'static {
    fn id(&self) -> BackendId;
    fn enumerate(&self) -> CameraResult<Vec<BackendDeviceInfo>>;
    fn open(&self, native_id: &str) -> CameraResult<Box<dyn BackendDevice>>;
}

/// A device handle may keep native thread-affine objects inside its own worker.
/// Calls are serialized by the framework and never occur on the per-frame path.
pub trait BackendDevice: Send + 'static {
    fn capabilities(&self) -> CameraResult<DeviceCapabilities>;
    fn start(&mut self, sink: FrameSink, plan: &CapturePlan) -> CameraResult<()>;
    fn stop(&mut self) -> CameraResult<()>;
}

#[derive(Clone)]
pub struct FrameSink {
    hub: FrameHub,
    session: u64,
}

impl fmt::Debug for FrameSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSink")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl FrameSink {
    pub(crate) fn begin(hub: FrameHub) -> Self {
        let session = hub.start();
        Self { hub, session }
    }

    pub fn publish_bytes(
        &self,
        layout: FrameLayout,
        timestamp: Option<SourceTimestamp>,
        bytes: &[u8],
    ) -> CameraResult<bool> {
        self.hub
            .publish_native(self.session, layout, timestamp, None, &[bytes])
    }

    pub fn writable(&self, layout: FrameLayout) -> CameraResult<WritableFrameLease> {
        Ok(WritableFrameLease {
            inner: Some(self.hub.writable_native(self.session, layout)?),
        })
    }

    /// Signal that the physical stream ended unexpectedly. This wakes automatic
    /// recovery immediately instead of waiting for the no-frame timeout.
    pub fn disconnect(&self) {
        self.hub.stop();
    }
}

/// Writable storage borrowed directly from the framework's bounded frame pool.
/// `commit` makes it immutable and visible to subscribers; dropping it returns
/// the slot to the pool without publishing.
pub struct WritableFrameLease {
    inner: Option<NativeWriteLease>,
}

impl WritableFrameLease {
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.inner
            .as_mut()
            .expect("lease is present until commit")
            .bytes_mut()
    }

    pub fn timestamp(mut self, timestamp: SourceTimestamp) -> Self {
        self.inner
            .as_mut()
            .expect("lease is present until commit")
            .set_timestamp(timestamp);
        self
    }

    pub fn commit(mut self) -> CameraResult<bool> {
        self.inner
            .take()
            .expect("lease is present until commit")
            .commit()
    }
}

pub(crate) struct RegisteredBackend {
    pub id: BackendId,
    pub provider: Arc<dyn BackendProvider>,
}

impl Clone for RegisteredBackend {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            provider: self.provider.clone(),
        }
    }
}

impl fmt::Debug for RegisteredBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisteredBackend")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CameraError, PixelFormat, PlaneLayout, SessionEvent};

    fn rgb_layout() -> FrameLayout {
        FrameLayout {
            width: 2,
            height: 1,
            format: PixelFormat::Rgb8,
            planes: vec![PlaneLayout {
                offset: 0,
                length: 6,
                row_stride: 6,
                pixel_stride: 3,
            }],
            color: Default::default(),
            orientation: Default::default(),
            bottom_up: false,
        }
    }

    #[test]
    fn writable_lease_uses_the_bounded_pool_and_never_overwrites_retained_frames() {
        let hub = FrameHub::new(2);
        let sink = FrameSink::begin(hub.clone());
        let (events, mut receiver) = tokio::sync::broadcast::channel(4);
        hub.set_event_sender(events);

        let mut first_lease = sink.writable(rgb_layout()).unwrap();
        first_lease.bytes_mut().fill(1);
        assert!(first_lease.commit().unwrap());
        let first = hub.latest().unwrap();

        let mut second_lease = sink.writable(rgb_layout()).unwrap();
        second_lease.bytes_mut().fill(2);
        assert!(second_lease.commit().unwrap());
        let second = hub.latest().unwrap();

        assert!(matches!(
            sink.writable(rgb_layout()),
            Err(CameraError::BufferEmpty)
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::ResourcePressure { dropped_frames: 1 }
        ));

        drop(first);
        let mut third_lease = sink.writable(rgb_layout()).unwrap();
        third_lease.bytes_mut().fill(3);
        assert!(third_lease.commit().unwrap());
        assert_eq!(second.bytes(), &[2; 6]);
        assert_eq!(hub.latest().unwrap().bytes(), &[3; 6]);
    }

    #[test]
    fn custom_backend_can_signal_a_physical_disconnect_immediately() {
        let hub = FrameHub::new(2);
        let sink = FrameSink::begin(hub.clone());
        assert!(hub.is_streaming());

        sink.disconnect();

        assert!(!hub.is_streaming());
    }
}
