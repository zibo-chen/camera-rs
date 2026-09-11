//! Reusable conversion into caller-owned RGB storage; no implicit frame copies.
use crate::{
    utils::color_convert::{self, Yuv420Sp, YuvPlane},
    CameraError, CameraResult, CapturedFrame, ColorInfo, FrameLayout, PixelFormat,
};

/// Describes the RGB image a consumer actually needs.
///
/// A smaller target is produced directly from raw frames and uses TurboJPEG's
/// DCT scaling for MJPEG when a supported factor can reduce the decode work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConversionRequest {
    pub width: u32,
    pub height: u32,
    pub color_override: Option<ColorInfo>,
}
impl ConversionRequest {
    pub fn new(width: u32, height: u32) -> CameraResult<Self> {
        let request = Self {
            width,
            height,
            color_override: None,
        };
        request.output_len()?;
        Ok(request)
    }
    pub fn for_layout(layout: &FrameLayout) -> Self {
        Self {
            width: layout.width,
            height: layout.height,
            color_override: None,
        }
    }
    pub fn with_color(mut self, color: ColorInfo) -> Self {
        self.color_override = Some(color);
        self
    }
    pub fn output_len(self) -> CameraResult<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .filter(|&bytes| bytes > 0)
            .ok_or_else(|| CameraError::InvalidFormat("Invalid RGB output dimensions".into()))
    }
}

#[derive(Default)]
pub struct RgbConverter {
    #[cfg(feature = "turbojpeg")]
    jpeg: Option<turbojpeg::Decompressor>,
    #[cfg(feature = "turbojpeg")]
    jpeg_scratch: Vec<u8>,
}
impl RgbConverter {
    pub fn new() -> Self {
        Self::default()
    }
    /// Convert without applying orientation/mirroring. Unknown YUV colorimetry
    /// requires an explicit override instead of silently assuming a matrix/range.
    pub fn convert_into(
        &mut self,
        frame: &CapturedFrame,
        request: ConversionRequest,
        output: &mut [u8],
    ) -> CameraResult<()> {
        self.convert_layout_into(frame.layout(), frame.bytes(), request, output)
    }
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
            return Err(CameraError::InvalidFormat(
                "RGB destination size mismatch".into(),
            ));
        }
        if layout.format == PixelFormat::Mjpeg {
            #[cfg(feature = "turbojpeg")]
            {
                if self.jpeg.is_none() {
                    self.jpeg = Some(
                        turbojpeg::Decompressor::new()
                            .map_err(|e| CameraError::InvalidFormat(e.to_string()))?,
                    );
                }
                let decoder = self.jpeg.as_mut().unwrap();
                let header = decoder
                    .read_header(data)
                    .map_err(|e| CameraError::InvalidFormat(e.to_string()))?;
                if (header.width, header.height) != (w, h) {
                    return Err(CameraError::InvalidFormat(
                        "JPEG dimensions differ from layout".into(),
                    ));
                }
                let scaling = jpeg_scaling(&header, out_w, out_h);
                decoder
                    .set_scaling_factor(scaling)
                    .map_err(|e| CameraError::InvalidFormat(e.to_string()))?;
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
                        .map_err(|e| CameraError::InvalidFormat(e.to_string()));
                }
                let scratch_len = scaled
                    .width
                    .checked_mul(scaled.height)
                    .and_then(|pixels| pixels.checked_mul(3))
                    .ok_or_else(|| {
                        CameraError::InvalidFormat("JPEG output size overflow".into())
                    })?;
                self.jpeg_scratch.resize(scratch_len, 0);
                decoder
                    .decompress(
                        data,
                        turbojpeg::Image {
                            pixels: &mut self.jpeg_scratch,
                            width: scaled.width,
                            height: scaled.height,
                            pitch: scaled.width * 3,
                            format: turbojpeg::PixelFormat::RGB,
                        },
                    )
                    .map_err(|e| CameraError::InvalidFormat(e.to_string()))?;
                resize_rgb_nearest(
                    &self.jpeg_scratch,
                    scaled.width,
                    scaled.height,
                    out,
                    out_w,
                    out_h,
                );
                return Ok(());
            }
            #[cfg(not(feature = "turbojpeg"))]
            return Err(CameraError::UnsupportedFormat(
                "Enable jpeg for MJPEG decoding".into(),
            ));
        }
        if layout.format == PixelFormat::H264 {
            return Err(CameraError::UnsupportedFormat(
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
        if !layout.bottom_up
            && w.is_multiple_of(2)
            && h.is_multiple_of(2)
            && (out_w, out_h) == (w / 2, h / 2)
            && matches!(layout.format, PixelFormat::Nv12 | PixelFormat::Nv21)
            && layout.planes[0].pixel_stride == 1
            && layout.planes[1].pixel_stride == 2
        {
            let y = &layout.planes[0];
            let uv = &layout.planes[1];
            return color_convert::yuv420sp_to_rgb_half_with_coefficients_into(
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
        let at = |plane: usize, x: usize, y: usize| {
            let p = &layout.planes[plane];
            p.offset + y * p.row_stride + x * p.pixel_stride
        };
        for y in 0..out_h {
            let source_y = y * h / out_h;
            let sy = if layout.bottom_up {
                h - source_y - 1
            } else {
                source_y
            };
            for x in 0..out_w {
                let source_x = x * w / out_w;
                let i = at(0, source_x, sy);
                let dst = &mut out[(y * out_w + x) * 3..][..3];
                match layout.format {
                    PixelFormat::Rgb8 | PixelFormat::Rgba8 => dst.copy_from_slice(&data[i..i + 3]),
                    PixelFormat::Bgr8 | PixelFormat::Bgra8 => {
                        dst[0] = data[i + 2];
                        dst[1] = data[i + 1];
                        dst[2] = data[i];
                    }
                    PixelFormat::Argb8 => dst.copy_from_slice(&data[i + 1..i + 4]),
                    PixelFormat::Gray8 => dst.fill(data[i]),
                    _ => {
                        let (yy, u, v) = match layout.format {
                            PixelFormat::Yuyv => {
                                let pair = at(0, source_x & !1, sy);
                                (data[i], data[pair + 1], data[pair + 3])
                            }
                            PixelFormat::Uyvy => {
                                let pair = at(0, source_x & !1, sy);
                                (data[i + 1], data[pair], data[pair + 2])
                            }
                            PixelFormat::Nv12 | PixelFormat::Nv21 => {
                                let uv = at(1, source_x / 2, sy / 2);
                                if layout.format == PixelFormat::Nv12 {
                                    (data[i], data[uv], data[uv + 1])
                                } else {
                                    (data[i], data[uv + 1], data[uv])
                                }
                            }
                            PixelFormat::Yuv420p => (
                                data[i],
                                data[at(1, source_x / 2, sy / 2)],
                                data[at(2, source_x / 2, sy / 2)],
                            ),
                            _ => unreachable!(),
                        };
                        color_convert::yuv_pixel(coeff.unwrap(), yy, u, v, dst);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(feature = "turbojpeg")]
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

#[cfg(feature = "turbojpeg")]
fn resize_rgb_nearest(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    destination: &mut [u8],
    destination_width: usize,
    destination_height: usize,
) {
    for y in 0..destination_height {
        let source_y = y * source_height / destination_height;
        for x in 0..destination_width {
            let source_x = x * source_width / destination_width;
            let source_offset = (source_y * source_width + source_x) * 3;
            let destination_offset = (y * destination_width + x) * 3;
            destination[destination_offset..destination_offset + 3]
                .copy_from_slice(&source[source_offset..source_offset + 3]);
        }
    }
}
