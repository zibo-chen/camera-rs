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
       -> recovery wake [disconnect / source stall / bad-frame burst]
```

`Capture` is only a convenience owner of one `Session` and one latest receiver. It uses the same path as the complete API. Receivers and retained frames never control camera lifetime.

Features compile backend, conversion and integration capabilities independently. Runtime policy cannot enable code absent from the build. Platform modules, FFI types, native locking and compatibility traits stay private; external implementations use `BackendProvider`, `BackendDevice`, `FrameSink` and `WritableFrameLease`.

Backends publish native layout and bytes into a bounded pool. Conversion occurs after an individual consumer receives a frame. Conversion and full-frame copies do not run while the pool lock is held. Epoch checks reject callbacks from an old or stopped capture. Driver-buffer leases, GPU frames and unbounded queues are deliberately outside this contract.

Native thread-affine objects remain inside their backend worker. Public control/open/configuration calls may use dynamic dispatch because they are not pixel hot paths. The frame hot path does not create one async task per frame.

Automatic recovery has separate "enabled" and "actively recovering" states, so the backend-stop notification cannot race a receiver into a terminal error. Explicit disconnects wake the monitor immediately. Silent stalls wake at their exact activity deadline without scheduling the monitor on every frame. Decode/format failures remain frame-local until eight are consecutive; recovery first tries a bounded soft stream restart and escalates to device re-enumeration/reopen after failure. Every successful recovery creates a new frame epoch, rejects late callbacks from the old epoch, and becomes `Streaming` only after first-frame validation.
