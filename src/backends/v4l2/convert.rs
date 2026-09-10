use crate::utils::color_convert::{
    uyvy_to_rgb_into, yuv420_to_rgb_into, yuyv_to_rgb_into, YuvPlane,
};
use crate::{CameraConfig, CameraError, CameraResult, VideoFormat};
use turbojpeg::{Decompressor, Image, PixelFormat};

#[derive(Clone, Copy)]
pub(super) struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

pub(super) fn from_fourcc(code: u32) -> Option<VideoFormat> {
    match &code.to_le_bytes() {
        b"MJPG" => Some(VideoFormat::MJPEG),
        b"YUYV" => Some(VideoFormat::YUYV),
        b"UYVY" => Some(VideoFormat::UYVY),
        b"NV12" | b"NM12" => Some(VideoFormat::NV12),
        b"RGB3" => Some(VideoFormat::RGB),
        b"GREY" => Some(VideoFormat::Gray),
        _ => None,
    }
}

pub(super) fn to_fourcc(format: VideoFormat) -> CameraResult<u32> {
    Ok(u32::from_le_bytes(match format {
        VideoFormat::MJPEG => *b"MJPG",
        VideoFormat::YUYV => *b"YUYV",
        VideoFormat::UYVY => *b"UYVY",
        VideoFormat::NV12 => *b"NV12",
        VideoFormat::RGB => *b"RGB3",
        VideoFormat::Gray => *b"GREY",
        _ => {
            return Err(CameraError::UnsupportedFormat(format!(
                "V4L2 cannot decode {format:?}"
            )))
        }
    }))
}

fn invalid(message: &str) -> CameraError {
    CameraError::InvalidFormat(message.into())
}

fn check_rows(plane: &Plane<'_>, row_bytes: usize, height: usize) -> CameraResult<()> {
    let required = height
        .checked_sub(1)
        .and_then(|n| n.checked_mul(plane.stride))
        .and_then(|n| n.checked_add(row_bytes));
    if row_bytes == 0 || plane.stride < row_bytes || required.is_none_or(|n| plane.data.len() < n) {
        return Err(invalid("V4L2 frame has invalid stride or truncated data"));
    }
    Ok(())
}

pub(super) struct Converter {
    jpeg: Decompressor,
    coefficients: Option<[i32; 6]>,
}

impl Converter {
    pub fn new() -> CameraResult<Self> {
        Ok(Self {
            coefficients: None,
            jpeg: Decompressor::new().map_err(|e| invalid(&e.to_string()))?,
        })
    }

    // Preserve the negotiated YCbCr matrix and quantization. Common full-range
    // BT.601 frames keep using the shared SIMD converter.
    pub fn set_colorimetry(&mut self, matrix: u32, full: bool) -> CameraResult<()> {
        let (kr, kb): (f64, f64) = match matrix {
            1 | 3 | 5 => (0.299, 0.114),
            2 | 4 => (0.2126, 0.0722),
            6 => (0.2627, 0.0593),
            8 => (0.212, 0.087),
            _ => {
                return Err(CameraError::UnsupportedFormat(format!(
                    "V4L2 YCbCr encoding {matrix}"
                )))
            }
        };
        if full && matches!(matrix, 1 | 3 | 5) {
            self.coefficients = None;
            return Ok(());
        }
        let scale_y = if full { 1.0 } else { 255.0 / 219.0 };
        let scale_c = if full { 1.0 } else { 255.0 / 224.0 };
        let kg = 1.0 - kr - kb;
        self.coefficients = Some([
            (scale_y * 1024.0_f64).round() as i32,
            ((2.0 - 2.0 * kr) * scale_c * 1024.0).round() as i32,
            ((2.0 - 2.0 * kb) * kb / kg * scale_c * 1024.0).round() as i32,
            ((2.0 - 2.0 * kr) * kr / kg * scale_c * 1024.0).round() as i32,
            ((2.0 - 2.0 * kb) * scale_c * 1024.0).round() as i32,
            if full { 0 } else { 16 },
        ]);
        Ok(())
    }

    fn pixel(&self, y: u8, u: u8, v: u8, rgb: &mut [u8]) {
        let [cy, cr, cg_u, cg_v, cb, offset] = self.coefficients.unwrap();
        let y = (i32::from(y) - offset) * cy;
        let u = i32::from(u) - 128;
        let v = i32::from(v) - 128;
        rgb[0] = ((y + cr * v + 512) >> 10).clamp(0, 255) as u8;
        rgb[1] = ((y - cg_u * u - cg_v * v + 512) >> 10).clamp(0, 255) as u8;
        rgb[2] = ((y + cb * u + 512) >> 10).clamp(0, 255) as u8;
    }

    pub fn convert(
        &mut self,
        config: &CameraConfig,
        planes: &[Plane<'_>],
        rgb: &mut [u8],
    ) -> CameraResult<()> {
        config.validate()?;
        let (w, h) = (config.width as usize, config.height as usize);
        let length = w
            .checked_mul(h)
            .and_then(|n| n.checked_mul(3))
            .ok_or_else(|| invalid("V4L2 RGB dimensions overflow"))?;
        if rgb.len() < length {
            return Err(invalid("V4L2 RGB output is too short"));
        }
        let first = planes
            .first()
            .ok_or_else(|| invalid("V4L2 frame has no planes"))?;
        if planes.len() != 1 && !(config.format == VideoFormat::NV12 && planes.len() == 2) {
            return Err(invalid("Unexpected V4L2 plane count"));
        }
        match config.format {
            VideoFormat::MJPEG => {
                let header = self
                    .jpeg
                    .read_header(first.data)
                    .map_err(|e| invalid(&e.to_string()))?;
                if header.width != w || header.height != h {
                    return Err(invalid(
                        "MJPEG dimensions differ from negotiated V4L2 format",
                    ));
                }
                self.jpeg
                    .decompress(
                        first.data,
                        Image {
                            pixels: rgb,
                            width: w,
                            pitch: w * 3,
                            height: h,
                            format: PixelFormat::RGB,
                        },
                    )
                    .map_err(|e| invalid(&e.to_string()))?;
            }
            VideoFormat::NV12 => {
                let uv;
                let second = if planes.len() == 2 {
                    &planes[1]
                } else {
                    let offset = first
                        .stride
                        .checked_mul(h)
                        .ok_or_else(|| invalid("NV12 plane offset overflow"))?;
                    uv = Plane {
                        data: first
                            .data
                            .get(offset..)
                            .ok_or_else(|| invalid("Truncated NV12 Y plane"))?,
                        stride: first.stride,
                    };
                    &uv
                };
                check_rows(first, w, h)?;
                check_rows(second, w, h / 2)?;
                if self.coefficients.is_some() {
                    for row in 0..h {
                        for col in 0..w {
                            let chroma = (row / 2) * second.stride + (col / 2) * 2;
                            self.pixel(
                                first.data[row * first.stride + col],
                                second.data[chroma],
                                second.data[chroma + 1],
                                &mut rgb[(row * w + col) * 3..][..3],
                            );
                        }
                    }
                    return Ok(());
                }
                yuv420_to_rgb_into(
                    YuvPlane {
                        data: first.data,
                        row_stride: first.stride,
                        pixel_stride: 1,
                    },
                    YuvPlane {
                        data: second.data,
                        row_stride: second.stride,
                        pixel_stride: 2,
                    },
                    YuvPlane {
                        data: &second.data[1..],
                        row_stride: second.stride,
                        pixel_stride: 2,
                    },
                    w,
                    h,
                    rgb,
                )?;
            }
            VideoFormat::RGB | VideoFormat::Gray | VideoFormat::YUYV | VideoFormat::UYVY => {
                let row_bytes = w
                    .checked_mul(config.format.bytes_per_pixel().unwrap())
                    .ok_or_else(|| invalid("V4L2 row size overflow"))?;
                check_rows(first, row_bytes, h)?;
                for row in 0..h {
                    let input = &first.data[row * first.stride..][..row_bytes];
                    let output = &mut rgb[row * w * 3..][..w * 3];
                    if self.coefficients.is_some()
                        && matches!(config.format, VideoFormat::YUYV | VideoFormat::UYVY)
                    {
                        for (pair, out) in input
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .zip(output.as_chunks_mut::<6>().0.iter_mut())
                        {
                            let (y0, y1, u, v) = if config.format == VideoFormat::YUYV {
                                (pair[0], pair[2], pair[1], pair[3])
                            } else {
                                (pair[1], pair[3], pair[0], pair[2])
                            };
                            self.pixel(y0, u, v, &mut out[..3]);
                            self.pixel(y1, u, v, &mut out[3..]);
                        }
                        continue;
                    }
                    match config.format {
                        VideoFormat::RGB => output.copy_from_slice(input),
                        VideoFormat::Gray => {
                            for (pixel, &gray) in
                                output.as_chunks_mut::<3>().0.iter_mut().zip(input)
                            {
                                pixel.fill(gray);
                            }
                        }
                        VideoFormat::YUYV => yuyv_to_rgb_into(input, config.width, 1, output)?,
                        VideoFormat::UYVY => uyvy_to_rgb_into(input, config.width, 1, output)?,
                        _ => unreachable!(),
                    }
                }
            }
            other => {
                return Err(CameraError::UnsupportedFormat(format!(
                    "V4L2 cannot decode {other:?}"
                )))
            }
        }
        Ok(())
    }
}
