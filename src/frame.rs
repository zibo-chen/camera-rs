//! Immutable frames, bounded storage, and independent cancellation-safe readers.
use crate::format::*;
use crate::{pixels::Pixels, CameraError, CameraResult, StreamStats};
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
    pub(crate) fn rgb_pixels(&self) -> CameraResult<&Arc<Pixels<u8>>> {
        match &self.storage {
            Storage::Rgb(p) => Ok(p),
            _ => Err(CameraError::UnsupportedFormat(
                "Frame is native; request RGB or convert explicitly".into(),
            )),
        }
    }
    #[cfg(feature = "ndarray")]
    pub fn ndarray(&self) -> CameraResult<ndarray::ArrayView3<'_, u8>> {
        Ok(self.rgb_pixels()?.view())
    }
    /// Clone the shared RGB allocation without copying pixels. Long-lived references
    /// occupy pool slots; copy explicitly if the application needs archival storage.
    #[cfg(feature = "ndarray")]
    pub fn shared_ndarray(&self) -> CameraResult<Arc<ndarray::Array3<u8>>> {
        Ok(self.rgb_pixels()?.clone())
    }
}
#[derive(Clone, Default, Debug)]
pub struct FrameMetrics {
    pub received: u64,
    pub published: u64,
    pub pool_drops: u64,
    pub conversion_errors: u64,
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
    latest: Option<Frame>,
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
    recent: VecDeque<Instant>,
    conversion_samples: VecDeque<u64>,
}
struct Subscriber {
    queue: Mutex<VecDeque<Frame>>,
    delivery: DeliveryPolicy,
    dropped: AtomicU64,
    owner: std::sync::OnceLock<Arc<AtomicBool>>,
}
/// Internal publisher. Public applications receive FrameReceiver instead.
#[derive(Clone)]
pub struct FrameHub {
    state: watch::Sender<Snapshot>,
    pool: Arc<Mutex<Pool>>,
    subscribers: Arc<Mutex<Vec<Weak<Subscriber>>>>,
    output: Arc<Mutex<OutputFormat>>,
    delivery: Arc<Mutex<DeliveryPolicy>>,
}
impl Default for FrameHub {
    fn default() -> Self {
        Self::new(6)
    }
}
impl FrameHub {
    pub fn new(capacity: usize) -> Self {
        let (state, _) = watch::channel(Snapshot::default());
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
                recent: VecDeque::new(),
                conversion_samples: VecDeque::with_capacity(256),
            })),
            subscribers: Arc::new(Mutex::new(vec![])),
            output: Arc::new(Mutex::new(OutputFormat::Rgb8)),
            delivery: Arc::new(Mutex::new(DeliveryPolicy::Latest)),
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
        *self.output.lock() = r.output;
        *self.delivery.lock() = r.delivery;
        Ok(())
    }
    pub fn start(&self) -> u64 {
        let mut pool = self.pool.lock();
        pool.metrics = FrameMetrics::default();
        pool.started = Some(Instant::now());
        pool.stopped = None;
        pool.recent.clear();
        pool.conversion_samples.clear();
        let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        self.clear_queues();
        let recovering = self.state.borrow().recovering;
        self.state.send_replace(Snapshot {
            session,
            active: true,
            recovering,
            latest: None,
        });
        session
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
    pub fn stop(&self) {
        self.pool.lock().stopped = Some(Instant::now());
        self.clear_queues();
        self.state.send_modify(|s| {
            s.active = false;
            s.latest = None;
        });
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
    pub(crate) fn wants_native(&self) -> bool {
        *self.output.lock() == OutputFormat::Native
    }
    pub fn subscribe(&self) -> FrameReceiver {
        self.subscribe_with(*self.delivery.lock())
    }
    pub(crate) fn subscribe_with(&self, delivery: DeliveryPolicy) -> FrameReceiver {
        let subscriber = Arc::new(Subscriber {
            queue: Mutex::new(VecDeque::new()),
            delivery,
            dropped: AtomicU64::new(0),
            owner: std::sync::OnceLock::new(),
        });
        let state = self.state.subscribe();
        {
            let snapshot = state.borrow();
            let mut subs = self.subscribers.lock();
            if let Some(f) = snapshot.latest.clone() {
                subscriber.queue.lock().push_back(f);
            }
            subs.push(Arc::downgrade(&subscriber));
        }
        FrameReceiver {
            state,
            subscriber,
            last: None,
        }
    }
    pub async fn wait_after(
        &self,
        after: Option<FrameKey>,
        timeout: Duration,
    ) -> CameraResult<Frame> {
        let mut rx = self.subscribe_with(DeliveryPolicy::Latest);
        rx.last = after;
        rx.next_timeout(timeout).await
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
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        {
            let state = self.state.borrow();
            if !state.active || state.session != session {
                return Ok(false);
            }
        }
        let length = if rgb {
            (layout.width as usize)
                .checked_mul(layout.height as usize)
                .and_then(|n| n.checked_mul(3))
        } else {
            layout
                .planes
                .iter()
                .filter_map(|p| p.offset.checked_add(p.length))
                .max()
        }
        .filter(|&n| n > 0 && n <= MAX_FRAME_BYTES)
        .ok_or_else(|| CameraError::InvalidFormat("Frame exceeds size limit".into()))?;
        let (index, storage) = {
            let mut pool = self.pool.lock();
            {
                let state = self.state.borrow();
                if !state.active || state.session != session {
                    return Ok(false);
                }
            }
            pool.metrics.received += 1;
            let index = pool
                .slots
                .iter_mut()
                .position(|s| s.data.as_mut().is_some_and(Storage::unique));
            let index = match index {
                Some(i) => i,
                None if pool.slots.len() < pool.budget.buffers => {
                    pool.slots.push(Slot {
                        data: None,
                        bytes: 0,
                    });
                    pool.slots.len() - 1
                }
                _ => {
                    pool.metrics.pool_drops += 1;
                    return Ok(false);
                }
            };
            let used: usize = pool.slots.iter().map(|s| s.bytes).sum();
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
                return Ok(false);
            }
            let existing = pool.slots[index].data.take();
            let storage = match existing {
                Some(Storage::Rgb(p))
                    if rgb && p.dim() == (layout.height as usize, layout.width as usize, 3) =>
                {
                    Storage::Rgb(p)
                }
                Some(Storage::Bytes(mut p)) if !rgb => {
                    let bytes = Arc::get_mut(&mut p).expect("exclusive native slot");
                    if length > bytes.capacity() {
                        bytes.reserve_exact(length - bytes.len());
                    }
                    bytes.resize(length, 0);
                    Storage::Bytes(p)
                }
                _ if rgb => Storage::Rgb(Arc::new(Pixels::zeros((
                    layout.height as usize,
                    layout.width as usize,
                    3,
                )))),
                _ => Storage::Bytes(Arc::new(vec![0; length])),
            };
            pool.slots[index].bytes = match &storage {
                Storage::Bytes(p) => p.capacity(),
                _ => length,
            };
            (index, storage)
        };
        // Lease returns reserved storage even when conversion panics. Conversion never
        // holds the pool mutex, so monitoring cannot stall behind JPEG/SIMD work.
        let mut lease = Lease {
            hub: self,
            index,
            data: Some(storage),
        };
        let begin = Instant::now();
        let result = convert(lease.data.as_mut().unwrap().bytes_mut());
        let elapsed = begin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let mut pool = self.pool.lock();
        let mut published = false;
        if let Err(error) = result {
            if self.session() == session {
                pool.metrics.conversion_errors += 1;
            }
            return Err(error);
        }
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
            pool.metrics.published += 1;
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
        drop(pool);
        Ok(published)
    }
    pub fn metrics(&self) -> FrameMetrics {
        let p = self.pool.lock();
        let mut samples: Vec<_> = p.conversion_samples.iter().copied().collect();
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
            allocated_buffers: p.slots.len(),
            allocated_bytes: p.slots.iter().map(|s| s.bytes).sum(),
            retained_buffers: p
                .slots
                .iter()
                .filter(|s| s.data.as_ref().is_none_or(Storage::retained))
                .count(),
            ..p.metrics.clone()
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
struct Lease<'a> {
    hub: &'a FrameHub,
    index: usize,
    data: Option<Storage>,
}
impl Drop for Lease<'_> {
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
                    || (!state.active && !state.recovering)
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
            .map_err(|_| CameraError::Timeout)?
    }
}
