//! Capture requests, negotiated formats, and frame memory contracts.
use crate::{CameraConfig, CameraError, CameraResult, VideoFormat};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameRate {
    numerator: u32,
    denominator: u32,
}
impl FrameRate {
    pub fn new(numerator: u32, denominator: u32) -> CameraResult<Self> {
        if numerator == 0 || denominator == 0 {
            return Err(CameraError::InvalidConfig(
                "Frame rate must be positive".into(),
            ));
        }
        let mut a = numerator;
        let mut b = denominator;
        while b != 0 {
            let r = a % b;
            a = b;
            b = r;
        }
        Ok(Self {
            numerator: numerator / a,
            denominator: denominator / a,
        })
    }
    pub const fn numerator(self) -> u32 {
        self.numerator
    }
    pub const fn denominator(self) -> u32 {
        self.denominator
    }
    pub fn as_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
    pub fn interval(self) -> Duration {
        Duration::from_secs_f64(1.0 / self.as_f64()).max(Duration::from_nanos(1))
    }
}
impl PartialOrd for FrameRate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for FrameRate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (u64::from(self.numerator) * u64::from(other.denominator))
            .cmp(&(u64::from(other.numerator) * u64::from(self.denominator)))
    }
}
impl Default for FrameRate {
    fn default() -> Self {
        Self {
            numerator: 30,
            denominator: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Rgb8,
    Native,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionPolicy {
    Exact,
    Closest,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowPolicy {
    DropOldest,
    DropNewest,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryPolicy {
    Latest,
    Buffered {
        capacity: usize,
        overflow: OverflowPolicy,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryBudget {
    pub buffers: usize,
    pub bytes: usize,
}
impl Default for MemoryBudget {
    fn default() -> Self {
        Self {
            buffers: 6,
            bytes: 192 * 1024 * 1024,
        }
    }
}
impl MemoryBudget {
    pub fn validate(self) -> CameraResult<()> {
        if !(2..=256).contains(&self.buffers) || self.bytes == 0 {
            return Err(CameraError::InvalidConfig(
                "Memory budget requires 2..=256 buffers and nonzero bytes".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct StreamRequest {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) fps: FrameRate,
    pub(crate) format: Option<VideoFormat>,
    pub(crate) output: OutputFormat,
    pub(crate) selection: SelectionPolicy,
    pub(crate) delivery: DeliveryPolicy,
    pub(crate) memory: MemoryBudget,
    pub(crate) startup_timeout: Duration,
    pub(crate) driver_buffers: Option<usize>,
    pub(crate) reconnect: Option<ReconnectPolicy>,
}
#[derive(Clone, Debug)]
pub struct ReconnectPolicy {
    pub max_attempts: Option<u32>,
    pub delay: Duration,
    pub stall_timeout: Duration,
}
impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: Some(10),
            delay: Duration::from_secs(1),
            stall_timeout: Duration::from_secs(5),
        }
    }
}
#[derive(Clone, Debug)]
pub struct StreamRequestBuilder(StreamRequest);
impl StreamRequest {
    pub fn builder() -> StreamRequestBuilder {
        StreamRequestBuilder(Self {
            width: 640,
            height: 480,
            fps: FrameRate::default(),
            format: None,
            output: OutputFormat::Rgb8,
            selection: SelectionPolicy::Closest,
            delivery: DeliveryPolicy::Latest,
            memory: MemoryBudget::default(),
            startup_timeout: Duration::from_secs(3),
            driver_buffers: None,
            reconnect: None,
        })
    }
    pub fn output(&self) -> OutputFormat {
        self.output
    }
    pub fn resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn frame_rate(&self) -> FrameRate {
        self.fps
    }
}
impl StreamRequestBuilder {
    pub fn resolution(mut self, w: u32, h: u32) -> Self {
        self.0.width = w;
        self.0.height = h;
        self
    }
    pub fn frame_rate(mut self, fps: FrameRate) -> Self {
        self.0.fps = fps;
        self
    }
    pub fn capture_format(mut self, format: VideoFormat) -> Self {
        self.0.format = Some(format);
        self
    }
    pub fn output(mut self, v: OutputFormat) -> Self {
        self.0.output = v;
        self
    }
    pub fn selection(mut self, v: SelectionPolicy) -> Self {
        self.0.selection = v;
        self
    }
    pub fn delivery(mut self, v: DeliveryPolicy) -> Self {
        self.0.delivery = v;
        self
    }
    pub fn memory_budget(mut self, v: MemoryBudget) -> Self {
        self.0.memory = v;
        self
    }
    pub fn driver_buffers(mut self, v: usize) -> Self {
        self.0.driver_buffers = Some(v);
        self
    }
    pub fn startup_timeout(mut self, v: Duration) -> Self {
        self.0.startup_timeout = v;
        self
    }
    pub fn reconnect(mut self, v: ReconnectPolicy) -> Self {
        self.0.reconnect = Some(v);
        self
    }
    pub fn build(self) -> CameraResult<StreamRequest> {
        let r = self.0;
        r.memory.validate()?;
        let rgb = (r.width as usize)
            .checked_mul(r.height as usize)
            .and_then(|n| n.checked_mul(3));
        if r.width == 0 || r.height == 0 || rgb.is_none() || r.startup_timeout.is_zero() {
            return Err(CameraError::InvalidConfig(
                "Invalid resolution/startup deadline".into(),
            ));
        }
        if r.output == OutputFormat::Rgb8 && rgb.unwrap() > r.memory.bytes / r.memory.buffers {
            return Err(CameraError::InvalidConfig(
                "RGB buffers exceed memory budget".into(),
            ));
        }
        if let DeliveryPolicy::Buffered { capacity, .. } = r.delivery {
            if capacity == 0 || capacity >= r.memory.buffers {
                return Err(CameraError::InvalidConfig(
                    "Queue depth must be positive and less than pool capacity".into(),
                ));
            }
        }
        if r.driver_buffers.is_some_and(|n| !(2..=32).contains(&n)) {
            return Err(CameraError::InvalidConfig(
                "Driver buffer count must be 2..=32".into(),
            ));
        }
        if r.reconnect.as_ref().is_some_and(|p| {
            p.delay.is_zero() || p.stall_timeout.is_zero() || p.max_attempts == Some(0)
        }) {
            return Err(CameraError::InvalidConfig(
                "Invalid reconnect policy".into(),
            ));
        }
        Ok(r)
    }
}
#[derive(Clone, Debug)]
pub struct NegotiatedConfig {
    pub capture: CameraConfig,
    pub frame_rate: FrameRate,
    pub output: OutputFormat,
    pub adjustments: Vec<String>,
    pub first_frame_latency: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb8,
    Bgr8,
    Bgra8,
    Rgba8,
    Argb8,
    Yuyv,
    Uyvy,
    Nv12,
    Nv21,
    Yuv420p,
    Gray8,
    Mjpeg,
    H264,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorMatrix {
    #[default]
    Unknown,
    Bt601,
    Bt709,
    Bt2020,
    Smpte240M,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorRange {
    #[default]
    Unknown,
    Full,
    Limited,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ColorInfo {
    pub matrix: ColorMatrix,
    pub range: ColorRange,
    pub primaries: ColorPrimaries,
    pub transfer: TransferFunction,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorPrimaries {
    #[default]
    Unknown,
    Bt709,
    Bt601_525,
    Bt601_625,
    Bt2020,
    DisplayP3,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TransferFunction {
    #[default]
    Unknown,
    Linear,
    Srgb,
    Bt709,
    Pq,
    Hlg,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClockDomain {
    #[default]
    Unknown,
    HostMonotonic,
    MediaPresentation,
    DeviceMonotonic,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTimestamp {
    pub nanoseconds: i64,
    pub clock: ClockDomain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Orientation {
    pub rotation_degrees: u16,
    pub mirrored: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneLayout {
    pub offset: usize,
    pub length: usize,
    pub row_stride: usize,
    pub pixel_stride: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameLayout {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub planes: Vec<PlaneLayout>,
    pub color: ColorInfo,
    pub orientation: Orientation,
    /// Whether row zero is the bottom image row (e.g. a Windows RGB DIB).
    pub bottom_up: bool,
}
impl FrameLayout {
    pub(crate) fn rgb(w: u32, h: u32) -> Self {
        Self {
            width: w,
            height: h,
            format: PixelFormat::Rgb8,
            planes: vec![PlaneLayout {
                offset: 0,
                length: (w as usize)
                    .checked_mul(h as usize)
                    .and_then(|n| n.checked_mul(3))
                    .unwrap_or(0),
                row_stride: (w as usize).checked_mul(3).unwrap_or(0),
                pixel_stride: 3,
            }],
            color: ColorInfo::default(),
            orientation: Orientation::default(),
            bottom_up: false,
        }
    }
    /// Build a single packed/encoded plane. Padding remains part of the payload.
    pub(crate) fn packed(
        w: u32,
        h: u32,
        format: PixelFormat,
        stride: usize,
        length: usize,
    ) -> Self {
        let pixel_stride = match format {
            PixelFormat::Rgb8 | PixelFormat::Bgr8 => 3,
            PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Argb8 => 4,
            PixelFormat::Yuyv | PixelFormat::Uyvy => 2,
            _ => 1,
        };
        Self {
            width: w,
            height: h,
            format,
            planes: vec![PlaneLayout {
                offset: 0,
                length,
                row_stride: stride,
                pixel_stride,
            }],
            color: ColorInfo::default(),
            orientation: Orientation::default(),
            bottom_up: false,
        }
    }
    pub fn validate(&self, length: usize) -> CameraResult<()> {
        let invalid =
            || CameraError::InvalidFormat("Invalid frame dimensions, planes, or strides".into());
        if self.width == 0
            || self.height == 0
            || ![0, 90, 180, 270].contains(&self.orientation.rotation_degrees)
        {
            return Err(invalid());
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let specs: Vec<(usize, usize, usize)> = match self.format {
            PixelFormat::Mjpeg | PixelFormat::H264 => vec![(1, 1, 1)],
            PixelFormat::Nv12 | PixelFormat::Nv21 => {
                vec![(w, h, 1), (w.div_ceil(2), h.div_ceil(2), 2)]
            }
            PixelFormat::Yuv420p => vec![
                (w, h, 1),
                (w.div_ceil(2), h.div_ceil(2), 1),
                (w.div_ceil(2), h.div_ceil(2), 1),
            ],
            PixelFormat::Rgb8 | PixelFormat::Bgr8 => vec![(w, h, 3)],
            PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Argb8 => vec![(w, h, 4)],
            PixelFormat::Yuyv | PixelFormat::Uyvy => {
                if w % 2 != 0 {
                    return Err(invalid());
                }
                vec![(w, h, 2)]
            }
            PixelFormat::Gray8 => vec![(w, h, 1)],
        };
        if self.planes.len() != specs.len() {
            return Err(invalid());
        }
        for (p, (cols, rows, channels)) in self.planes.iter().zip(specs) {
            if p.length == 0 || p.offset.checked_add(p.length).is_none_or(|n| n > length) {
                return Err(invalid());
            }
            if matches!(self.format, PixelFormat::Mjpeg | PixelFormat::H264) {
                continue;
            }
            let row = (cols - 1)
                .checked_mul(p.pixel_stride)
                .and_then(|n| n.checked_add(channels))
                .ok_or_else(invalid)?;
            let required = (rows - 1)
                .checked_mul(p.row_stride)
                .and_then(|n| n.checked_add(row))
                .ok_or_else(invalid)?;
            if (matches!(self.format, PixelFormat::Yuyv | PixelFormat::Uyvy) && p.pixel_stride != 2)
                || p.pixel_stride < channels
                || p.row_stride < row
                || p.length < required
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
