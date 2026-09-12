//! Immutable frames, bounded storage, and independent cancellation-safe readers.
use crate::format::*;
use crate::{pixels::Pixels, CameraError, CameraResult, StreamStats, SubscriptionOptions};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const FRAME_ERROR_RECOVERY_THRESHOLD: u64 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryTrigger {
    SourceStopped,
    SourceStalled,
    FrameErrors { consecutive: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub session: u64,
    pub sequence: u64,
}
#[derive(Clone, Debug)]
enum Storage {
    Rgb(Arc<Pixels<u8>>),
    Bytes(Arc<Vec<u8>>),
}
impl Storage {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Rgb(p) => p.as_slice().expect("packed RGB"),
            Self::Bytes(p) => p,
        }
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Rgb(p) => Arc::get_mut(p)
                .expect("leased RGB")
                .as_slice_mut()
                .expect("packed RGB"),
            Self::Bytes(p) => Arc::get_mut(p).expect("leased bytes"),
        }
    }
    fn unique(&mut self) -> bool {
        fn detach<T: Default>(p: &mut Arc<T>) -> bool {
            if Arc::get_mut(p).is_some() {
                return true;
            }
            if Arc::strong_count(p) != 1 {
                return false;
            }
            let old = std::mem::replace(p, Arc::new(T::default()));
            match Arc::try_unwrap(old) {
                Ok(data) => {
                    *p = Arc::new(data);
                    true
                }
                Err(shared) => {
                    *p = shared;
                    false
                }
            }
        }
        match self {
            Self::Rgb(p) => {
                if Arc::get_mut(p).is_some() {
                    return true;
                }
                if Arc::strong_count(p) != 1 {
                    return false;
                }
                let old = std::mem::replace(p, Arc::new(Pixels::zeros((0, 0, 3))));
                match Arc::try_unwrap(old) {
                    Ok(data) => {
                        *p = Arc::new(data);
                        true
                    }
                    Err(shared) => {
                        *p = shared;
                        false
                    }
                }
            }
            Self::Bytes(p) => detach(p),
        }
    }
    fn retained(&self) -> bool {
        match self {
            Self::Rgb(p) => Arc::strong_count(p) > 1,
            Self::Bytes(p) => Arc::strong_count(p) > 1,
        }
    }
}
/// Cheaply shared immutable frame. RGB/native bytes never change while retained.
#[derive(Debug)]
pub struct CapturedFrame {
    pub key: FrameKey,
    storage: Storage,
    layout: FrameLayout,
    /// Host monotonic processing-entry time, not the sensor exposure time.
    pub captured_at: Instant,
    pub timestamp_ns: u64,
    pub source_timestamp_ns: Option<i64>,
    source_clock: ClockDomain,
    pub source_sequence: Option<u64>,
}
pub type Frame = Arc<CapturedFrame>;
impl CapturedFrame {
    pub fn age(&self) -> Duration {
        self.captured_at.elapsed()
    }
    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }
    pub fn bytes(&self) -> &[u8] {
        self.storage.bytes()
    }
    pub fn plane(&self, index: usize) -> Option<&[u8]> {
        let p = self.layout.planes.get(index)?;
        self.bytes().get(p.offset..p.offset + p.length)
    }
    pub fn rgb_view(&self) -> CameraResult<RgbView<'_>> {
        if self.layout.format != PixelFormat::Rgb8 || self.layout.planes.len() != 1 {
            return Err(CameraError::UnsupportedFormat(
                "Frame layout is not packed RGB8".into(),
            ));
        }
        let plane = self.layout.planes[0];
        if plane.pixel_stride != 3 {
            return Err(CameraError::InvalidFormat(
                "RGB8 pixel stride must be three bytes".into(),
            ));
        }
        let bytes = self
            .bytes()
            .get(plane.offset..plane.offset + plane.length)
            .ok_or_else(|| CameraError::InvalidFormat("RGB plane is outside the frame".into()))?;
        Ok(RgbView {
            bytes,
            width: self.layout.width as usize,
            height: self.layout.height as usize,
            row_stride: plane.row_stride,
        })
    }
    pub fn source_timestamp(&self) -> Option<SourceTimestamp> {
        self.source_timestamp_ns.map(|nanoseconds| SourceTimestamp {
            nanoseconds,
            clock: self.source_clock,
        })
    }
    pub fn copy_to(&self, out: &mut [u8]) -> CameraResult<()> {
        if out.len() != self.bytes().len() {
            return Err(CameraError::InvalidFormat(
                "Destination length differs from frame".into(),
            ));
        }
        out.copy_from_slice(self.bytes());
        Ok(())
    }
    pub fn to_owned_bytes(&self) -> Vec<u8> {
        self.bytes().to_vec()
    }
    #[allow(dead_code)]
    pub(crate) fn rgb_pixels(&self) -> CameraResult<&Arc<Pixels<u8>>> {
        match &self.storage {
            Storage::Rgb(p) => Ok(p),
            _ => Err(CameraError::UnsupportedFormat(
                "Frame is native; request RGB or convert explicitly".into(),
            )),
        }
    }
    #[cfg(feature = "ndarray")]
    pub fn ndarray_view(&self) -> CameraResult<ndarray::ArrayView3<'_, u8>> {
        use ndarray::ShapeBuilder;
        let view = self.rgb_view()?;
        ndarray::ArrayView3::from_shape(
            (view.height, view.width, 3).strides((view.row_stride, 3, 1)),
            view.bytes,
        )
        .map_err(|error| CameraError::InvalidFormat(error.to_string()))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RgbView<'a> {
    bytes: &'a [u8],
    width: usize,
    height: usize,
    row_stride: usize,
}

impl<'a> RgbView<'a> {
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn row_stride(&self) -> usize {
        self.row_stride
    }

    pub fn row(&self, index: usize) -> Option<&'a [u8]> {
        if index >= self.height {
            return None;
        }
        let start = index.checked_mul(self.row_stride)?;
        self.bytes
            .get(start..start.checked_add(self.width.checked_mul(3)?)?)
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}
#[derive(Clone, Default, Debug)]
pub struct FrameMetrics {
    pub received: u64,
    pub published: u64,
    pub pool_drops: u64,
    pub conversion_errors: u64,
    /// Failed conversions since the latest successfully published frame.
    pub consecutive_conversion_errors: u64,
    /// Largest failed-conversion burst observed in the current session.
    pub max_consecutive_conversion_errors: u64,
    pub allocated_buffers: usize,
    pub allocated_bytes: usize,
    pub retained_buffers: usize,
    pub conversion_total_ns: u64,
    pub conversion_max_ns: u64,
    /// Quantiles of at most the latest 256 published-frame conversion durations.
    pub conversion_p50_ns: u64,
    pub conversion_p95_ns: u64,
}
#[derive(Clone, Default)]
struct Snapshot {
    session: u64,
    active: bool,
    recovering: bool,
    reconnect_enabled: bool,
    latest: Option<Frame>,
}
struct Publication {
    session: u64,
    layout: FrameLayout,
    timestamp: Option<SourceTimestamp>,
    source_sequence: Option<u64>,
    captured_at: Instant,
    elapsed: u64,
}
struct Slot {
    data: Option<Storage>,
    bytes: usize,
}
struct Pool {
    slots: Vec<Slot>,
    budget: MemoryBudget,
    metrics: FrameMetrics,
    started: Option<Instant>,
    stopped: Option<Instant>,
    last_source_activity: Option<Instant>,
    recent: VecDeque<Instant>,
    conversion_samples: VecDeque<u64>,
    last_error_report: Option<Instant>,
    suppressed_error_reports: u64,
}
struct Subscriber {
    queue: Mutex<VecDeque<Frame>>,
    delivery: DeliveryPolicy,
    dropped: AtomicU64,
    max_rate: Option<FrameRate>,
    last_enqueued: Mutex<Option<Instant>>,
    owner: std::sync::OnceLock<Arc<AtomicBool>>,
}
/// Internal publisher. Public applications receive FrameReceiver instead.
#[derive(Clone)]
pub struct FrameHub {
    state: watch::Sender<Snapshot>,
    pool: Arc<Mutex<Pool>>,
    subscribers: Arc<Mutex<Vec<Weak<Subscriber>>>>,
    native_output: Arc<AtomicBool>,
    events: Arc<Mutex<Option<tokio::sync::broadcast::Sender<crate::SessionEvent>>>>,
    recovery_wake: watch::Sender<u64>,
}
impl Default for FrameHub {
    fn default() -> Self {
        Self::new(6)
    }
}
impl FrameHub {
    pub fn new(capacity: usize) -> Self {
        let (state, _) = watch::channel(Snapshot::default());
        let (recovery_wake, _) = watch::channel(0);
        Self {
            state,
            pool: Arc::new(Mutex::new(Pool {
                slots: vec![],
                budget: MemoryBudget {
                    buffers: capacity.max(2),
                    ..Default::default()
                },
                metrics: FrameMetrics::default(),
                started: None,
                stopped: None,
                last_source_activity: None,
                recent: VecDeque::new(),
                conversion_samples: VecDeque::with_capacity(256),
                last_error_report: None,
                suppressed_error_reports: 0,
            })),
            subscribers: Arc::new(Mutex::new(vec![])),
            native_output: Arc::new(AtomicBool::new(false)),
            events: Arc::new(Mutex::new(None)),
            recovery_wake,
        }
    }
    pub(crate) fn configure(&self, r: &StreamRequest) -> CameraResult<()> {
        if self.is_streaming() {
            return Err(CameraError::InvalidState(
                "Stop before configuring buffers".into(),
            ));
        }
        let mut pool = self.pool.lock();
        if pool.slots.iter().any(|s| s.data.is_none() && s.bytes > 0) {
            return Err(CameraError::InvalidState(
                "Frame conversion is still finishing".into(),
            ));
        }
        pool.slots
            .retain_mut(|s| s.data.as_mut().is_none_or(|d| !d.unique()));
        if pool.slots.len() > r.memory.buffers
            || pool.slots.iter().map(|s| s.bytes).sum::<usize>() > r.memory.bytes
        {
            return Err(CameraError::InvalidConfig(
                "Retained frames exceed the new budget".into(),
            ));
        }
        pool.budget = r.memory;
        self.native_output
            .store(r.output == OutputFormat::Native, Ordering::Release);
        Ok(())
    }
    pub fn start(&self) -> u64 {
        let mut pool = self.pool.lock();
        pool.metrics = FrameMetrics::default();
        pool.started = Some(Instant::now());
        pool.stopped = None;
        pool.last_source_activity = Some(Instant::now());
        pool.recent.clear();
        pool.conversion_samples.clear();
        pool.last_error_report = None;
        pool.suppressed_error_reports = 0;
        let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        self.clear_queues();
        let current = self.state.borrow();
        let recovering = current.recovering;
        let reconnect_enabled = current.reconnect_enabled;
        drop(current);
        self.state.send_replace(Snapshot {
            session,
            active: true,
            recovering,
            reconnect_enabled,
            latest: None,
        });
        session
    }
    pub(crate) fn set_event_sender(
        &self,
        sender: tokio::sync::broadcast::Sender<crate::SessionEvent>,
    ) {
        *self.events.lock() = Some(sender);
    }

    fn report_resource_pressure(&self, dropped_frames: u64) {
        if let Some(sender) = self.events.lock().as_ref() {
            let _ = sender.send(crate::SessionEvent::ResourcePressure { dropped_frames });
        }
    }
    fn clear_queues(&self) {
        self.subscribers.lock().retain(|s| {
            if let Some(s) = s.upgrade() {
                s.queue.lock().clear();
                true
            } else {
                false
            }
        });
    }
    pub(crate) fn begin_recovery(&self) {
        self.state.send_modify(|s| s.recovering = true);
    }
    pub(crate) fn end_recovery(&self) {
        self.state.send_modify(|s| s.recovering = false);
    }
    pub(crate) fn enable_recovery(&self) {
        self.state.send_modify(|s| s.reconnect_enabled = true);
    }
    pub(crate) fn disable_recovery(&self) {
        self.state.send_modify(|s| {
            s.reconnect_enabled = false;
            s.recovering = false;
        });
    }
    pub fn stop(&self) {
        self.pool.lock().stopped = Some(Instant::now());
        self.clear_queues();
        self.state.send_modify(|s| {
            s.active = false;
            s.latest = None;
        });
        self.wake_recovery();
    }
    pub fn session(&self) -> u64 {
        self.state.borrow().session
    }
    pub fn is_streaming(&self) -> bool {
        self.state.borrow().active
    }
    pub fn latest(&self) -> Option<Frame> {
        self.state.borrow().latest.clone()
    }
    #[cfg(test)]
    pub(crate) fn source_activity_is_stale(&self, timeout: Duration) -> bool {
        self.pool
            .lock()
            .last_source_activity
            .is_none_or(|activity| activity.elapsed() > timeout)
    }
    fn wake_recovery(&self) {
        self.recovery_wake
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    pub(crate) fn recovery_trigger(&self, stall_timeout: Duration) -> Option<RecoveryTrigger> {
        if !self.state.borrow().active {
            return Some(RecoveryTrigger::SourceStopped);
        }
        let pool = self.pool.lock();
        if pool.metrics.consecutive_conversion_errors >= FRAME_ERROR_RECOVERY_THRESHOLD {
            return Some(RecoveryTrigger::FrameErrors {
                consecutive: pool.metrics.consecutive_conversion_errors,
            });
        }
        pool.last_source_activity
            .is_none_or(|activity| activity.elapsed() >= stall_timeout)
            .then_some(RecoveryTrigger::SourceStalled)
    }
    fn source_stall_remaining(&self, timeout: Duration) -> Duration {
        self.pool
            .lock()
            .last_source_activity
            .map_or(Duration::ZERO, |activity| {
                timeout.saturating_sub(activity.elapsed())
            })
    }
    pub(crate) async fn wait_for_recovery_trigger(
        &self,
        stall_timeout: Duration,
    ) -> RecoveryTrigger {
        let mut wake = self.recovery_wake.subscribe();
        loop {
            if let Some(trigger) = self.recovery_trigger(stall_timeout) {
                return trigger;
            }
            let remaining = self.source_stall_remaining(stall_timeout);
            tokio::select! {
                result = wake.changed() => {
                    if result.is_err() {
                        return RecoveryTrigger::SourceStopped;
                    }
                }
                _ = tokio::time::sleep(remaining) => {}
            }
        }
    }
    fn record_conversion_error(&self, session: u64, source_activity: bool) {
        {
            let state = self.state.borrow();
            if !state.active || state.session != session {
                return;
            }
        }
        let notify = {
            let mut pool = self.pool.lock();
            if source_activity {
                pool.metrics.received += 1;
                pool.last_source_activity = Some(Instant::now());
            }
            pool.metrics.conversion_errors += 1;
            pool.metrics.consecutive_conversion_errors += 1;
            pool.metrics.max_consecutive_conversion_errors = pool
                .metrics
                .max_consecutive_conversion_errors
                .max(pool.metrics.consecutive_conversion_errors);
            pool.metrics.consecutive_conversion_errors == FRAME_ERROR_RECOVERY_THRESHOLD
        };
        if notify {
            self.wake_recovery();
        }
    }
    #[allow(dead_code)]
    pub(crate) fn record_input_error(&self, session: u64) {
        self.record_conversion_error(session, true);
    }
    #[allow(dead_code)]
    pub(crate) fn log_frame_error(&self, backend: &str, error: &CameraError) {
        let now = Instant::now();
        let report = {
            let mut pool = self.pool.lock();
            if pool.last_error_report.is_some_and(|previous| {
                now.saturating_duration_since(previous) < Duration::from_secs(1)
            }) {
                pool.suppressed_error_reports += 1;
                None
            } else {
                let suppressed = std::mem::take(&mut pool.suppressed_error_reports);
                pool.last_error_report = Some(now);
                Some(suppressed)
            }
        };
        if let Some(suppressed) = report {
            if suppressed == 0 {
                log::warn!("{backend} frame rejected: {error}");
            } else {
                log::warn!(
                    "{backend} frame rejected: {error} ({suppressed} similar errors suppressed)"
                );
            }
        }
    }
    pub(crate) fn wants_native(&self) -> bool {
        self.native_output.load(Ordering::Acquire)
    }
    #[allow(dead_code)]
    pub(crate) fn record_input_drop(&self, session: u64) {
        if self.session() == session {
            let mut pool = self.pool.lock();
            pool.last_source_activity = Some(Instant::now());
            pool.metrics.received += 1;
            pool.metrics.pool_drops += 1;
            let dropped = pool.metrics.pool_drops;
            drop(pool);
            self.report_resource_pressure(dropped);
        }
    }
    #[doc(hidden)]
    pub fn subscribe_with(&self, options: SubscriptionOptions) -> CameraResult<FrameReceiver> {
        let budget = self.pool.lock().budget;
        options.validate(budget)?;
        let subscriber = Arc::new(Subscriber {
            queue: Mutex::new(VecDeque::new()),
            delivery: options.delivery,
            dropped: AtomicU64::new(0),
            max_rate: options.max_rate,
            last_enqueued: Mutex::new(None),
            owner: std::sync::OnceLock::new(),
        });
        let state = self.state.subscribe();
        {
            let snapshot = state.borrow();
            let mut subs = self.subscribers.lock();
            if let Some(f) = snapshot.latest.clone() {
                *subscriber.last_enqueued.lock() = Some(f.captured_at);
                subscriber.queue.lock().push_back(f);
            }
            subs.push(Arc::downgrade(&subscriber));
        }
        Ok(FrameReceiver {
            state,
            subscriber,
            last: None,
        })
    }
    pub async fn wait_after(
        &self,
        after: Option<FrameKey>,
        timeout: Duration,
    ) -> CameraResult<Frame> {
        let mut rx = self.subscribe_with(SubscriptionOptions::latest())?;
        rx.last = after.or_else(|| rx.state.borrow().latest.as_ref().map(|frame| frame.key));
        rx.next_timeout(timeout).await
    }

    pub(crate) async fn wait_first_in(
        &self,
        session: u64,
        timeout: Duration,
    ) -> CameraResult<Frame> {
        if self.session() != session {
            return Err(CameraError::StreamStopped);
        }
        let mut rx = self.subscribe_with(SubscriptionOptions::latest())?;
        let frame = rx.next_timeout(timeout).await?;
        if frame.key.session != session {
            return Err(CameraError::StreamStopped);
        }
        Ok(frame)
    }
    pub fn publish_rgb(
        &self,
        session: u64,
        width: u32,
        height: u32,
        source_timestamp_ns: Option<i64>,
        convert: impl FnOnce(&mut [u8]) -> CameraResult<()>,
    ) -> CameraResult<bool> {
        self.publish(
            session,
            FrameLayout::rgb(width, height),
            source_timestamp_ns.map(|nanoseconds| SourceTimestamp {
                nanoseconds,
                clock: ClockDomain::Unknown,
            }),
            None,
            true,
            convert,
        )
    }
    #[allow(dead_code)]
    pub(crate) fn publish_rgb_metadata(
        &self,
        session: u64,
        width: u32,
        height: u32,
        timestamp: Option<SourceTimestamp>,
        sequence: Option<u64>,
        convert: impl FnOnce(&mut [u8]) -> CameraResult<()>,
    ) -> CameraResult<bool> {
        self.publish(
            session,
            FrameLayout::rgb(width, height),
            timestamp,
            sequence,
            true,
            convert,
        )
    }
    pub(crate) fn publish_native_into(
        &self,
        session: u64,
        layout: FrameLayout,
        timestamp: Option<SourceTimestamp>,
        convert: impl FnOnce(&mut [u8]) -> CameraResult<()>,
    ) -> CameraResult<bool> {
        let length = layout
            .planes
            .iter()
            .try_fold(0, |n: usize, p| {
                p.offset.checked_add(p.length).map(|end| n.max(end))
            })
            .ok_or_else(|| CameraError::InvalidFormat("Native plane extent overflow".into()))?;
        layout.validate(length)?;
        self.publish(session, layout, timestamp, None, false, convert)
    }
    pub(crate) fn publish_native(
        &self,
        session: u64,
        layout: FrameLayout,
        timestamp: Option<SourceTimestamp>,
        sequence: Option<u64>,
        parts: &[&[u8]],
    ) -> CameraResult<bool> {
        let len = parts
            .iter()
            .try_fold(0usize, |n, p| n.checked_add(p.len()))
            .ok_or_else(|| CameraError::InvalidFormat("Native size overflow".into()))?;
        layout.validate(len)?;
        let end = layout
            .planes
            .iter()
            .map(|p| p.offset + p.length)
            .max()
            .unwrap_or(0);
        if end != len {
            return Err(CameraError::InvalidFormat(
                "Native payload contains undescribed trailing data".into(),
            ));
        }
        self.publish(session, layout, timestamp, sequence, false, |out| {
            let mut offset = 0;
            for p in parts {
                out[offset..offset + p.len()].copy_from_slice(p);
                offset += p.len();
            }
            Ok(())
        })
    }

    pub(crate) fn writable_native(
        &self,
        session: u64,
        layout: FrameLayout,
    ) -> CameraResult<NativeWriteLease> {
        let length = Self::frame_length(&layout, false)?;
        layout.validate(length)?;
        let lease = self
            .reserve(session, &layout, false)?
            .ok_or(CameraError::BufferEmpty)?;
        Ok(NativeWriteLease {
            lease: Some(lease),
            session,
            layout,
            timestamp: None,
            captured_at: Instant::now(),
        })
    }

    fn frame_length(layout: &FrameLayout, rgb: bool) -> CameraResult<usize> {
        let length = if rgb {
            (layout.width as usize)
                .checked_mul(layout.height as usize)
                .and_then(|n| n.checked_mul(3))
        } else {
            layout
                .planes
                .iter()
                .filter_map(|plane| plane.offset.checked_add(plane.length))
                .max()
        };
        length
            .filter(|&length| length > 0 && length <= MAX_FRAME_BYTES)
            .ok_or_else(|| CameraError::InvalidFormat("Frame exceeds size limit".into()))
    }

    fn reserve(
        &self,
        session: u64,
        layout: &FrameLayout,
        rgb: bool,
    ) -> CameraResult<Option<Lease>> {
        {
            let state = self.state.borrow();
            if !state.active || state.session != session {
                return Ok(None);
            }
        }
        let length = Self::frame_length(layout, rgb)?;
        let mut pool = self.pool.lock();
        {
            let state = self.state.borrow();
            if !state.active || state.session != session {
                return Ok(None);
            }
        }
        pool.metrics.received += 1;
        pool.last_source_activity = Some(Instant::now());
        let index = pool
            .slots
            .iter_mut()
            .position(|slot| slot.data.as_mut().is_some_and(Storage::unique));
        let index = match index {
            Some(index) => index,
            None if pool.slots.len() < pool.budget.buffers => {
                pool.slots.push(Slot {
                    data: None,
                    bytes: 0,
                });
                pool.slots.len() - 1
            }
            _ => {
                pool.metrics.pool_drops += 1;
                let dropped = pool.metrics.pool_drops;
                drop(pool);
                self.report_resource_pressure(dropped);
                return Ok(None);
            }
        };
        let used: usize = pool.slots.iter().map(|slot| slot.bytes).sum();
        if length
            > pool
                .budget
                .bytes
                .saturating_sub(used - pool.slots[index].bytes)
        {
            if pool.slots[index].bytes == 0 {
                pool.slots.pop();
            }
            pool.metrics.pool_drops += 1;
            let dropped = pool.metrics.pool_drops;
            drop(pool);
            self.report_resource_pressure(dropped);
            return Ok(None);
        }
        let available = pool
            .budget
            .bytes
            .saturating_sub(used - pool.slots[index].bytes);
        let existing = pool.slots[index].data.take();
        let storage = match existing {
            Some(Storage::Rgb(pixels))
                if rgb && pixels.dim() == (layout.height as usize, layout.width as usize, 3) =>
            {
                Storage::Rgb(pixels)
            }
            Some(Storage::Bytes(mut bytes)) if !rgb => {
                let buffer = Arc::get_mut(&mut bytes).expect("exclusive native slot");
                if length > buffer.capacity() {
                    bytes = Arc::new(vec![0; length]);
                } else {
                    buffer.resize(length, 0);
                }
                Storage::Bytes(bytes)
            }
            _ if rgb => Storage::Rgb(Arc::new(Pixels::zeros((
                layout.height as usize,
                layout.width as usize,
                3,
            )))),
            _ => Storage::Bytes(Arc::new(vec![0; length])),
        };
        let allocated = match &storage {
            Storage::Bytes(bytes) => bytes.capacity(),
            Storage::Rgb(_) => length,
        };
        if allocated > available {
            pool.slots[index].data = Some(Storage::Bytes(Arc::new(Vec::new())));
            pool.slots[index].bytes = 0;
            pool.metrics.pool_drops += 1;
            let dropped = pool.metrics.pool_drops;
            drop(pool);
            self.report_resource_pressure(dropped);
            return Ok(None);
        }
        pool.slots[index].bytes = allocated;
        drop(pool);
        Ok(Some(Lease {
            hub: self.clone(),
            index,
            data: Some(storage),
        }))
    }

    fn publish(
        &self,
        session: u64,
        layout: FrameLayout,
        timestamp: Option<SourceTimestamp>,
        source_sequence: Option<u64>,
        rgb: bool,
        convert: impl FnOnce(&mut [u8]) -> CameraResult<()>,
    ) -> CameraResult<bool> {
        let now = Instant::now();
        let Some(mut lease) = self.reserve(session, &layout, rgb)? else {
            return Ok(false);
        };
        let begin = Instant::now();
        let result = convert(lease.data.as_mut().unwrap().bytes_mut());
        let elapsed = begin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        if let Err(error) = result {
            self.record_conversion_error(session, false);
            return Err(error);
        }
        self.finish_publish(
            Publication {
                session,
                layout,
                timestamp,
                source_sequence,
                captured_at: now,
                elapsed,
            },
            &lease,
        )
    }

    fn finish_publish(&self, publication: Publication, lease: &Lease) -> CameraResult<bool> {
        let Publication {
            session,
            layout,
            timestamp,
            source_sequence,
            captured_at: now,
            elapsed,
        } = publication;
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        let mut published = false;
        self.state.send_if_modified(|state| {
            if !state.active || state.session != session {
                return false;
            }
            let sequence = state.latest.as_ref().map_or(1, |f| f.key.sequence + 1);
            let frame = Arc::new(CapturedFrame {
                key: FrameKey { session, sequence },
                storage: lease.data.as_ref().unwrap().clone(),
                layout,
                captured_at: now,
                timestamp_ns,
                source_timestamp_ns: timestamp.map(|t| t.nanoseconds),
                source_clock: timestamp.map_or(ClockDomain::Unknown, |t| t.clock),
                source_sequence,
            });
            self.subscribers.lock().retain(|subscriber| {
                let Some(subscriber) = subscriber.upgrade() else {
                    return false;
                };
                let mut queue = subscriber.queue.lock();
                if subscriber
                    .owner
                    .get()
                    .is_some_and(|f| f.load(Ordering::Acquire))
                {
                    queue.clear();
                    return false;
                }
                if let Some(rate) = subscriber.max_rate {
                    let mut last = subscriber.last_enqueued.lock();
                    if last.is_some_and(|previous| {
                        frame.captured_at.saturating_duration_since(previous) < rate.interval()
                    }) {
                        subscriber.dropped.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                    *last = Some(frame.captured_at);
                }
                match subscriber.delivery {
                    DeliveryPolicy::Latest => {
                        subscriber
                            .dropped
                            .fetch_add(queue.len() as u64, Ordering::Relaxed);
                        queue.clear();
                        queue.push_back(frame.clone());
                    }
                    DeliveryPolicy::Buffered { capacity, overflow } => {
                        if queue.len() >= capacity {
                            subscriber.dropped.fetch_add(1, Ordering::Relaxed);
                            if overflow == OverflowPolicy::DropNewest {
                                return true;
                            }
                            queue.pop_front();
                        }
                        queue.push_back(frame.clone());
                    }
                }
                true
            });
            state.latest = Some(frame);
            published = true;
            true
        });
        if published {
            let mut pool = self.pool.lock();
            if self.session() != session {
                return Ok(true);
            }
            pool.metrics.published += 1;
            pool.metrics.consecutive_conversion_errors = 0;
            pool.metrics.conversion_total_ns =
                pool.metrics.conversion_total_ns.saturating_add(elapsed);
            pool.metrics.conversion_max_ns = pool.metrics.conversion_max_ns.max(elapsed);
            if pool.conversion_samples.len() == 256 {
                pool.conversion_samples.pop_front();
            }
            pool.conversion_samples.push_back(elapsed);
            pool.recent.push_back(now);
            while pool
                .recent
                .front()
                .is_some_and(|t| now.saturating_duration_since(*t) > Duration::from_secs(1))
            {
                pool.recent.pop_front();
            }
        }
        Ok(published)
    }
    pub fn metrics(&self) -> FrameMetrics {
        let (mut samples, allocated_buffers, allocated_bytes, retained_buffers, metrics) = {
            let p = self.pool.lock();
            (
                p.conversion_samples.iter().copied().collect::<Vec<_>>(),
                p.slots.len(),
                p.slots.iter().map(|s| s.bytes).sum(),
                p.slots
                    .iter()
                    .filter(|s| s.data.as_ref().is_none_or(Storage::retained))
                    .count(),
                p.metrics.clone(),
            )
        };
        samples.sort_unstable();
        let quantile = |percent: usize| {
            samples
                .get((samples.len() * percent).div_ceil(100).saturating_sub(1))
                .copied()
                .unwrap_or(0)
        };
        FrameMetrics {
            conversion_p50_ns: quantile(50),
            conversion_p95_ns: quantile(95),
            allocated_buffers,
            allocated_bytes,
            retained_buffers,
            ..metrics
        }
    }
    pub fn stats(&self) -> StreamStats {
        let p = self.pool.lock();
        StreamStats {
            total_frames: p.metrics.received,
            dropped_frames: p.metrics.pool_drops + p.metrics.conversion_errors,
            current_fps: if self.is_streaming() {
                p.recent
                    .iter()
                    .filter(|t| t.elapsed() < Duration::from_secs(1))
                    .count() as f64
            } else {
                0.0
            },
            buffer_size: p.budget.buffers,
            buffered_frames: usize::from(self.latest().is_some()),
            uptime_seconds: p.started.map_or(0, |t| {
                p.stopped
                    .unwrap_or_else(Instant::now)
                    .saturating_duration_since(t)
                    .as_secs()
            }),
        }
    }
}
pub(crate) struct NativeWriteLease {
    lease: Option<Lease>,
    session: u64,
    layout: FrameLayout,
    timestamp: Option<SourceTimestamp>,
    captured_at: Instant,
}

impl NativeWriteLease {
    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        self.lease
            .as_mut()
            .expect("lease is present until commit")
            .data
            .as_mut()
            .expect("reserved storage is present")
            .bytes_mut()
    }

    pub(crate) fn set_timestamp(&mut self, timestamp: SourceTimestamp) {
        self.timestamp = Some(timestamp);
    }

    pub(crate) fn commit(mut self) -> CameraResult<bool> {
        let lease = self.lease.take().expect("lease is present until commit");
        let hub = lease.hub.clone();
        hub.finish_publish(
            Publication {
                session: self.session,
                layout: self.layout,
                timestamp: self.timestamp,
                source_sequence: None,
                captured_at: self.captured_at,
                elapsed: 0,
            },
            &lease,
        )
    }
}

struct Lease {
    hub: FrameHub,
    index: usize,
    data: Option<Storage>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.hub.pool.lock().slots[self.index].data = self.data.take();
    }
}
/// Each receiver owns its cursor/queue. Dropping next() before it resolves does
/// not consume a frame. Stop wakes all receivers with StreamStopped.
pub struct FrameReceiver {
    state: watch::Receiver<Snapshot>,
    subscriber: Arc<Subscriber>,
    last: Option<FrameKey>,
}
impl std::fmt::Debug for FrameReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameReceiver")
            .field("last", &self.last)
            .field("dropped", &self.dropped_frames())
            .finish()
    }
}
impl FrameReceiver {
    pub(crate) fn with_stop_flag(self, flag: Arc<AtomicBool>) -> Self {
        if flag.load(Ordering::Acquire) {
            self.subscriber.queue.lock().clear();
        }
        let _ = self.subscriber.owner.set(flag);
        self
    }
    pub fn last_key(&self) -> Option<FrameKey> {
        self.last
    }
    pub fn queued_frames(&self) -> usize {
        self.subscriber.queue.lock().len()
    }
    pub fn dropped_frames(&self) -> u64 {
        self.subscriber.dropped.load(Ordering::Relaxed)
    }
    pub async fn next(&mut self) -> CameraResult<Frame> {
        loop {
            {
                let state = self.state.borrow_and_update();
                if self
                    .subscriber
                    .owner
                    .get()
                    .is_some_and(|f| f.load(Ordering::Acquire))
                    || (!state.active && !state.recovering && !state.reconnect_enabled)
                {
                    return Err(CameraError::StreamStopped);
                }
                let mut q = self.subscriber.queue.lock();
                while let Some(f) = q.pop_front() {
                    if Some(f.key) != self.last && f.key.session == state.session {
                        self.last = Some(f.key);
                        return Ok(f);
                    }
                }
            }
            self.state
                .changed()
                .await
                .map_err(|_| CameraError::StreamStopped)?;
        }
    }
    pub async fn next_timeout(&mut self, timeout: Duration) -> CameraResult<Frame> {
        tokio::time::timeout(timeout, self.next())
            .await
            .map_err(|_| CameraError::Timeout {
                stage: crate::OperationStage::FrameWait,
            })?
    }
}
