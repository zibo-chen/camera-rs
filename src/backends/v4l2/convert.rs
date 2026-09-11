use crate::utils::color_convert::{
    color_coefficients, yuv420sp_to_rgb_with_coefficients_into,
    yuv422_to_rgb_with_coefficients_into, Yuv420Sp,
};
use crate::{
    CameraConfig, CameraError, CameraResult, ColorInfo, ColorMatrix, ColorRange, VideoFormat,
};
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
    coefficients: [i32; 6],
}

impl Converter {
    pub fn new() -> CameraResult<Self> {
        Ok(Self {
            coefficients: color_coefficients(ColorInfo {
                matrix: ColorMatrix::Bt601,
                range: ColorRange::Full,
                ..Default::default()
            })?,
            jpeg: Decompressor::new().map_err(|e| invalid(&e.to_string()))?,
        })
    }

    // Preserve the negotiated YCbCr matrix and quantization while selecting the
    // shared configured SIMD kernel for every supported color mode.
    pub fn set_colorimetry(&mut self, matrix: u32, full: bool) -> CameraResult<()> {
        let matrix = match matrix {
            1 | 3 | 5 => ColorMatrix::Bt601,
            2 | 4 => ColorMatrix::Bt709,
            6 => ColorMatrix::Bt2020,
            8 => ColorMatrix::Smpte240M,
            _ => {
                return Err(CameraError::UnsupportedFormat(format!(
                    "V4L2 YCbCr encoding {matrix}"
                )))
            }
        };
        self.coefficients = color_coefficients(ColorInfo {
            matrix,
            range: if full {
                ColorRange::Full
            } else {
                ColorRange::Limited
            },
            ..Default::default()
        })?;
        Ok(())
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
                check_rows(second, w, h.div_ceil(2))?;
                yuv420sp_to_rgb_with_coefficients_into(
                    Yuv420Sp {
                        y: first.data,
                        uv: second.data,
                        y_stride: first.stride,
                        uv_stride: second.stride,
                        vu_order: false,
                    },
                    w,
                    h,
                    self.coefficients,
                    rgb,
                )?;
            }
            VideoFormat::YUYV | VideoFormat::UYVY => {
                let row_bytes = w
                    .checked_mul(config.format.bytes_per_pixel().unwrap())
                    .ok_or_else(|| invalid("V4L2 row size overflow"))?;
                check_rows(first, row_bytes, h)?;
                yuv422_to_rgb_with_coefficients_into(
                    first.data,
                    w,
                    h,
                    first.stride,
                    config.format == VideoFormat::UYVY,
                    self.coefficients,
                    rgb,
                )?;
            }
            VideoFormat::RGB | VideoFormat::Gray => {
                let row_bytes = w
                    .checked_mul(config.format.bytes_per_pixel().unwrap())
                    .ok_or_else(|| invalid("V4L2 row size overflow"))?;
                check_rows(first, row_bytes, h)?;
                for row in 0..h {
                    let input = &first.data[row * first.stride..][..row_bytes];
                    let output = &mut rgb[row * w * 3..][..w * 3];
                    match config.format {
                        VideoFormat::RGB => output.copy_from_slice(input),
                        VideoFormat::Gray => {
                            for (pixel, &gray) in
                                output.as_chunks_mut::<3>().0.iter_mut().zip(input)
                            {
                                pixel.fill(gray);
                            }
                        }
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
