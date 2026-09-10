//! Reusable conversion into caller-owned RGB storage; no implicit frame copies.
use crate::{
    CameraError, CameraResult, CapturedFrame, ColorInfo, ColorMatrix, ColorRange, FrameLayout,
    PixelFormat,
};

#[derive(Default)]
pub struct RgbConverter {
    #[cfg(feature = "turbojpeg")]
    jpeg: Option<turbojpeg::Decompressor>,
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
        color_override: Option<ColorInfo>,
        output: &mut [u8],
    ) -> CameraResult<()> {
        self.convert_layout_into(frame.layout(), frame.bytes(), color_override, output)
    }
    pub fn convert_layout_into(
        &mut self,
        layout: &FrameLayout,
        data: &[u8],
        color_override: Option<ColorInfo>,
        out: &mut [u8],
    ) -> CameraResult<()> {
        layout.validate(data.len())?;
        let (w, h) = (layout.width as usize, layout.height as usize);
        if w.checked_mul(h).and_then(|n| n.checked_mul(3)) != Some(out.len()) {
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
                return decoder
                    .decompress(
                        data,
                        turbojpeg::Image {
                            pixels: out,
                            width: w,
                            height: h,
                            pitch: w * 3,
                            format: turbojpeg::PixelFormat::RGB,
                        },
                    )
                    .map_err(|e| CameraError::InvalidFormat(e.to_string()));
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
        let coeff = if yuv {
            Some(coefficients(color_override.unwrap_or(layout.color))?)
        } else {
            None
        };
        // Packed RGB/RGBA and common full-range BT.601 YUV use the checked
        // vectorized backend kernels; unusual layouts/colorimetry use the scalar path.
        if !layout.bottom_up {
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
            let color = color_override.unwrap_or(layout.color);
            if color.matrix == ColorMatrix::Bt601 && color.range == ColorRange::Full {
                use crate::utils::color_convert::{self, YuvPlane};
                let p = |i: usize, shift: usize| {
                    let p = &layout.planes[i];
                    YuvPlane {
                        data: &data[p.offset + shift..p.offset + p.length],
                        row_stride: p.row_stride,
                        pixel_stride: p.pixel_stride,
                    }
                };
                match layout.format {
                    PixelFormat::Yuv420p => {
                        return color_convert::yuv420_to_rgb_into(
                            p(0, 0),
                            p(1, 0),
                            p(2, 0),
                            w,
                            h,
                            out,
                        )
                    }
                    PixelFormat::Nv12 | PixelFormat::Nv21 => {
                        let (u, v) = if layout.format == PixelFormat::Nv12 {
                            (p(1, 0), p(1, 1))
                        } else {
                            (p(1, 1), p(1, 0))
                        };
                        return color_convert::yuv420_to_rgb_into(p(0, 0), u, v, w, h, out);
                    }
                    PixelFormat::Yuyv | PixelFormat::Uyvy => {
                        for y in 0..h {
                            let src = &source[y * plane.row_stride..y * plane.row_stride + w * 2];
                            let dst = &mut out[y * w * 3..(y + 1) * w * 3];
                            if layout.format == PixelFormat::Yuyv {
                                color_convert::yuyv_to_rgb_into(src, w as u32, 1, dst)?;
                            } else {
                                color_convert::uyvy_to_rgb_into(src, w as u32, 1, dst)?;
                            }
                        }
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
        let at = |plane: usize, x: usize, y: usize| {
            let p = &layout.planes[plane];
            p.offset + y * p.row_stride + x * p.pixel_stride
        };
        for y in 0..h {
            let sy = if layout.bottom_up { h - y - 1 } else { y };
            for x in 0..w {
                let i = at(0, x, sy);
                let dst = &mut out[(y * w + x) * 3..][..3];
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
                                let pair = at(0, x & !1, sy);
                                (data[i], data[pair + 1], data[pair + 3])
                            }
                            PixelFormat::Uyvy => {
                                let pair = at(0, x & !1, sy);
                                (data[i + 1], data[pair], data[pair + 2])
                            }
                            PixelFormat::Nv12 | PixelFormat::Nv21 => {
                                let uv = at(1, x / 2, sy / 2);
                                if layout.format == PixelFormat::Nv12 {
                                    (data[i], data[uv], data[uv + 1])
                                } else {
                                    (data[i], data[uv + 1], data[uv])
                                }
                            }
                            PixelFormat::Yuv420p => (
                                data[i],
                                data[at(1, x / 2, sy / 2)],
                                data[at(2, x / 2, sy / 2)],
                            ),
                            _ => unreachable!(),
                        };
                        pixel(coeff.unwrap(), yy, u, v, dst);
                    }
                }
            }
        }
        Ok(())
    }
}
pub(crate) fn coefficients(color: ColorInfo) -> CameraResult<[i32; 6]> {
    let (kr, kb): (f64, f64) = match color.matrix {
        ColorMatrix::Bt601 => (0.299, 0.114),
        ColorMatrix::Bt709 => (0.2126, 0.0722),
        ColorMatrix::Bt2020 => (0.2627, 0.0593),
        ColorMatrix::Smpte240M => (0.212, 0.087),
        ColorMatrix::Unknown => {
            return Err(CameraError::UnsupportedFormat(
                "YUV color matrix is unknown; supply a color override".into(),
            ))
        }
    };
    let (sy, sc, offset) = match color.range {
        ColorRange::Full => (1.0, 1.0, 0),
        ColorRange::Limited => (255.0 / 219.0, 255.0 / 224.0, 16),
        ColorRange::Unknown => {
            return Err(CameraError::UnsupportedFormat(
                "YUV quantization range is unknown; supply a color override".into(),
            ))
        }
    };
    let kg = 1.0 - kr - kb;
    Ok([
        (sy * 1024.0_f64).round() as i32,
        ((2.0 - 2.0 * kr) * sc * 1024.0).round() as i32,
        ((2.0 - 2.0 * kb) * kb / kg * sc * 1024.0).round() as i32,
        ((2.0 - 2.0 * kr) * kr / kg * sc * 1024.0).round() as i32,
        ((2.0 - 2.0 * kb) * sc * 1024.0).round() as i32,
        offset,
    ])
}
pub(crate) fn pixel([cy, cr, gu, gv, cb, offset]: [i32; 6], y: u8, u: u8, v: u8, out: &mut [u8]) {
    let y = (y as i32 - offset) * cy;
    let u = u as i32 - 128;
    let v = v as i32 - 128;
    out[0] = ((y + cr * v + 512) >> 10).clamp(0, 255) as u8;
    out[1] = ((y - gu * u - gv * v + 512) >> 10).clamp(0, 255) as u8;
    out[2] = ((y + cb * u + 512) >> 10).clamp(0, 255) as u8;
}
