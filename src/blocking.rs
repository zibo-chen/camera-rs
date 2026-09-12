//! Synchronous facade backed by one shared Tokio runtime.
//!
//! It mirrors the owning async types without constructing a runtime per call.
use crate::{
    BackendPolicy, CameraResult, CapturePlan, CaptureRequest, DeviceInfo, DeviceSelector, Frame,
    FrameReceiver, SessionState, SubscriptionOptions,
};
use std::{future::Future, sync::OnceLock, time::Duration};

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("camera blocking runtime")
    })
}

fn run<F: Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

#[derive(Clone, Debug)]
pub struct CameraSystem(crate::CameraSystem);

impl CameraSystem {
    pub fn new() -> Self {
        Self(crate::CameraSystem::new())
    }

    pub fn synthetic() -> Self {
        Self(crate::CameraSystem::synthetic())
    }

    pub fn with_policy(policy: BackendPolicy) -> CameraResult<Self> {
        Ok(Self(
            crate::CameraSystem::builder()
                .backend_policy(policy)
                .build()?,
        ))
    }

    pub fn from_async(system: crate::CameraSystem) -> Self {
        Self(system)
    }

    pub fn devices(&self) -> CameraResult<Vec<DeviceInfo>> {
        self.0.devices_blocking()
    }

    pub fn open(&self, selector: DeviceSelector) -> CameraResult<Device> {
        run(self.0.open(selector)).map(Device)
    }

    pub fn plan(
        &self,
        selector: DeviceSelector,
        request: CaptureRequest,
    ) -> CameraResult<CapturePlan> {
        run(self.0.plan(selector, request))
    }
}

impl Default for CameraSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct Device(crate::Device);

impl Device {
    pub fn info(&self) -> &DeviceInfo {
        self.0.device()
    }

    pub fn start(&mut self, request: CaptureRequest) -> CameraResult<Session> {
        run(self.0.start(request)).map(Session)
    }
}

#[derive(Debug)]
pub struct Session(crate::Session);

impl Session {
    pub fn state(&self) -> SessionState {
        self.0.state()
    }

    pub fn subscribe(&self, options: SubscriptionOptions) -> CameraResult<FrameReceiverSync> {
        self.0.subscribe(options).map(FrameReceiverSync)
    }

    pub fn close(&self) -> CameraResult<()> {
        run(self.0.close())
    }
}

#[derive(Debug)]
pub struct FrameReceiverSync(FrameReceiver);

impl FrameReceiverSync {
    pub fn next_frame(&mut self) -> CameraResult<Frame> {
        run(self.0.next())
    }

    pub fn next_timeout(&mut self, timeout: Duration) -> CameraResult<Frame> {
        run(self.0.next_timeout(timeout))
    }

    pub fn dropped_frames(&self) -> u64 {
        self.0.dropped_frames()
    }
}
