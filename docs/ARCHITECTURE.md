# Architecture

The public design separates intent, lifecycle and extension concerns without duplicating capture logic.

```text
CameraSystem
  + BackendPolicy + registered BackendProvider(s)
  -> DeviceSelector -> Device
  -> CaptureRequest -> CapturePlan -> native start + first-frame validation
  -> Session (sole capture lifetime owner)
       -> FrameHub (bounded immutable pool, capture epoch)
            -> FrameReceiver [Latest / Buffered / max_rate]
            -> FrameReceiver [independent policy]
       -> current state watch
       -> bounded event stream with missed-count reporting
```

`Capture` is only a convenience owner of one `Session` and one latest receiver. It uses the same path as the complete API. Receivers and retained frames never control camera lifetime.

Features compile backend, conversion and integration capabilities independently. Runtime policy cannot enable code absent from the build. Platform modules, FFI types, native locking and compatibility traits stay private; external implementations use `BackendProvider`, `BackendDevice`, `FrameSink` and `WritableFrameLease`.

Backends publish native layout and bytes into a bounded pool. Conversion occurs after an individual consumer receives a frame. Conversion and full-frame copies do not run while the pool lock is held. Epoch checks reject callbacks from an old or stopped capture. Driver-buffer leases, GPU frames and unbounded queues are deliberately outside this contract.

Native thread-affine objects remain inside their backend worker. Public control/open/configuration calls may use dynamic dispatch because they are not pixel hot paths. The frame hot path does not create one async task per frame.
