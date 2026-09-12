//! Reusable conversion into caller-owned RGB storage; no implicit frame copies.
#[cfg(feature = "runtime-tokio")]
use crate::CapturedFrame;
use crate::{
    utils::color_convert::{self, Yuv420Sp, YuvPlane},
    CameraError, CameraResult, ColorInfo, FrameLayout, PixelFormat,
};

/// Describes the RGB image a consumer actually needs.
///
/// A smaller target is produced directly from raw frames and uses TurboJPEG's
/// DCT scaling for MJPEG when a supported factor can reduce the decode work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConversionRequest {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// The color override value.
    pub color_override: Option<ColorInfo>,
}
impl ConversionRequest {
    /// Performs the `new` operation.
    pub fn new(width: u32, height: u32) -> CameraResult<Self> {
        let request = Self {
            width,
            height,
            color_override: None,
        };
        request.output_len()?;
        Ok(request)
    }
    /// Performs the `for_layout` operation.
    pub fn for_layout(layout: &FrameLayout) -> Self {
        Self {
            width: layout.width,
            height: layout.height,
            color_override: None,
        }
    }
    /// Performs the `with_color` operation.
    pub fn with_color(mut self, color: ColorInfo) -> Self {
        self.color_override = Some(color);
        self
    }
    /// Performs the `output_len` operation.
    pub fn output_len(self) -> CameraResult<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .filter(|&bytes| bytes > 0)
            .ok_or_else(|| CameraError::invalid_frame("Invalid RGB output dimensions".into()))
    }
}

fn is_exact_half(layout: &FrameLayout, request: ConversionRequest) -> bool {
    let (width, height) = (layout.width as usize, layout.height as usize);
    !layout.bottom_up
        && width.is_multiple_of(2)
        && height.is_multiple_of(2)
        && (request.width as usize, request.height as usize) == (width / 2, height / 2)
}

fn is_exact_two_thirds(layout: &FrameLayout, request: ConversionRequest) -> bool {
    u64::from(request.width) * 3 == u64::from(layout.width) * 2
        && u64::from(request.height) * 3 == u64::from(layout.height) * 2
}

fn is_exact_three_quarters(layout: &FrameLayout, request: ConversionRequest) -> bool {
    u64::from(request.width) * 4 == u64::from(layout.width) * 3
        && u64::from(request.height) * 4 == u64::from(layout.height) * 3
}

fn direct_half_path(layout: &FrameLayout, request: ConversionRequest) -> Option<&'static str> {
    if !is_exact_half(layout, request) {
        return None;
    }
    match layout.format {
        PixelFormat::Nv12 | PixelFormat::Nv21
            if layout.planes.len() == 2
                && layout.planes[0].pixel_stride == 1
                && layout.planes[1].pixel_stride == 2 =>
        {
            Some("nv12-direct-half")
        }
        PixelFormat::Yuyv | PixelFormat::Uyvy
            if layout.planes.len() == 1 && layout.planes[0].pixel_stride == 2 =>
        {
            Some("yuv422-direct-half")
        }
        PixelFormat::Yuv420p
            if layout.planes.len() == 3
                && layout.planes.iter().all(|plane| plane.pixel_stride == 1) =>
        {
            Some("yuv420p-direct-half")
        }
        _ => None,
    }
}

fn supports_yuv_row_conversion(layout: &FrameLayout) -> bool {
    match layout.format {
        PixelFormat::Yuyv | PixelFormat::Uyvy => true,
        PixelFormat::Nv12 | PixelFormat::Nv21 => {
            layout.planes.len() == 2
                && layout.planes[0].pixel_stride == 1
                && layout.planes[1].pixel_stride == 2
        }
        PixelFormat::Yuv420p => {
            layout.planes.len() == 3 && layout.planes.iter().all(|plane| plane.pixel_stride == 1)
        }
        _ => false,
    }
}

/// Reports the production conversion strategy used for a valid layout/request pair.
/// Intended for diagnostics and benchmark output, so benchmark labels cannot drift
/// from the real dispatch conditions.
pub fn selected_conversion_path(layout: &FrameLayout, request: ConversionRequest) -> &'static str {
    if layout.format == PixelFormat::Mjpeg {
        return "jpeg-adaptive-decode-scale";
    }
    if layout.format == PixelFormat::H264 {
        return "unsupported-h264";
    }
    if let Some(path) = direct_half_path(layout, request) {
        return path;
    }
    if (request.width, request.height) == (layout.width, layout.height) && !layout.bottom_up {
        return "direct-full-size";
    }
    if layout.format == PixelFormat::Rgb8
        && layout
            .planes
            .first()
            .is_some_and(|plane| plane.pixel_stride == 3)
    {
        if is_exact_two_thirds(layout, request) {
            return "rgb-row-nearest-2x3";
        }
        if is_exact_three_quarters(layout, request) {
            return "rgb-row-nearest-3x4";
        }
    }
    let yuv = matches!(
        layout.format,
        PixelFormat::Yuyv
            | PixelFormat::Uyvy
            | PixelFormat::Nv12
            | PixelFormat::Nv21
            | PixelFormat::Yuv420p
    );
    if yuv
        && supports_yuv_row_conversion(layout)
        && (request.width as usize).saturating_mul(4) >= layout.width as usize
    {
        if is_exact_two_thirds(layout, request) {
            "yuv-row-convert-nearest-2x3"
        } else if is_exact_three_quarters(layout, request) {
            "yuv-row-convert-nearest-3x4"
        } else {
            "yuv-row-convert-nearest"
        }
    } else {
        "mapped-nearest"
    }
}

#[derive(Default)]
struct NearestMap {
    dimensions: (usize, usize, usize, usize, bool),
    x: Vec<usize>,
    y: Vec<usize>,
}

impl NearestMap {
    fn update(
        &mut self,
        source_width: usize,
        source_height: usize,
        destination_width: usize,
        destination_height: usize,
        bottom_up: bool,
    ) {
        let dimensions = (
            source_width,
            source_height,
            destination_width,
            destination_height,
            bottom_up,
        );
        if self.dimensions == dimensions {
            return;
        }
        self.dimensions = dimensions;
        self.x.resize(destination_width, 0);
        self.y.resize(destination_height, 0);
        for (destination, source) in self.x.iter_mut().enumerate() {
            *source = destination * source_width / destination_width;
        }
        for (destination, source) in self.y.iter_mut().enumerate() {
            let row = destination * source_height / destination_height;
            *source = if bottom_up {
                source_height - row - 1
            } else {
                row
            };
        }
    }
}

#[derive(Default)]
/// Values for `RgbConverter`.
pub struct RgbConverter {
    #[cfg(feature = "decode-mjpeg")]
    jpeg: Option<turbojpeg::Decompressor>,
    #[cfg(feature = "decode-mjpeg")]
    jpeg_scratch: Vec<u8>,
    nearest: NearestMap,
    rgb_row_scratch: Vec<u8>,
}
impl RgbConverter {
    /// Performs the `new` operation.
    pub fn new() -> Self {
        Self::default()
    }
    /// Convert without applying orientation/mirroring. Unknown YUV colorimetry
    /// requires an explicit override instead of silently assuming a matrix/range.
    #[cfg(feature = "runtime-tokio")]
    pub fn convert_into(
        &mut self,
        frame: &CapturedFrame,
        request: ConversionRequest,
        output: &mut [u8],
    ) -> CameraResult<()> {
        self.convert_layout_into(frame.layout(), frame.bytes(), request, output)
    }
    /// Performs the `convert_layout_into` operation.
    pub fn convert_layout_into(
        &mut self,
        layout: &FrameLayout,
        data: &[u8],
        request: ConversionRequest,
        out: &mut [u8],
    ) -> CameraResult<()> {
        layout.validate(data.len())?;
        let (w, h) = (layout.width as usize, layout.height as usize);
        let (out_w, out_h) = (request.width as usize, request.height as usize);
        if request.output_len()? != out.len() {
            return Err(CameraError::invalid_frame(
                "RGB destination size mismatch".into(),
            ));
        }
        if layout.format == PixelFormat::Mjpeg {
            #[cfg(feature = "decode-mjpeg")]
            {
                let (decoder, scratch, nearest) =
                    (&mut self.jpeg, &mut self.jpeg_scratch, &mut self.nearest);
                return crate::mjpeg::decode_with(decoder, data, |decoder, data| {
                    let header = decoder
                        .read_header(data)
                        .map_err(|e| CameraError::invalid_frame(e.to_string()))?;
                    if (header.width, header.height) != (w, h) {
                        return Err(CameraError::invalid_frame(
                            "JPEG dimensions differ from layout".into(),
                        ));
                    }
                    let scaling = jpeg_scaling(&header, out_w, out_h);
                    decoder
                        .set_scaling_factor(scaling)
                        .map_err(|e| CameraError::invalid_frame(e.to_string()))?;
                    let scaled = header.scaled(scaling);
                    if (scaled.width, scaled.height) == (out_w, out_h) {
                        return decoder
                            .decompress(
                                data,
                                turbojpeg::Image {
                                    pixels: out,
                                    width: out_w,
                                    height: out_h,
                                    pitch: out_w * 3,
                                    format: turbojpeg::PixelFormat::RGB,
                                },
                            )
                            .map_err(|e| CameraError::invalid_frame(e.to_string()));
                    }
                    let scratch_len = scaled
                        .width
                        .checked_mul(scaled.height)
                        .and_then(|pixels| pixels.checked_mul(3))
                        .ok_or_else(|| {
                            CameraError::invalid_frame("JPEG output size overflow".into())
                        })?;
                    scratch.resize(scratch_len, 0);
                    decoder
                        .decompress(
                            data,
                            turbojpeg::Image {
                                pixels: scratch,
                                width: scaled.width,
                                height: scaled.height,
                                pitch: scaled.width * 3,
                                format: turbojpeg::PixelFormat::RGB,
                            },
                        )
                        .map_err(|e| CameraError::invalid_frame(e.to_string()))?;
                    nearest.update(scaled.width, scaled.height, out_w, out_h, false);
                    resize_rgb_nearest(scratch, scaled.width, out, out_w, &nearest.x, &nearest.y);
                    Ok(())
                });
            }
            #[cfg(not(feature = "decode-mjpeg"))]
            return Err(CameraError::unsupported_format(
                "Enable jpeg for MJPEG decoding".into(),
            ));
        }
        if layout.format == PixelFormat::H264 {
            return Err(CameraError::unsupported_format(
                "H264 decoding requires an external video decoder".into(),
            ));
        }
        let yuv = matches!(
            layout.format,
            PixelFormat::Yuyv
                | PixelFormat::Uyvy
                | PixelFormat::Nv12
                | PixelFormat::Nv21
                | PixelFormat::Yuv420p
        );
        let color = request.color_override.unwrap_or(layout.color);
        let coeff = if yuv {
            Some(color_convert::color_coefficients(color)?)
        } else {
            None
        };
        if direct_half_path(layout, request).is_some() {
            let p = |index: usize| {
                let plane = &layout.planes[index];
                YuvPlane {
                    data: &data[plane.offset..plane.offset + plane.length],
                    row_stride: plane.row_stride,
                    pixel_stride: plane.pixel_stride,
                }
            };
            return match layout.format {
                PixelFormat::Nv12 | PixelFormat::Nv21 => {
                    let y = &layout.planes[0];
                    let uv = &layout.planes[1];
                    color_convert::yuv420sp_to_rgb_half_with_coefficients_into(
                        Yuv420Sp {
                            y: &data[y.offset..y.offset + y.length],
                            uv: &data[uv.offset..uv.offset + uv.length],
                            y_stride: y.row_stride,
                            uv_stride: uv.row_stride,
                            vu_order: layout.format == PixelFormat::Nv21,
                        },
                        w,
                        h,
                        coeff.unwrap(),
                        out,
                    )
                }
                PixelFormat::Yuyv | PixelFormat::Uyvy => {
                    let plane = &layout.planes[0];
                    color_convert::yuv422_to_rgb_half_with_coefficients_into(
                        &data[plane.offset..plane.offset + plane.length],
                        w,
                        h,
                        plane.row_stride,
                        layout.format == PixelFormat::Uyvy,
                        coeff.unwrap(),
                        out,
                    )
                }
                PixelFormat::Yuv420p => color_convert::yuv420_to_rgb_half_with_coefficients_into(
                    p(0),
                    p(1),
                    p(2),
                    w,
                    h,
                    coeff.unwrap(),
                    out,
                ),
                _ => unreachable!(),
            };
        }
        // Same-size top-down frames use checked vectorized kernels. Scaling and
        // bottom-up layouts keep a direct scalar path without an intermediate image.
        if (out_w, out_h) == (w, h) && !layout.bottom_up {
            let plane = &layout.planes[0];
            let source = &data[plane.offset..plane.offset + plane.length];
            if layout.format == PixelFormat::Rgb8 && plane.pixel_stride == 3 {
                for y in 0..h {
                    out[y * w * 3..(y + 1) * w * 3].copy_from_slice(
                        &source[y * plane.row_stride..y * plane.row_stride + w * 3],
                    );
                }
                return Ok(());
            }
            if layout.format == PixelFormat::Rgba8 && plane.pixel_stride == 4 {
                return crate::utils::color_convert::rgba8888_to_rgb_into(
                    source,
                    w,
                    h,
                    plane.row_stride,
                    out,
                );
            }
            if layout.format == PixelFormat::Bgra8 && plane.pixel_stride == 4 {
                return crate::utils::color_convert::bgra8888_to_rgb_into(
                    source,
                    w,
                    h,
                    plane.row_stride,
                    out,
                );
            }
            if layout.format == PixelFormat::Argb8 && plane.pixel_stride == 4 {
                return crate::utils::color_convert::argb8888_to_rgb_into(
                    source,
                    w,
                    h,
                    plane.row_stride,
                    out,
                );
            }
            let p = |i: usize, shift: usize| {
                let plane = &layout.planes[i];
                YuvPlane {
                    data: &data[plane.offset + shift..plane.offset + plane.length],
                    row_stride: plane.row_stride,
                    pixel_stride: plane.pixel_stride,
                }
            };
            match layout.format {
                PixelFormat::Yuv420p => {
                    return color_convert::yuv420_to_rgb_with_coefficients_into(
                        p(0, 0),
                        p(1, 0),
                        p(2, 0),
                        w,
                        h,
                        coeff.unwrap(),
                        out,
                    );
                }
                PixelFormat::Nv12 | PixelFormat::Nv21 => {
                    let y = &layout.planes[0];
                    let uv = &layout.planes[1];
                    if y.pixel_stride == 1 && uv.pixel_stride == 2 {
                        return color_convert::yuv420sp_to_rgb_with_coefficients_into(
                            Yuv420Sp {
                                y: &data[y.offset..y.offset + y.length],
                                uv: &data[uv.offset..uv.offset + uv.length],
                                y_stride: y.row_stride,
                                uv_stride: uv.row_stride,
                                vu_order: layout.format == PixelFormat::Nv21,
                            },
                            w,
                            h,
                            coeff.unwrap(),
                            out,
                        );
                    }
                    let (u, v) = if layout.format == PixelFormat::Nv12 {
                        (p(1, 0), p(1, 1))
                    } else {
                        (p(1, 1), p(1, 0))
                    };
                    return color_convert::yuv420_to_rgb_with_coefficients_into(
                        p(0, 0),
                        u,
                        v,
                        w,
                        h,
                        coeff.unwrap(),
                        out,
                    );
                }
                PixelFormat::Yuyv | PixelFormat::Uyvy => {
                    return color_convert::yuv422_to_rgb_with_coefficients_into(
                        source,
                        w,
                        h,
                        plane.row_stride,
                        layout.format == PixelFormat::Uyvy,
                        coeff.unwrap(),
                        out,
                    );
                }
                _ => {}
            }
        }
        self.nearest.update(w, h, out_w, out_h, layout.bottom_up);
        let yuv_row_fast = supports_yuv_row_conversion(layout);
        if yuv && yuv_row_fast && out_w.saturating_mul(4) >= w {
            self.rgb_row_scratch.resize(w * 6, 0);
            return scale_yuv_nearest(
                layout,
                data,
                out,
                &self.nearest.x,
                &self.nearest.y,
                coeff.unwrap(),
                &mut self.rgb_row_scratch,
            );
        }
        if yuv {
            return scale_yuv_scalar(
                layout,
                data,
                out,
                out_w,
                &self.nearest.x,
                &self.nearest.y,
                coeff.unwrap(),
            );
        }
        scale_nearest(layout, data, out, out_w, &self.nearest.x, &self.nearest.y)
    }
}

#[cfg(feature = "decode-mjpeg")]
fn jpeg_scaling(
    header: &turbojpeg::DecompressHeader,
    target_width: usize,
    target_height: usize,
) -> turbojpeg::ScalingFactor {
    if header.is_lossless || target_width >= header.width || target_height >= header.height {
        return turbojpeg::ScalingFactor::ONE;
    }
    turbojpeg::Decompressor::supported_scaling_factors()
        .into_iter()
        .filter(|factor| factor.num() <= factor.denom())
        // libjpeg-turbo's fractional IDCTs above 1/2 (notably 3/4) are
        // substantially slower than its full-size SIMD IDCT on tested ARM64
        // and x86 builds. Decode full-size and use the cached nearest map.
        .filter(|factor| factor.num() * 2 <= factor.denom() || factor.num() == factor.denom())
        .filter(|factor| {
            factor.scale(header.width) >= target_width
                && factor.scale(header.height) >= target_height
        })
        .min_by_key(|factor| {
            factor
                .scale(header.width)
                .saturating_mul(factor.scale(header.height))
        })
        .unwrap_or(turbojpeg::ScalingFactor::ONE)
}

#[cfg(feature = "decode-mjpeg")]
fn resize_rgb_nearest(
    source: &[u8],
    source_width: usize,
    destination: &mut [u8],
    destination_width: usize,
    x_map: &[usize],
    y_map: &[usize],
) {
    for (destination_row, &source_y) in destination
        .chunks_exact_mut(destination_width * 3)
        .zip(y_map)
    {
        let source_row = &source[source_y * source_width * 3..][..source_width * 3];
        resize_rgb_row_nearest(source_row, destination_row, x_map);
    }
}

fn scale_nearest(
    layout: &FrameLayout,
    data: &[u8],
    destination: &mut [u8],
    destination_width: usize,
    x_map: &[usize],
    y_map: &[usize],
) -> CameraResult<()> {
    let packed = &layout.planes[0];
    match layout.format {
        PixelFormat::Rgb8 if packed.pixel_stride == 3 => {
            let source_width = layout.width as usize;
            for (destination_row, &source_y) in destination
                .chunks_exact_mut(destination_width * 3)
                .zip(y_map)
            {
                let row = packed.offset + source_y * packed.row_stride;
                resize_rgb_row_nearest(&data[row..row + source_width * 3], destination_row, x_map);
            }
        }
        PixelFormat::Rgb8 | PixelFormat::Rgba8 => {
            for (destination_row, &source_y) in destination
                .chunks_exact_mut(destination_width * 3)
                .zip(y_map)
            {
                let row = packed.offset + source_y * packed.row_stride;
                for (pixel, &source_x) in
                    destination_row.as_chunks_mut::<3>().0.iter_mut().zip(x_map)
                {
                    let source = row + source_x * packed.pixel_stride;
                    pixel.copy_from_slice(&data[source..source + 3]);
                }
            }
        }
        PixelFormat::Bgr8 | PixelFormat::Bgra8 => {
            for (destination_row, &source_y) in destination
                .chunks_exact_mut(destination_width * 3)
                .zip(y_map)
            {
                let row = packed.offset + source_y * packed.row_stride;
                for (pixel, &source_x) in
                    destination_row.as_chunks_mut::<3>().0.iter_mut().zip(x_map)
                {
                    let source = row + source_x * packed.pixel_stride;
                    pixel[0] = data[source + 2];
                    pixel[1] = data[source + 1];
                    pixel[2] = data[source];
                }
            }
        }
        PixelFormat::Argb8 => {
            for (destination_row, &source_y) in destination
                .chunks_exact_mut(destination_width * 3)
                .zip(y_map)
            {
                let row = packed.offset + source_y * packed.row_stride;
                for (pixel, &source_x) in
                    destination_row.as_chunks_mut::<3>().0.iter_mut().zip(x_map)
                {
                    let source = row + source_x * packed.pixel_stride + 1;
                    pixel.copy_from_slice(&data[source..source + 3]);
                }
            }
        }
        PixelFormat::Gray8 => {
            for (destination_row, &source_y) in destination
                .chunks_exact_mut(destination_width * 3)
                .zip(y_map)
            {
                let row = packed.offset + source_y * packed.row_stride;
                for (pixel, &source_x) in
                    destination_row.as_chunks_mut::<3>().0.iter_mut().zip(x_map)
                {
                    pixel.fill(data[row + source_x * packed.pixel_stride]);
                }
            }
        }
        PixelFormat::Yuyv
        | PixelFormat::Uyvy
        | PixelFormat::Nv12
        | PixelFormat::Nv21
        | PixelFormat::Yuv420p => unreachable!("YUV scaling uses scale_yuv_nearest"),
        PixelFormat::Mjpeg | PixelFormat::H264 => unreachable!(),
    }
    Ok(())
}

fn scale_yuv_scalar(
    layout: &FrameLayout,
    data: &[u8],
    destination: &mut [u8],
    destination_width: usize,
    x_map: &[usize],
    y_map: &[usize],
    coefficients: [i32; 6],
) -> CameraResult<()> {
    let y_plane = &layout.planes[0];
    for (destination_row, &source_y) in destination
        .chunks_exact_mut(destination_width * 3)
        .zip(y_map)
    {
        let y_row = y_plane.offset + source_y * y_plane.row_stride;
        for (pixel, &source_x) in destination_row.as_chunks_mut::<3>().0.iter_mut().zip(x_map) {
            let y_offset = y_row + source_x * y_plane.pixel_stride;
            let (y, u, v) = match layout.format {
                PixelFormat::Yuyv | PixelFormat::Uyvy => {
                    let pair = y_row + (source_x & !1) * 2;
                    if layout.format == PixelFormat::Uyvy {
                        (data[y_offset + 1], data[pair], data[pair + 2])
                    } else {
                        (data[y_offset], data[pair + 1], data[pair + 3])
                    }
                }
                PixelFormat::Nv12 | PixelFormat::Nv21 => {
                    let chroma = &layout.planes[1];
                    let uv = chroma.offset
                        + (source_y / 2) * chroma.row_stride
                        + (source_x / 2) * chroma.pixel_stride;
                    if layout.format == PixelFormat::Nv12 {
                        (data[y_offset], data[uv], data[uv + 1])
                    } else {
                        (data[y_offset], data[uv + 1], data[uv])
                    }
                }
                PixelFormat::Yuv420p => {
                    let u = &layout.planes[1];
                    let v = &layout.planes[2];
                    (
                        data[y_offset],
                        data[u.offset
                            + (source_y / 2) * u.row_stride
                            + (source_x / 2) * u.pixel_stride],
                        data[v.offset
                            + (source_y / 2) * v.row_stride
                            + (source_x / 2) * v.pixel_stride],
                    )
                }
                _ => unreachable!(),
            };
            color_convert::yuv_pixel(coefficients, y, u, v, pixel);
        }
    }
    Ok(())
}

fn scale_yuv_nearest(
    layout: &FrameLayout,
    data: &[u8],
    destination: &mut [u8],
    x_map: &[usize],
    y_map: &[usize],
    coefficients: [i32; 6],
    rgb_row: &mut [u8],
) -> CameraResult<()> {
    let width = layout.width as usize;
    let source_row_bytes = width * 3;
    let destination_row_bytes = x_map.len() * 3;
    let pairable = matches!(
        layout.format,
        PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Yuv420p
    );
    let mut destination_rows = destination.chunks_exact_mut(destination_row_bytes);
    let mut index = 0;
    while let Some(destination_row) = destination_rows.next() {
        let source_y = y_map[index];
        let paired =
            pairable && source_y.is_multiple_of(2) && y_map.get(index + 1) == Some(&(source_y + 1));
        if paired {
            let next_destination = destination_rows
                .next()
                .expect("paired source rows require paired destinations");
            convert_yuv_rows(
                layout,
                data,
                source_y,
                2,
                coefficients,
                &mut rgb_row[..source_row_bytes * 2],
            )?;
            let (first_source, second_source) = rgb_row.split_at(source_row_bytes);
            resize_rgb_row_nearest(first_source, destination_row, x_map);
            resize_rgb_row_nearest(&second_source[..source_row_bytes], next_destination, x_map);
            index += 2;
        } else {
            convert_yuv_rows(
                layout,
                data,
                source_y,
                1,
                coefficients,
                &mut rgb_row[..source_row_bytes],
            )?;
            resize_rgb_row_nearest(&rgb_row[..source_row_bytes], destination_row, x_map);
            index += 1;
        }
    }
    Ok(())
}

fn convert_yuv_rows(
    layout: &FrameLayout,
    data: &[u8],
    source_y: usize,
    rows: usize,
    coefficients: [i32; 6],
    rgb: &mut [u8],
) -> CameraResult<()> {
    let width = layout.width as usize;
    let y_plane = &layout.planes[0];
    let y_start = y_plane.offset + source_y * y_plane.row_stride;
    match layout.format {
        PixelFormat::Yuyv | PixelFormat::Uyvy => {
            let row_bytes = width * 2;
            let length = (rows - 1) * y_plane.row_stride + row_bytes;
            color_convert::yuv422_to_rgb_with_coefficients_into(
                &data[y_start..y_start + length],
                width,
                rows,
                y_plane.row_stride,
                layout.format == PixelFormat::Uyvy,
                coefficients,
                rgb,
            )
        }
        PixelFormat::Nv12 | PixelFormat::Nv21 => {
            let uv_plane = &layout.planes[1];
            let uv_start = uv_plane.offset + (source_y / 2) * uv_plane.row_stride;
            let chroma_width = width.div_ceil(2) * 2;
            let y_length = (rows - 1) * y_plane.row_stride + width;
            color_convert::yuv420sp_to_rgb_with_coefficients_into(
                Yuv420Sp {
                    y: &data[y_start..y_start + y_length],
                    uv: &data[uv_start..uv_start + chroma_width],
                    y_stride: y_plane.row_stride,
                    uv_stride: uv_plane.row_stride,
                    vu_order: layout.format == PixelFormat::Nv21,
                },
                width,
                rows,
                coefficients,
                rgb,
            )
        }
        PixelFormat::Yuv420p => {
            let u_plane = &layout.planes[1];
            let v_plane = &layout.planes[2];
            let u_start = u_plane.offset + (source_y / 2) * u_plane.row_stride;
            let v_start = v_plane.offset + (source_y / 2) * v_plane.row_stride;
            let chroma_width = width.div_ceil(2);
            let y_length = (rows - 1) * y_plane.row_stride + width;
            color_convert::yuv420_to_rgb_with_coefficients_into(
                YuvPlane {
                    data: &data[y_start..y_start + y_length],
                    row_stride: y_plane.row_stride,
                    pixel_stride: y_plane.pixel_stride,
                },
                YuvPlane {
                    data: &data[u_start..u_start + chroma_width],
                    row_stride: u_plane.row_stride,
                    pixel_stride: u_plane.pixel_stride,
                },
                YuvPlane {
                    data: &data[v_start..v_start + chroma_width],
                    row_stride: v_plane.row_stride,
                    pixel_stride: v_plane.pixel_stride,
                },
                width,
                rows,
                coefficients,
                rgb,
            )
        }
        _ => unreachable!(),
    }
}

fn resize_rgb_row_nearest(source_row: &[u8], destination: &mut [u8], x_map: &[usize]) {
    let source_width = source_row.len() / 3;
    let destination_width = destination.len() / 3;
    if source_width.is_multiple_of(3)
        && destination_width * 3 == source_width * 2
        && x_map.len() == destination_width
    {
        let (source_groups, source_tail) = source_row.as_chunks::<9>();
        let (destination_groups, destination_tail) = destination.as_chunks_mut::<6>();
        debug_assert!(source_tail.is_empty() && destination_tail.is_empty());
        for (source, destination) in source_groups.iter().zip(destination_groups) {
            destination.copy_from_slice(&source[..6]);
        }
        return;
    }
    if source_width.is_multiple_of(4)
        && destination_width * 4 == source_width * 3
        && x_map.len() == destination_width
    {
        let (source_groups, source_tail) = source_row.as_chunks::<12>();
        let (destination_groups, destination_tail) = destination.as_chunks_mut::<9>();
        debug_assert!(source_tail.is_empty() && destination_tail.is_empty());
        for (source, destination) in source_groups.iter().zip(destination_groups) {
            destination.copy_from_slice(&source[..9]);
        }
        return;
    }
    for (pixel, &source_x) in destination.as_chunks_mut::<3>().0.iter_mut().zip(x_map) {
        let source = source_x * 3;
        pixel.copy_from_slice(&source_row[source..source + 3]);
    }
}
