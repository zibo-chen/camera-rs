//! All COM objects live on one MTA worker. ReadSample is asynchronous: frame
//! access and timeouts never execute a blocking driver read on the caller.
use super::{
    com::MFGuard,
    control,
    convert::{self, guid_to_format},
    device::{activate_to_device_info, create_source_reader, query_activate_pointers},
};
use crate::pixels::Pixels as Array3;
use crate::traits::{CameraControl, CameraManager, StreamStats, StreamingCamera};
use crate::{
    CameraConfig, CameraControlRange, CameraControlType, CameraControlValue, CameraDeviceInfo,
    CameraError, CameraResult, FrameHub, VideoFormat,
};
use std::{
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};
#[cfg(feature = "decode-mjpeg")]
use turbojpeg::{Decompressor, Image, PixelFormat};
#[cfg(feature = "decode-mjpeg")]
type JpegDecoder = Decompressor;
#[cfg(not(feature = "decode-mjpeg"))]
struct JpegDecoder;
use windows::{
    core::{implement, Interface, HRESULT},
    Win32::Media::MediaFoundation::*,
};
const MEDIA_FOUNDATION_FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;
type Reply = mpsc::Sender<CameraResult<()>>;

enum Event {
    Call(Box<dyn FnOnce(&mut Worker) + Send>),
    Stop(Reply),
    Sample(HRESULT, u32, i64, Option<MtaSample>),
    Flushed,
    Quit,
}

// Private transport, exclusively between MF's MTA work queue and our MTA
// worker. No interface escapes to callers or crosses into an STA apartment.
// https://learn.microsoft.com/en-us/windows/win32/medfound/media-foundation-and-com
struct MtaSample(IMFSample);
unsafe impl Send for MtaSample {}

#[implement(IMFSourceReaderCallback)]
struct ReaderCallback {
    sender: mpsc::Sender<Event>,
}
impl IMFSourceReaderCallback_Impl for ReaderCallback_Impl {
    fn OnReadSample(
        &self,
        status: HRESULT,
        _: u32,
        flags: u32,
        timestamp: i64,
        sample: Option<&IMFSample>,
    ) -> windows::core::Result<()> {
        let _ = self.sender.send(Event::Sample(
            status,
            flags,
            timestamp,
            sample.cloned().map(MtaSample),
        ));
        Ok(())
    }
    fn OnFlush(&self, _: u32) -> windows::core::Result<()> {
        let _ = self.sender.send(Event::Flushed);
        Ok(())
    }
    fn OnEvent(&self, _: u32, _: Option<&IMFMediaEvent>) -> windows::core::Result<()> {
        Ok(())
    }
}

struct Worker {
    source_reader: IMFSourceReader,
    media_source: IMFMediaSource,
    hub: Arc<FrameHub>,
    config: Arc<Mutex<Option<CameraConfig>>>,
    decoder: Option<JpegDecoder>,
    compressed_scratch: Vec<u8>,
    session: u64,
    running: bool,
    stopping: Option<Reply>,
    stride: i32,
}

enum MediaBufferLock {
    Linear(IMFMediaBuffer),
    TwoDimensional(IMF2DBuffer),
}

impl Drop for MediaBufferLock {
    fn drop(&mut self) {
        unsafe {
            match self {
                Self::Linear(buffer) => {
                    let _ = buffer.Unlock();
                }
                Self::TwoDimensional(buffer) => {
                    let _ = buffer.Unlock2D();
                }
            }
        }
    }
}

fn two_dimensional_length(config: &CameraConfig, pitch: usize) -> CameraResult<usize> {
    let width = config.width as usize;
    let height = config.height as usize;
    let (rows, final_row_bytes) = match config.format {
        VideoFormat::NV12 => (height + height.div_ceil(2), width),
        VideoFormat::YUYV | VideoFormat::UYVY => (height, width.saturating_mul(2)),
        VideoFormat::RGB => (height, width.saturating_mul(3)),
        VideoFormat::Gray => (height, width),
        _ => {
            return Err(CameraError::unsupported_format(
                "MF 2D buffer format".into(),
            ))
        }
    };
    if rows == 0 || final_row_bytes == 0 || pitch < final_row_bytes {
        return Err(CameraError::invalid_frame("Invalid MF 2D pitch".into()));
    }
    (rows - 1)
        .checked_mul(pitch)
        .and_then(|bytes| bytes.checked_add(final_row_bytes))
        .ok_or_else(|| CameraError::invalid_frame("MF 2D buffer size overflow".into()))
}

unsafe fn lock_media_buffer(
    buffer: IMFMediaBuffer,
    config: &CameraConfig,
    fallback_stride: i32,
) -> CameraResult<(*mut u8, usize, i32, MediaBufferLock)> {
    if config.format != VideoFormat::MJPEG {
        if let Ok(two_dimensional) = buffer.cast::<IMF2DBuffer>() {
            let mut scanline = std::ptr::null_mut();
            let mut pitch = 0i32;
            if two_dimensional.Lock2D(&mut scanline, &mut pitch).is_ok() {
                let usable = !scanline.is_null()
                    && pitch != 0
                    && !(config.format == VideoFormat::NV12 && pitch < 0);
                if usable {
                    if let Ok(length) =
                        two_dimensional_length(config, pitch.unsigned_abs() as usize)
                    {
                        let pointer = if pitch < 0 {
                            scanline
                                .offset((config.height as isize - 1).saturating_mul(pitch as isize))
                        } else {
                            scanline
                        };
                        return Ok((
                            pointer,
                            length,
                            pitch,
                            MediaBufferLock::TwoDimensional(two_dimensional),
                        ));
                    }
                }
                let _ = two_dimensional.Unlock2D();
            }
        }
    }
    let mut pointer = std::ptr::null_mut();
    let mut length = 0u32;
    buffer
        .Lock(&mut pointer, None, Some(&mut length))
        .map_err(mf_error)?;
    Ok((
        pointer,
        length as usize,
        fallback_stride,
        MediaBufferLock::Linear(buffer),
    ))
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.fail();
        unsafe {
            let _ = self.media_source.Shutdown();
        }
    }
}

impl Worker {
    /// Returns the supported format list.
    fn get_compatible_formats(&self) -> CameraResult<Vec<CameraConfig>> {
        let mut formats = Vec::new();
        let mut index = 0u32;

        loop {
            let media_type = unsafe {
                match self
                    .source_reader
                    .GetNativeMediaType(MEDIA_FOUNDATION_FIRST_VIDEO_STREAM, index)
                {
                    Ok(mt) => mt,
                    Err(_) => break, // No more formats.
                }
            };
            index += 1;

            // Read the format.
            let fourcc = unsafe {
                match media_type.GetGUID(&MF_MT_SUBTYPE) {
                    Ok(guid) => guid,
                    Err(_) => continue,
                }
            };

            let format = match guid_to_format(fourcc) {
                Some(f) => f,
                None => continue,
            };

            // Read the resolution.
            let (width, height) = unsafe {
                match media_type.GetUINT64(&MF_MT_FRAME_SIZE) {
                    Ok(size) => {
                        let w = (size >> 32) as u32;
                        let h = size as u32;
                        (w, h)
                    }
                    Err(_) => continue,
                }
            };

            // Read the frame rate.
            let frame_rates = self.get_frame_rates(&media_type);

            for rate in frame_rates {
                formats.push(
                    CameraConfig::new(format, width, height, rate.numerator())
                        .with_frame_rate(rate),
                );
            }
        }

        Ok(formats)
    }

    /// Returns frame rates represented by a MediaType.
    fn get_frame_rates(&self, media_type: &IMFMediaType) -> Vec<crate::FrameRate> {
        let mut rates = Vec::new();
        for key in [
            &MF_MT_FRAME_RATE,
            &MF_MT_FRAME_RATE_RANGE_MAX,
            &MF_MT_FRAME_RATE_RANGE_MIN,
        ] {
            if let Ok(raw) = unsafe { media_type.GetUINT64(key) } {
                if let Ok(rate) = crate::FrameRate::new((raw >> 32) as u32, raw as u32) {
                    if !rates.contains(&rate) {
                        rates.push(rate);
                    }
                }
            }
        }
        rates
    }

    fn set_media_format(&self, config: &CameraConfig) -> CameraResult<()> {
        let target_guid = convert::format_to_guid(config.format).ok_or_else(|| {
            CameraError::unsupported_format(format!("Unsupported format: {:?}", config.format))
        })?;

        let mut index = 0u32;
        let mut last_error = None;

        loop {
            let media_type = unsafe {
                match self
                    .source_reader
                    .GetNativeMediaType(MEDIA_FOUNDATION_FIRST_VIDEO_STREAM, index)
                {
                    Ok(mt) => mt,
                    Err(_) => break,
                }
            };
            index += 1;

            // Check the format.
            let fourcc = unsafe {
                match media_type.GetGUID(&MF_MT_SUBTYPE) {
                    Ok(guid) => guid,
                    Err(_) => continue,
                }
            };

            if fourcc != target_guid {
                continue;
            }

            // Check the resolution.
            let (width, height) = unsafe {
                match media_type.GetUINT64(&MF_MT_FRAME_SIZE) {
                    Ok(size) => ((size >> 32) as u32, size as u32),
                    Err(_) => continue,
                }
            };

            if width != config.width || height != config.height {
                continue;
            }

            // Check the frame rate.
            let frame_rates = self.get_frame_rates(&media_type);
            if !frame_rates.contains(&config.frame_rate()?) {
                continue;
            }

            unsafe {
                media_type.SetUINT64(
                    &MF_MT_FRAME_RATE,
                    ((config.fps as u64) << 32) | config.fps_denominator as u64,
                )
            }
            .map_err(mf_error)?;
            // Apply the matching format.
            match unsafe {
                self.source_reader.SetCurrentMediaType(
                    MEDIA_FOUNDATION_FIRST_VIDEO_STREAM,
                    None,
                    &media_type,
                )
            } {
                Ok(_) => return Ok(()),
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            }
        }

        if let Some(e) = last_error {
            Err(CameraError::invalid_config(format!(
                "Failed to set format: {}",
                e
            )))
        } else {
            Err(CameraError::invalid_config(format!(
                "No matching format found for {:?}",
                config
            )))
        }
    }

    fn refresh_format(&mut self) -> CameraResult<()> {
        let media = unsafe {
            self.source_reader
                .GetCurrentMediaType(MEDIA_FOUNDATION_FIRST_VIDEO_STREAM)
        }
        .map_err(mf_error)?;
        let size = unsafe { media.GetUINT64(&MF_MT_FRAME_SIZE) }.map_err(mf_error)?;
        let format = guid_to_format(unsafe { media.GetGUID(&MF_MT_SUBTYPE) }.map_err(mf_error)?)
            .ok_or_else(|| CameraError::unsupported_format("MF subtype".into()))?;
        let width = (size >> 32) as u32;
        let height = size as u32;
        let rate = unsafe { media.GetUINT64(&MF_MT_FRAME_RATE) }.map_err(mf_error)?;
        let frame_rate = crate::FrameRate::new((rate >> 32) as u32, rate as u32)?;
        let pixel_bytes = match format {
            VideoFormat::YUYV | VideoFormat::UYVY => 2,
            VideoFormat::RGB => 3,
            _ => 1,
        };
        self.stride =
            unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.unwrap_or(width * pixel_bytes) as i32;
        *self.config.lock().unwrap() = Some(
            CameraConfig::new(format, width, height, frame_rate.numerator())
                .with_frame_rate(frame_rate),
        );
        Ok(())
    }
    fn request_sample(&self) -> CameraResult<()> {
        unsafe {
            self.source_reader.ReadSample(
                MEDIA_FOUNDATION_FIRST_VIDEO_STREAM,
                0,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(mf_error)
    }
    fn start(&mut self, config: CameraConfig) -> CameraResult<()> {
        if self.running || self.stopping.is_some() {
            return Err(CameraError::stream_error(
                "MF stream running or flushing".into(),
            ));
        }
        config.validate()?;
        self.set_media_format(&config)?;
        unsafe {
            self.source_reader
                .SetStreamSelection(MEDIA_FOUNDATION_FIRST_VIDEO_STREAM, true)
        }
        .map_err(mf_error)?;
        self.refresh_format()?;
        self.session = self.hub.start();
        self.running = true;
        if let Err(error) = self.request_sample() {
            self.fail();
            return Err(error);
        }
        Ok(())
    }
    fn fail(&mut self) {
        self.running = false;
        self.hub.stop();
    }
    fn process_sample(&mut self, sample: &IMFSample, timestamp: i64) -> CameraResult<()> {
        let config = self
            .config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| CameraError::stream_error("Missing MF format".into()))?;
        let buffer = if unsafe { sample.GetBufferCount() }.map_err(mf_error)? == 1 {
            unsafe { sample.GetBufferByIndex(0) }.map_err(mf_error)?
        } else {
            unsafe { sample.ConvertToContiguousBuffer() }.map_err(mf_error)?
        };
        let (pointer, length, stride, lock) =
            unsafe { lock_media_buffer(buffer, &config, self.stride) }?;
        if pointer.is_null() || length == 0 {
            return Err(CameraError::buffer_exhausted());
        }
        if !self.hub.wants_native() && config.format == VideoFormat::MJPEG {
            #[cfg(not(feature = "decode-mjpeg"))]
            return Err(CameraError::unsupported_format(
                "MJPEG decoding requires the decode-mjpeg feature".into(),
            ));
            #[cfg(feature = "decode-mjpeg")]
            {
                let data = unsafe { std::slice::from_raw_parts(pointer, length) };
                self.compressed_scratch.clear();
                self.compressed_scratch.extend_from_slice(data);
                drop(lock);
                let decoder = &mut self.decoder;
                return self
                    .hub
                    .publish_rgb_metadata(
                        self.session,
                        config.width,
                        config.height,
                        timestamp
                            .checked_mul(100)
                            .map(|nanoseconds| crate::SourceTimestamp {
                                nanoseconds,
                                clock: crate::ClockDomain::MediaPresentation,
                            }),
                        None,
                        |rgb| decode_into(&self.compressed_scratch, &config, stride, decoder, rgb),
                    )
                    .map(|_| ());
            }
        }
        // Always unlock, including failed conversion. Bytes stay borrowed while
        // the shared pool receives the converted pixels.
        let result = {
            let data = unsafe { std::slice::from_raw_parts(pointer, length) };
            let decoder = &mut self.decoder;
            if self.hub.wants_native() {
                let format = match config.format {
                    VideoFormat::MJPEG => crate::PixelFormat::Mjpeg,
                    VideoFormat::YUYV => crate::PixelFormat::Yuyv,
                    VideoFormat::UYVY => crate::PixelFormat::Uyvy,
                    VideoFormat::NV12 => crate::PixelFormat::Nv12,
                    VideoFormat::RGB => crate::PixelFormat::Bgr8,
                    VideoFormat::Gray => crate::PixelFormat::Gray8,
                    _ => return Err(CameraError::unsupported_format("MF native layout".into())),
                };
                let mut layout = crate::FrameLayout::packed(
                    config.width,
                    config.height,
                    format,
                    stride.unsigned_abs() as usize,
                    data.len(),
                );
                layout.bottom_up = stride < 0;
                if format == crate::PixelFormat::Nv12 {
                    let y_len = (stride.unsigned_abs() as usize)
                        .checked_mul(config.height as usize)
                        .filter(|&n| n < data.len())
                        .ok_or_else(|| CameraError::invalid_frame("Truncated MF NV12".into()))?;
                    layout.planes[0].length = y_len;
                    layout.planes.push(crate::PlaneLayout {
                        offset: y_len,
                        length: data.len() - y_len,
                        row_stride: stride.unsigned_abs() as usize,
                        pixel_stride: 2,
                    });
                }
                self.hub
                    .publish_native(
                        self.session,
                        layout,
                        timestamp
                            .checked_mul(100)
                            .map(|nanoseconds| crate::SourceTimestamp {
                                nanoseconds,
                                clock: crate::ClockDomain::MediaPresentation,
                            }),
                        None,
                        &[data],
                    )
                    .map(|_| ())
            } else {
                self.hub
                    .publish_rgb_metadata(
                        self.session,
                        config.width,
                        config.height,
                        timestamp
                            .checked_mul(100)
                            .map(|nanoseconds| crate::SourceTimestamp {
                                nanoseconds,
                                clock: crate::ClockDomain::MediaPresentation,
                            }),
                        None,
                        |rgb| decode_into(data, &config, stride, decoder, rgb),
                    )
                    .map(|_| ())
            }
        };
        drop(lock);
        result
    }
    fn run(&mut self, receiver: mpsc::Receiver<Event>) {
        while let Ok(event) = receiver.recv() {
            match event {
                Event::Call(call) => call(self),
                Event::Stop(reply) => {
                    self.fail();
                    if self.stopping.is_some() {
                        let _ = reply.send(Err(CameraError::stream_error(
                            "Flush already pending".into(),
                        )));
                        continue;
                    }
                    match unsafe {
                        self.source_reader
                            .Flush(MEDIA_FOUNDATION_FIRST_VIDEO_STREAM)
                    } {
                        Ok(()) => self.stopping = Some(reply),
                        Err(error) => {
                            let _ = reply.send(Err(mf_error(error)));
                        }
                    }
                }
                Event::Flushed => {
                    if let Some(reply) = self.stopping.take() {
                        let _ = reply.send(Ok(()));
                    }
                }
                Event::Sample(status, flags, timestamp, sample) if self.running => {
                    if status.is_err()
                        || flags
                            & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32
                            != 0
                    {
                        log::warn!("MF stream ended: {:?}, flags={}", status, flags);
                        self.fail();
                        continue;
                    }
                    if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                        if let Err(error) = self.refresh_format() {
                            log::warn!("MF format change: {}", error);
                            self.fail();
                            continue;
                        }
                    }
                    if let Some(sample) = sample {
                        if let Err(error) = self.process_sample(&sample.0, timestamp) {
                            self.hub.log_frame_error("Media Foundation", &error);
                        }
                    }
                    if let Err(error) = self.request_sample() {
                        log::warn!("MF read request: {}", error);
                        self.fail();
                    }
                }
                Event::Sample(..) => {}
                Event::Quit => break,
            }
        }
        self.fail();
        unsafe {
            let _ = self
                .source_reader
                .Flush(MEDIA_FOUNDATION_FIRST_VIDEO_STREAM);
        }
    }
}

fn mf_error(error: windows::core::Error) -> CameraError {
    CameraError::native(
        crate::CameraErrorKind::BackendFailure,
        crate::BackendId::MEDIA_FOUNDATION,
        crate::OperationStage::Worker,
        "capture worker".into(),
        error.code().0 as i64,
        error.to_string(),
    )
    .with_source(error)
}

fn decode_into(
    data: &[u8],
    config: &CameraConfig,
    stride: i32,
    _decoder: &mut Option<JpegDecoder>,
    rgb: &mut [u8],
) -> CameraResult<()> {
    let width = config.width as usize;
    let height = config.height as usize;
    if config.format == VideoFormat::MJPEG {
        #[cfg(not(feature = "decode-mjpeg"))]
        return Err(CameraError::unsupported_format(
            "MJPEG decoding requires the decode-mjpeg feature".into(),
        ));
        #[cfg(feature = "decode-mjpeg")]
        {
            return crate::mjpeg::decode_with(_decoder, data, |decoder, data| {
                let header = decoder
                    .read_header(data)
                    .map_err(|e| CameraError::invalid_frame(e.to_string()))?;
                if header.width != width || header.height != height {
                    return Err(CameraError::invalid_frame(
                        "MF MJPEG dimension mismatch".into(),
                    ));
                }
                decoder
                    .decompress(
                        data,
                        Image {
                            pixels: rgb,
                            width,
                            height,
                            pitch: width * 3,
                            format: PixelFormat::RGB,
                        },
                    )
                    .map_err(|e| CameraError::invalid_frame(e.to_string()))
            });
        }
    }
    if config.format == VideoFormat::NV12 {
        if stride <= 0 {
            return Err(CameraError::invalid_frame("Negative NV12 stride".into()));
        }
        let y_length = (stride as usize)
            .checked_mul(height)
            .filter(|n| *n <= data.len())
            .ok_or_else(|| CameraError::invalid_frame("Truncated MF NV12 frame".into()))?;
        return crate::utils::color_convert::yuv420sp_to_rgb_into(
            &data[..y_length],
            &data[y_length..],
            width,
            height,
            stride as usize,
            stride as usize,
            rgb,
        );
    }
    let pixel_bytes = match config.format {
        VideoFormat::YUYV | VideoFormat::UYVY => 2,
        VideoFormat::RGB => 3,
        VideoFormat::Gray => 1,
        _ => {
            return Err(CameraError::unsupported_format(format!(
                "{:?}",
                config.format
            )))
        }
    };
    let row_bytes = width * pixel_bytes;
    let pitch = stride.unsigned_abs() as usize;
    if pitch < row_bytes {
        return Err(CameraError::invalid_frame(
            "MF stride shorter than row".into(),
        ));
    }
    let converter = crate::utils::color_convert::ColorConverter::new();
    if stride > 0 && pitch == row_bytes {
        match config.format {
            VideoFormat::YUYV => {
                return converter.yuyv_to_rgb_into(data, config.width, config.height, rgb)
            }
            VideoFormat::UYVY => {
                return converter.uyvy_to_rgb_into(data, config.width, config.height, rgb)
            }
            _ => {}
        }
    }
    for (row, destination) in rgb.chunks_exact_mut(width * 3).enumerate() {
        let source_row = if stride < 0 { height - row - 1 } else { row };
        let offset = source_row
            .checked_mul(pitch)
            .ok_or_else(|| CameraError::invalid_frame("MF row overflow".into()))?;
        let source = data
            .get(offset..offset.saturating_add(row_bytes))
            .ok_or_else(|| CameraError::invalid_frame("Truncated MF row".into()))?;
        match config.format {
            VideoFormat::YUYV => {
                converter.yuyv_to_rgb_into(source, config.width, 1, destination)?
            }
            VideoFormat::UYVY => {
                converter.uyvy_to_rgb_into(source, config.width, 1, destination)?
            }
            VideoFormat::RGB => {
                for (src, dst) in source
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(destination.as_chunks_mut::<3>().0.iter_mut())
                {
                    dst.copy_from_slice(&[src[2], src[1], src[0]]);
                }
            }
            VideoFormat::Gray => {
                for (gray, dst) in source
                    .iter()
                    .zip(destination.as_chunks_mut::<3>().0.iter_mut())
                {
                    dst.fill(*gray);
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

pub struct MFCamera {
    device_info: CameraDeviceInfo,
    formats: Vec<CameraConfig>,
    sender: mpsc::Sender<Event>,
    worker: Mutex<Option<JoinHandle<()>>>,
    lifecycle: Mutex<()>,
    hub: Arc<FrameHub>,
    config: Arc<Mutex<Option<CameraConfig>>>,
}

impl MFCamera {
    pub fn new(device_index: u32) -> CameraResult<Self> {
        Self::new_with_hub(device_index, crate::FrameHub::default())
    }
    pub(crate) fn new_with_hub(device_index: u32, hub: crate::FrameHub) -> CameraResult<Self> {
        Self::new_selected(device_index, hub, None)
    }
    pub(crate) fn new_selected(
        device_index: u32,
        hub: crate::FrameHub,
        expected: Option<&str>,
    ) -> CameraResult<Self> {
        let expected = expected.map(str::to_owned);
        let (sender, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let hub = Arc::new(hub);
        let config = Arc::new(Mutex::new(None));
        let worker_hub = hub.clone();
        let worker_config = config.clone();
        let callback_sender = sender.clone();
        let worker = std::thread::Builder::new()
            .name("camera-mf".into())
            .spawn(move || {
                // Declared first: destroyed after all interfaces on this same thread.
                let guard = MFGuard::new();
                let _guard = match guard {
                    Ok(guard) => guard,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let create = || -> CameraResult<(Worker, CameraDeviceInfo, Vec<CameraConfig>)> {
                    let activates = query_activate_pointers()?;
                    let selected = if let Some(expected) = &expected {
                        let mut selected = None;
                        for (i, activate) in activates.iter().enumerate() {
                            if activate_to_device_info(i as u32, activate)?.unique_id() == *expected
                            {
                                if selected.is_some() {
                                    return Err(CameraError::ambiguous_device(expected.clone()));
                                }
                                selected = Some(activate);
                            }
                        }
                        selected
                    } else {
                        activates.get(device_index as usize)
                    };
                    let activate = selected.ok_or_else(|| {
                        CameraError::device_not_found(format!("Device index {}", device_index))
                    })?;
                    let info = activate_to_device_info(device_index, activate)?;
                    let callback: IMFSourceReaderCallback = ReaderCallback {
                        sender: callback_sender,
                    }
                    .into();
                    let (source_reader, media_source) = create_source_reader(activate, &callback)?;
                    let worker = Worker {
                        source_reader,
                        media_source,
                        hub: worker_hub,
                        config: worker_config,
                        decoder: None,
                        compressed_scratch: Vec::new(),
                        session: 0,
                        running: false,
                        stopping: None,
                        stride: 0,
                    };
                    let formats = worker.get_compatible_formats()?;
                    Ok((worker, info, formats))
                };
                match create() {
                    Ok((mut worker, info, formats)) => {
                        if ready_tx.send(Ok((info, formats))).is_ok() {
                            worker.run(receiver);
                        }
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|error| CameraError::io(crate::OperationStage::Worker, error))?;
        let ready = ready_rx
            .recv()
            .map_err(|_| CameraError::stream_error("MF worker initialization failed".into()))?;
        match ready {
            Ok((device_info, formats)) => Ok(Self {
                device_info,
                formats,
                sender,
                worker: Mutex::new(Some(worker)),
                lifecycle: Mutex::new(()),
                hub,
                config,
            }),
            Err(error) => {
                let _ = worker.join();
                Err(error)
            }
        }
    }
    pub fn device_info(&self) -> &CameraDeviceInfo {
        &self.device_info
    }
    pub fn get_compatible_formats(&self) -> CameraResult<Vec<CameraConfig>> {
        Ok(self.formats.clone())
    }
    fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Worker) -> CameraResult<T> + Send + 'static,
    ) -> CameraResult<T> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(Event::Call(Box::new(move |worker| {
                let _ = tx.send(f(worker));
            })))
            .map_err(|_| CameraError::stream_error("MF worker stopped".into()))?;
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| CameraError::timeout(crate::OperationStage::BackendCommand))?
    }
    fn start_sync(&self, config: CameraConfig) -> CameraResult<()> {
        let _lifecycle = self.lifecycle.lock().unwrap();
        if self.hub.is_streaming() {
            return Ok(());
        }
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let result = self.call(move |worker| {
            if worker_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err(CameraError::timeout(crate::OperationStage::Startup));
            }
            let result = worker.start(config);
            if worker_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                worker.fail();
                return Err(CameraError::timeout(crate::OperationStage::Startup));
            }
            result
        });
        if matches!(
            result,
            Err(ref error) if error.kind() == crate::CameraErrorKind::Timeout
        ) {
            cancelled.store(true, std::sync::atomic::Ordering::Release);
            self.hub.stop();
            // Complete native flushing even if the waiting caller timed out.
            let (reply, _) = mpsc::channel();
            let _ = self.sender.send(Event::Stop(reply));
        }
        result
    }
    fn stop_sync(&self) -> CameraResult<()> {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.hub.stop();
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(Event::Stop(tx))
            .map_err(|_| CameraError::stream_error("MF worker stopped".into()))?;
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| CameraError::timeout(crate::OperationStage::Close))?
    }
    pub fn cleanup(&self) {
        let _ = self.stop_sync();
    }
    pub async fn start_stream_arc(self: &Arc<Self>, config: CameraConfig) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || camera.start_sync(config))
            .await
            .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }
    pub async fn stop_stream_arc(self: &Arc<Self>) -> CameraResult<()> {
        let camera = self.clone();
        tokio::task::spawn_blocking(move || camera.stop_sync())
            .await
            .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?
    }
}
impl Drop for MFCamera {
    fn drop(&mut self) {
        self.hub.stop();
        let _ = self.sender.send(Event::Quit);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}
impl CameraManager for MFCamera {
    fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
        super::device::list_devices()
    }
    fn get_supported_configs(index: u32) -> CameraResult<Vec<CameraConfig>> {
        Self::new(index)?.get_compatible_formats()
    }
    fn is_config_supported(index: u32, config: &CameraConfig) -> CameraResult<bool> {
        Ok(Self::get_supported_configs(index)?.iter().any(|c| {
            c.width == config.width
                && c.height == config.height
                && c.frame_rate().ok() == config.frame_rate().ok()
                && c.format == config.format
        }))
    }
}
impl StreamingCamera for MFCamera {
    fn frame_hub(&self) -> Option<&FrameHub> {
        Some(&self.hub)
    }
    async fn start_stream(&self, config: CameraConfig) -> CameraResult<()> {
        self.start_sync(config)
    }
    async fn stop_stream(&self) -> CameraResult<()> {
        self.stop_sync()
    }
    fn get_latest_frame(&self) -> CameraResult<Option<Array3<u8>>> {
        Ok(self.hub.latest().map(|f| {
            f.rgb_pixels()
                .expect("RGB backend adapter")
                .as_ref()
                .clone()
        }))
    }
    async fn wait_for_frame(&self, timeout: Duration) -> CameraResult<Array3<u8>> {
        Ok((*self.hub.wait_after(None, timeout).await?.rgb_pixels()?)
            .as_ref()
            .clone())
    }
    fn is_streaming(&self) -> bool {
        self.hub.is_streaming()
    }
    fn get_config(&self) -> Option<CameraConfig> {
        self.config.lock().unwrap().clone()
    }
    fn get_stats(&self) -> StreamStats {
        self.hub.stats()
    }
    async fn set_buffer_size(&self, _: usize) -> CameraResult<()> {
        Err(CameraError::invalid_config(
            "Fixed frame pool capacity".into(),
        ))
    }
}
impl CameraControl for MFCamera {
    fn get_control(&self, kind: CameraControlType) -> CameraResult<CameraControlValue> {
        self.call(move |w| control::get_control(&w.source_reader, kind))
    }
    fn set_control(&self, kind: CameraControlType, value: CameraControlValue) -> CameraResult<()> {
        self.call(move |w| control::set_control(&w.source_reader, kind, value))
    }
    fn get_control_range(&self, kind: CameraControlType) -> CameraResult<CameraControlRange> {
        self.call(move |w| control::get_control_range(&w.source_reader, kind))
    }
    fn supports_control(&self, kind: CameraControlType) -> bool {
        self.call(move |w| Ok(control::supports_control(&w.source_reader, kind)))
            .unwrap_or(false)
    }
    fn get_supported_controls(&self) -> Vec<CameraControlType> {
        self.call(|w| Ok(control::get_supported_controls(&w.source_reader)))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_padded_bottom_up_bgr_without_copying_raw_buffer() {
        let config = CameraConfig::new(VideoFormat::RGB, 1, 2, 30);
        let mut out = [0; 6];
        decode_into(&[3, 2, 1, 0, 6, 5, 4], &config, -4, &mut None, &mut out).unwrap();
        assert_eq!(out, [4, 5, 6, 1, 2, 3]);
        assert!(decode_into(&[0; 2], &config, -4, &mut None, &mut out).is_err());
    }

    #[test]
    fn computes_checked_2d_surface_extents() {
        assert_eq!(
            two_dimensional_length(&CameraConfig::new(VideoFormat::RGB, 4, 3, 30), 16).unwrap(),
            44
        );
        assert_eq!(
            two_dimensional_length(&CameraConfig::new(VideoFormat::NV12, 4, 4, 30), 8).unwrap(),
            44
        );
        assert!(
            two_dimensional_length(&CameraConfig::new(VideoFormat::YUYV, 4, 3, 30), 7).is_err()
        );
    }
}
