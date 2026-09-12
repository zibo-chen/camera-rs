//! 高性能颜色空间转换模块
//!
//! 提供 YUYV/UYVY 到 RGB 的高效转换，支持：
//! - 标量实现（fallback）
//! - SIMD 优化实现（使用 portable_simd 或平台特定 intrinsics）
//!
//! ARM64 uses checked NEON conversion with scalar-equivalent rounding. x86_64
//! runtime-dispatches packed 4:2:2, planar YUV420 and NV12/NV21 full/half-size
//! rows to AVX2, and packed 8888 channel reordering to AVX2 or SSSE3; other
//! targets use scalar/compiler-vectorized loops. No parallel/GPU path is
//! enabled; use the benchmark on the target device instead of assuming a
//! speedup.

use crate::{
    error::{CameraError, Result},
    ColorInfo, ColorMatrix, ColorRange,
};

/// One Android YUV_420_888 plane. U and V may be disjoint (I420) or
/// overlapping (NV12/NV21). The final row need not include trailing padding.
#[derive(Clone, Copy)]
pub struct YuvPlane<'a> {
    pub data: &'a [u8],
    pub row_stride: usize,
    pub pixel_stride: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct Yuv420Sp<'a> {
    pub y: &'a [u8],
    pub uv: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
    pub vu_order: bool,
}

pub(crate) const BT601_FULL_COEFFICIENTS: [i32; 6] = [1024, 1436, 352, 731, 1815, 0];

impl YuvPlane<'_> {
    fn validate(&self, width: usize, height: usize) -> Result<()> {
        let row_bytes = width
            .checked_sub(1)
            .and_then(|n| n.checked_mul(self.pixel_stride))
            .and_then(|n| n.checked_add(1));
        let required = height
            .checked_sub(1)
            .and_then(|n| n.checked_mul(self.row_stride))
            .and_then(|n| n.checked_add(row_bytes?));
        if self.pixel_stride == 0
            || row_bytes.is_none_or(|n| self.row_stride < n)
            || required.is_none_or(|n| self.data.len() < n)
        {
            return Err(CameraError::InvalidFormat(
                "Invalid YUV plane stride or truncated data".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn color_coefficients(color: ColorInfo) -> Result<[i32; 6]> {
    if color.matrix == ColorMatrix::Bt601 && color.range == ColorRange::Full {
        return Ok(BT601_FULL_COEFFICIENTS);
    }
    let (kr, kb): (f64, f64) = match color.matrix {
        ColorMatrix::Bt601 => (0.299, 0.114),
        ColorMatrix::Bt709 => (0.2126, 0.0722),
        ColorMatrix::Bt2020 => (0.2627, 0.0593),
        ColorMatrix::Smpte240M => (0.212, 0.087),
        ColorMatrix::Unknown => {
            return Err(CameraError::UnsupportedFormat(
                "YUV color matrix is unknown; supply a color override".into(),
            ));
        }
    };
    let (sy, sc, offset) = match color.range {
        ColorRange::Full => (1.0, 1.0, 0),
        ColorRange::Limited => (255.0 / 219.0, 255.0 / 224.0, 16),
        ColorRange::Unknown => {
            return Err(CameraError::UnsupportedFormat(
                "YUV quantization range is unknown; supply a color override".into(),
            ));
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

#[inline(always)]
pub(crate) fn yuv_pixel(
    [cy, cr, gu, gv, cb, offset]: [i32; 6],
    y: u8,
    u: u8,
    v: u8,
    out: &mut [u8],
) {
    let y = (y as i32 - offset) * cy;
    let u = u as i32 - 128;
    let v = v as i32 - 128;
    out[0] = ((y + cr * v + 512) >> 10).clamp(0, 255) as u8;
    out[1] = ((y - gu * u - gv * v + 512) >> 10).clamp(0, 255) as u8;
    out[2] = ((y + cb * u + 512) >> 10).clamp(0, 255) as u8;
}

pub(crate) fn yuv420_to_rgb_with_coefficients_into(
    y: YuvPlane<'_>,
    u: YuvPlane<'_>,
    v: YuvPlane<'_>,
    width: usize,
    height: usize,
    coefficients: [i32; 6],
    rgb: &mut [u8],
) -> Result<()> {
    y.validate(width, height)?;
    u.validate(width.div_ceil(2), height.div_ceil(2))?;
    v.validate(width.div_ceil(2), height.div_ceil(2))?;
    let length = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if rgb.len() < length {
        return Err(CameraError::InvalidFormat("RGB buffer too short".into()));
    }
    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    if y.pixel_stride == 1 && u.pixel_stride == 1 && v.pixel_stride == 1 {
        let rgb_row_bytes = width * 3;
        let chroma_width = width.div_ceil(2);
        let paired_rows = height / 2 * 2;
        for row in (0..paired_rows).step_by(2) {
            let y0 = &y.data[row * y.row_stride..][..width];
            let y1 = &y.data[(row + 1) * y.row_stride..][..width];
            let chroma_row = row / 2;
            let u_row = &u.data[chroma_row * u.row_stride..][..chroma_width];
            let v_row = &v.data[chroma_row * v.row_stride..][..chroma_width];
            let rgb_offset = row * rgb_row_bytes;
            let (before, after) = rgb.split_at_mut(rgb_offset + rgb_row_bytes);
            let rgb0 = &mut before[rgb_offset..][..rgb_row_bytes];
            let rgb1 = &mut after[..rgb_row_bytes];
            let start = unsafe {
                neon::planar_contiguous_two_rows_with_coefficients(
                    y0,
                    y1,
                    u_row,
                    v_row,
                    coefficients,
                    rgb0,
                    rgb1,
                )
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2;
                for (luma, output) in [(y0, &mut *rgb0), (y1, &mut *rgb1)] {
                    for x in column..(column + 2).min(width) {
                        yuv_pixel(
                            coefficients,
                            luma[x],
                            u_row[chroma],
                            v_row[chroma],
                            &mut output[x * 3..x * 3 + 3],
                        );
                    }
                }
            }
        }
        if paired_rows != height {
            let row = paired_rows;
            let y_row = &y.data[row * y.row_stride..][..width];
            let chroma_row = row / 2;
            let u_row = &u.data[chroma_row * u.row_stride..][..chroma_width];
            let v_row = &v.data[chroma_row * v.row_stride..][..chroma_width];
            let rgb_row = &mut rgb[row * rgb_row_bytes..][..rgb_row_bytes];
            let start = unsafe {
                neon::planar_contiguous_row_with_coefficients(
                    y_row,
                    u_row,
                    v_row,
                    coefficients,
                    rgb_row,
                )
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2;
                for x in column..(column + 2).min(width) {
                    yuv_pixel(
                        coefficients,
                        y_row[x],
                        u_row[chroma],
                        v_row[chroma],
                        &mut rgb_row[x * 3..x * 3 + 3],
                    );
                }
            }
        }
        return Ok(());
    }
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    #[cfg(target_arch = "x86_64")]
    if y.pixel_stride == 1 && u.pixel_stride == 1 && v.pixel_stride == 1 && avx2 {
        let rgb_row_bytes = width * 3;
        let chroma_width = width.div_ceil(2);
        let paired_rows = height / 2 * 2;
        for row in (0..paired_rows).step_by(2) {
            let y0 = &y.data[row * y.row_stride..][..width];
            let y1 = &y.data[(row + 1) * y.row_stride..][..width];
            let chroma_row = row / 2;
            let u_row = &u.data[chroma_row * u.row_stride..][..chroma_width];
            let v_row = &v.data[chroma_row * v.row_stride..][..chroma_width];
            let rgb_offset = row * rgb_row_bytes;
            let (before, after) = rgb.split_at_mut(rgb_offset + rgb_row_bytes);
            let rgb0 = &mut before[rgb_offset..][..rgb_row_bytes];
            let rgb1 = &mut after[..rgb_row_bytes];
            let start = unsafe {
                x86::planar_two_rows_avx2::<10, 512, 512>(
                    y0,
                    y1,
                    u_row,
                    v_row,
                    rgb0,
                    rgb1,
                    coefficients,
                )
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2;
                for (luma, output) in [(y0, &mut *rgb0), (y1, &mut *rgb1)] {
                    for x in column..(column + 2).min(width) {
                        yuv_pixel(
                            coefficients,
                            luma[x],
                            u_row[chroma],
                            v_row[chroma],
                            &mut output[x * 3..x * 3 + 3],
                        );
                    }
                }
            }
        }
        if paired_rows != height {
            let row = paired_rows;
            let y_row = &y.data[row * y.row_stride..][..width];
            let u_row = &u.data[(row / 2) * u.row_stride..][..chroma_width];
            let v_row = &v.data[(row / 2) * v.row_stride..][..chroma_width];
            let rgb_row = &mut rgb[row * rgb_row_bytes..][..rgb_row_bytes];
            let start = unsafe {
                x86::planar_row_avx2::<10, 512, 512>(y_row, u_row, v_row, rgb_row, coefficients)
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2;
                for x in column..(column + 2).min(width) {
                    yuv_pixel(
                        coefficients,
                        y_row[x],
                        u_row[chroma],
                        v_row[chroma],
                        &mut rgb_row[x * 3..x * 3 + 3],
                    );
                }
            }
        }
        return Ok(());
    }
    for row in 0..height {
        #[cfg(any(
            all(target_arch = "aarch64", target_feature = "neon"),
            all(target_arch = "arm", target_feature = "neon")
        ))]
        let start = if y.pixel_stride == 1 {
            unsafe {
                if u.pixel_stride == 1 && v.pixel_stride == 1 {
                    neon::planar_contiguous_row_with_coefficients(
                        &y.data[row * y.row_stride..][..width],
                        &u.data[(row / 2) * u.row_stride..][..width.div_ceil(2)],
                        &v.data[(row / 2) * v.row_stride..][..width.div_ceil(2)],
                        coefficients,
                        &mut rgb[row * width * 3..][..width * 3],
                    )
                } else {
                    neon::planar_row_with_coefficients(
                        &y.data[row * y.row_stride..][..width],
                        &u.data[(row / 2) * u.row_stride..],
                        &v.data[(row / 2) * v.row_stride..],
                        (u.pixel_stride, v.pixel_stride),
                        coefficients,
                        &mut rgb[row * width * 3..][..width * 3],
                    )
                }
            }
        } else {
            0
        };
        #[cfg(target_arch = "x86_64")]
        let start = if y.pixel_stride == 1 && u.pixel_stride == 1 && v.pixel_stride == 1 && avx2 {
            unsafe {
                x86::planar_row_avx2::<10, 512, 512>(
                    &y.data[row * y.row_stride..][..width],
                    &u.data[(row / 2) * u.row_stride..][..width.div_ceil(2)],
                    &v.data[(row / 2) * v.row_stride..][..width.div_ceil(2)],
                    &mut rgb[row * width * 3..][..width * 3],
                    coefficients,
                )
            }
        } else {
            0
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            not(target_arch = "x86_64")
        ))]
        let start = 0;
        for column in (start..width).step_by(2) {
            let u = u.data[(row / 2) * u.row_stride + (column / 2) * u.pixel_stride];
            let v = v.data[(row / 2) * v.row_stride + (column / 2) * v.pixel_stride];
            for x in column..(column + 2).min(width) {
                yuv_pixel(
                    coefficients,
                    y.data[row * y.row_stride + x * y.pixel_stride],
                    u,
                    v,
                    &mut rgb[(row * width + x) * 3..][..3],
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn yuv420sp_to_rgb_with_coefficients_into(
    planes: Yuv420Sp<'_>,
    width: usize,
    height: usize,
    coefficients: [i32; 6],
    rgb: &mut [u8],
) -> Result<()> {
    let Yuv420Sp {
        y: y_plane,
        uv: uv_plane,
        y_stride,
        uv_stride,
        vu_order,
    } = planes;
    #[cfg(all(
        not(all(target_arch = "aarch64", target_feature = "neon")),
        target_arch = "x86_64"
    ))]
    if coefficients == BT601_FULL_COEFFICIENTS
        && !vu_order
        && !std::arch::is_x86_feature_detected!("avx2")
    {
        return yuv420sp_bt601_full_to_rgb_into(
            y_plane, uv_plane, width, height, y_stride, uv_stride, rgb,
        );
    }
    #[cfg(all(
        not(all(target_arch = "aarch64", target_feature = "neon")),
        not(target_arch = "x86_64")
    ))]
    if coefficients == BT601_FULL_COEFFICIENTS && !vu_order {
        return yuv420sp_bt601_full_to_rgb_into(
            y_plane, uv_plane, width, height, y_stride, uv_stride, rgb,
        );
    }
    YuvPlane {
        data: y_plane,
        row_stride: y_stride,
        pixel_stride: 1,
    }
    .validate(width, height)?;
    let chroma_row_bytes = width
        .div_ceil(2)
        .checked_mul(2)
        .ok_or_else(|| CameraError::InvalidFormat("YUV420 size overflow".into()))?;
    let chroma_rows = height.div_ceil(2);
    let required_uv = (chroma_rows - 1)
        .checked_mul(uv_stride)
        .and_then(|bytes| bytes.checked_add(chroma_row_bytes))
        .ok_or_else(|| CameraError::InvalidFormat("YUV420 size overflow".into()))?;
    let required_rgb = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if uv_stride < chroma_row_bytes || uv_plane.len() < required_uv || rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "invalid YUV420 UV stride or buffer size".into(),
        ));
    }
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    #[cfg(target_arch = "x86_64")]
    if avx2 {
        let rgb_row_bytes = width * 3;
        let paired_rows = height / 2 * 2;
        for row in (0..paired_rows).step_by(2) {
            let y0 = &y_plane[row * y_stride..][..width];
            let y1 = &y_plane[(row + 1) * y_stride..][..width];
            let uv_row = &uv_plane[(row / 2) * uv_stride..][..chroma_row_bytes];
            let rgb_offset = row * rgb_row_bytes;
            let (before, after) = rgb.split_at_mut(rgb_offset + rgb_row_bytes);
            let rgb0 = &mut before[rgb_offset..][..rgb_row_bytes];
            let rgb1 = &mut after[..rgb_row_bytes];
            let start = unsafe {
                if vu_order {
                    x86::semiplanar_two_rows_avx2::<true, 10, 512, 512>(
                        y0,
                        y1,
                        uv_row,
                        rgb0,
                        rgb1,
                        coefficients,
                    )
                } else {
                    x86::semiplanar_two_rows_avx2::<false, 10, 512, 512>(
                        y0,
                        y1,
                        uv_row,
                        rgb0,
                        rgb1,
                        coefficients,
                    )
                }
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2 * 2;
                let (u, v) = if vu_order {
                    (uv_row[chroma + 1], uv_row[chroma])
                } else {
                    (uv_row[chroma], uv_row[chroma + 1])
                };
                for x in column..(column + 2).min(width) {
                    yuv_pixel(coefficients, y0[x], u, v, &mut rgb0[x * 3..x * 3 + 3]);
                    yuv_pixel(coefficients, y1[x], u, v, &mut rgb1[x * 3..x * 3 + 3]);
                }
            }
        }
        if paired_rows != height {
            let row = paired_rows;
            let y_row = &y_plane[row * y_stride..][..width];
            let uv_row = &uv_plane[(row / 2) * uv_stride..][..chroma_row_bytes];
            let rgb_row = &mut rgb[row * rgb_row_bytes..][..rgb_row_bytes];
            let start = unsafe {
                if vu_order {
                    x86::semiplanar_row_avx2::<true, 10, 512, 512>(
                        y_row,
                        uv_row,
                        rgb_row,
                        coefficients,
                    )
                } else {
                    x86::semiplanar_row_avx2::<false, 10, 512, 512>(
                        y_row,
                        uv_row,
                        rgb_row,
                        coefficients,
                    )
                }
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2 * 2;
                let (u, v) = if vu_order {
                    (uv_row[chroma + 1], uv_row[chroma])
                } else {
                    (uv_row[chroma], uv_row[chroma + 1])
                };
                for x in column..(column + 2).min(width) {
                    yuv_pixel(coefficients, y_row[x], u, v, &mut rgb_row[x * 3..x * 3 + 3]);
                }
            }
        }
        return Ok(());
    }
    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    {
        let rgb_row_bytes = width * 3;
        let paired_rows = height / 2 * 2;
        for row in (0..paired_rows).step_by(2) {
            let y0 = &y_plane[row * y_stride..][..width];
            let y1 = &y_plane[(row + 1) * y_stride..][..width];
            let uv_row = &uv_plane[(row / 2) * uv_stride..][..chroma_row_bytes];
            let rgb_offset = row * rgb_row_bytes;
            let (before, after) = rgb.split_at_mut(rgb_offset + rgb_row_bytes);
            let rgb0 = &mut before[rgb_offset..][..rgb_row_bytes];
            let rgb1 = &mut after[..rgb_row_bytes];
            let start = unsafe {
                neon::semiplanar_two_rows_with_coefficients(
                    y0,
                    y1,
                    uv_row,
                    vu_order,
                    coefficients,
                    rgb0,
                    rgb1,
                )
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2 * 2;
                let (u, v) = if vu_order {
                    (uv_row[chroma + 1], uv_row[chroma])
                } else {
                    (uv_row[chroma], uv_row[chroma + 1])
                };
                for x in column..(column + 2).min(width) {
                    yuv_pixel(coefficients, y0[x], u, v, &mut rgb0[x * 3..x * 3 + 3]);
                    yuv_pixel(coefficients, y1[x], u, v, &mut rgb1[x * 3..x * 3 + 3]);
                }
            }
        }
        if paired_rows != height {
            let row = paired_rows;
            let y_row = &y_plane[row * y_stride..][..width];
            let uv_row = &uv_plane[(row / 2) * uv_stride..][..chroma_row_bytes];
            let rgb_row = &mut rgb[row * rgb_row_bytes..][..rgb_row_bytes];
            let start = unsafe {
                neon::semiplanar_row_with_coefficients(
                    y_row,
                    uv_row,
                    vu_order,
                    coefficients,
                    rgb_row,
                )
            };
            for column in (start..width).step_by(2) {
                let chroma = column / 2 * 2;
                let (u, v) = if vu_order {
                    (uv_row[chroma + 1], uv_row[chroma])
                } else {
                    (uv_row[chroma], uv_row[chroma + 1])
                };
                for x in column..(column + 2).min(width) {
                    yuv_pixel(coefficients, y_row[x], u, v, &mut rgb_row[x * 3..x * 3 + 3]);
                }
            }
        }
        Ok(())
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    )))]
    for row in 0..height {
        let y_row = &y_plane[row * y_stride..][..width];
        let uv_row = &uv_plane[(row / 2) * uv_stride..][..chroma_row_bytes];
        let rgb_row = &mut rgb[row * width * 3..][..width * 3];
        #[cfg(target_arch = "x86_64")]
        let start = if avx2 {
            unsafe {
                if vu_order {
                    x86::semiplanar_row_avx2::<true, 10, 512, 512>(
                        y_row,
                        uv_row,
                        rgb_row,
                        coefficients,
                    )
                } else {
                    x86::semiplanar_row_avx2::<false, 10, 512, 512>(
                        y_row,
                        uv_row,
                        rgb_row,
                        coefficients,
                    )
                }
            }
        } else {
            0
        };
        #[cfg(not(target_arch = "x86_64"))]
        let start = 0;
        for column in (start..width).step_by(2) {
            let chroma = (column / 2) * 2;
            let (u, v) = if vu_order {
                (uv_row[chroma + 1], uv_row[chroma])
            } else {
                (uv_row[chroma], uv_row[chroma + 1])
            };
            for x in column..(column + 2).min(width) {
                yuv_pixel(coefficients, y_row[x], u, v, &mut rgb_row[x * 3..x * 3 + 3]);
            }
        }
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    )))]
    Ok(())
}

pub(crate) fn yuv420sp_to_rgb_half_with_coefficients_into(
    planes: Yuv420Sp<'_>,
    width: usize,
    height: usize,
    coefficients: [i32; 6],
    rgb: &mut [u8],
) -> Result<()> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(CameraError::InvalidFormat(
            "half-size YUV420 conversion requires positive even dimensions".into(),
        ));
    }
    YuvPlane {
        data: planes.y,
        row_stride: planes.y_stride,
        pixel_stride: 1,
    }
    .validate(width, height)?;
    let chroma_rows = height / 2;
    let required_uv = (chroma_rows - 1)
        .checked_mul(planes.uv_stride)
        .and_then(|bytes| bytes.checked_add(width))
        .ok_or_else(|| CameraError::InvalidFormat("YUV420 size overflow".into()))?;
    let output_width = width / 2;
    let output_height = height / 2;
    let required_rgb = output_width
        .checked_mul(output_height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if planes.uv_stride < width || planes.uv.len() < required_uv || rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "invalid YUV420 UV stride or half-size RGB buffer".into(),
        ));
    }
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    for row in 0..output_height {
        let y_row = &planes.y[row * 2 * planes.y_stride..][..width];
        let uv_row = &planes.uv[row * planes.uv_stride..][..width];
        let rgb_row = &mut rgb[row * output_width * 3..][..output_width * 3];
        #[cfg(any(
            all(target_arch = "aarch64", target_feature = "neon"),
            all(target_arch = "arm", target_feature = "neon")
        ))]
        let start = unsafe {
            neon::semiplanar_half_row_with_coefficients(
                y_row,
                uv_row,
                planes.vu_order,
                coefficients,
                rgb_row,
            )
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            target_arch = "x86_64"
        ))]
        let start = if avx2 {
            unsafe {
                if planes.vu_order {
                    x86::semiplanar_half_row_avx2::<true, 10, 512, 512>(
                        y_row,
                        uv_row,
                        rgb_row,
                        coefficients,
                    )
                } else {
                    x86::semiplanar_half_row_avx2::<false, 10, 512, 512>(
                        y_row,
                        uv_row,
                        rgb_row,
                        coefficients,
                    )
                }
            }
        } else {
            0
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            not(target_arch = "x86_64")
        ))]
        let start = 0;
        for column in start..output_width {
            let (u, v) = if planes.vu_order {
                (uv_row[column * 2 + 1], uv_row[column * 2])
            } else {
                (uv_row[column * 2], uv_row[column * 2 + 1])
            };
            yuv_pixel(
                coefficients,
                y_row[column * 2],
                u,
                v,
                &mut rgb_row[column * 3..column * 3 + 3],
            );
        }
    }
    Ok(())
}

pub(crate) fn yuv420_to_rgb_half_with_coefficients_into(
    y: YuvPlane<'_>,
    u: YuvPlane<'_>,
    v: YuvPlane<'_>,
    width: usize,
    height: usize,
    coefficients: [i32; 6],
    rgb: &mut [u8],
) -> Result<()> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(CameraError::InvalidFormat(
            "half-size YUV420 conversion requires positive even dimensions".into(),
        ));
    }
    y.validate(width, height)?;
    u.validate(width / 2, height / 2)?;
    v.validate(width / 2, height / 2)?;
    if y.pixel_stride != 1 || u.pixel_stride != 1 || v.pixel_stride != 1 {
        return Err(CameraError::InvalidFormat(
            "direct half-size planar YUV requires contiguous pixels".into(),
        ));
    }
    let output_width = width / 2;
    let output_height = height / 2;
    let required_rgb = output_width
        .checked_mul(output_height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "half-size RGB buffer is too small".into(),
        ));
    }
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    for row in 0..output_height {
        let y_row = &y.data[row * 2 * y.row_stride..][..width];
        let u_row = &u.data[row * u.row_stride..][..output_width];
        let v_row = &v.data[row * v.row_stride..][..output_width];
        let rgb_row = &mut rgb[row * output_width * 3..][..output_width * 3];
        #[cfg(any(
            all(target_arch = "aarch64", target_feature = "neon"),
            all(target_arch = "arm", target_feature = "neon")
        ))]
        let start = unsafe {
            neon::planar_half_row_with_coefficients(y_row, u_row, v_row, coefficients, rgb_row)
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            target_arch = "x86_64"
        ))]
        let start = if avx2 {
            unsafe { x86::planar_half_row_avx2(y_row, u_row, v_row, rgb_row, coefficients) }
        } else {
            0
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            not(target_arch = "x86_64")
        ))]
        let start = 0;
        for column in start..output_width {
            yuv_pixel(
                coefficients,
                y_row[column * 2],
                u_row[column],
                v_row[column],
                &mut rgb_row[column * 3..column * 3 + 3],
            );
        }
    }
    Ok(())
}

pub(crate) fn yuv422_to_rgb_half_with_coefficients_into(
    source: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    uyvy: bool,
    coefficients: [i32; 6],
    rgb: &mut [u8],
) -> Result<()> {
    let source_row_bytes = width
        .checked_mul(2)
        .ok_or_else(|| CameraError::InvalidFormat("YUV422 size overflow".into()))?;
    let output_width = width / 2;
    let output_height = height / 2;
    let rgb_row_bytes = output_width
        .checked_mul(3)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || stride < source_row_bytes
    {
        return Err(CameraError::InvalidFormat(
            "invalid half-size YUV422 dimensions or stride".into(),
        ));
    }
    let required_source = (height - 1)
        .checked_mul(stride)
        .and_then(|bytes| bytes.checked_add(source_row_bytes))
        .ok_or_else(|| CameraError::InvalidFormat("YUV422 size overflow".into()))?;
    let required_rgb = output_height
        .checked_mul(rgb_row_bytes)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if source.len() < required_source || rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "YUV422 or half-size RGB buffer is too small".into(),
        ));
    }
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    for row in 0..output_height {
        let source_row = &source[row * 2 * stride..][..source_row_bytes];
        let rgb_row = &mut rgb[row * rgb_row_bytes..][..rgb_row_bytes];
        #[cfg(any(
            all(target_arch = "aarch64", target_feature = "neon"),
            all(target_arch = "arm", target_feature = "neon")
        ))]
        let start = unsafe {
            neon::packed422_half_row_with_coefficients(source_row, uyvy, coefficients, rgb_row)
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            target_arch = "x86_64"
        ))]
        let start = if avx2 {
            unsafe {
                if uyvy {
                    x86::packed422_half_row_avx2::<true>(source_row, rgb_row, coefficients)
                } else {
                    x86::packed422_half_row_avx2::<false>(source_row, rgb_row, coefficients)
                }
            }
        } else {
            0
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            not(target_arch = "x86_64")
        ))]
        let start = 0;
        for column in start..output_width {
            let input = column * 4;
            let (y, u, v) = if uyvy {
                (
                    source_row[input + 1],
                    source_row[input],
                    source_row[input + 2],
                )
            } else {
                (
                    source_row[input],
                    source_row[input + 1],
                    source_row[input + 3],
                )
            };
            yuv_pixel(
                coefficients,
                y,
                u,
                v,
                &mut rgb_row[column * 3..column * 3 + 3],
            );
        }
    }
    Ok(())
}

pub(crate) fn yuv422_to_rgb_with_coefficients_into(
    source: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    uyvy: bool,
    coefficients: [i32; 6],
    rgb: &mut [u8],
) -> Result<()> {
    let source_row_bytes = width
        .checked_mul(2)
        .ok_or_else(|| CameraError::InvalidFormat("YUV422 size overflow".into()))?;
    let rgb_row_bytes = width
        .checked_mul(3)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || stride < source_row_bytes {
        return Err(CameraError::InvalidFormat(
            "invalid YUV422 dimensions or stride".into(),
        ));
    }
    let required_source = (height - 1)
        .checked_mul(stride)
        .and_then(|bytes| bytes.checked_add(source_row_bytes))
        .ok_or_else(|| CameraError::InvalidFormat("YUV422 size overflow".into()))?;
    let required_rgb = height
        .checked_mul(rgb_row_bytes)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if source.len() < required_source || rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "YUV422 or RGB buffer is too small".into(),
        ));
    }
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    for row in 0..height {
        let source_row = &source[row * stride..][..source_row_bytes];
        let rgb_row = &mut rgb[row * rgb_row_bytes..][..rgb_row_bytes];
        #[cfg(any(
            all(target_arch = "aarch64", target_feature = "neon"),
            all(target_arch = "arm", target_feature = "neon")
        ))]
        let start = unsafe {
            neon::packed422_row_with_coefficients(source_row, uyvy, coefficients, rgb_row)
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            target_arch = "x86_64"
        ))]
        let start = if avx2 {
            unsafe {
                if uyvy {
                    x86::packed422_avx2::<true, 10, 512, 512>(source_row, rgb_row, coefficients);
                } else {
                    x86::packed422_avx2::<false, 10, 512, 512>(source_row, rgb_row, coefficients);
                }
            }
            width
        } else {
            0
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            not(target_arch = "x86_64")
        ))]
        let start = 0;
        for column in (start..width).step_by(2) {
            let input = column * 2;
            let output = column * 3;
            let (y0, u, y1, v) = if uyvy {
                (
                    source_row[input + 1],
                    source_row[input],
                    source_row[input + 3],
                    source_row[input + 2],
                )
            } else {
                (
                    source_row[input],
                    source_row[input + 1],
                    source_row[input + 2],
                    source_row[input + 3],
                )
            };
            yuv_pixel(coefficients, y0, u, v, &mut rgb_row[output..output + 3]);
            yuv_pixel(coefficients, y1, u, v, &mut rgb_row[output + 3..output + 6]);
        }
    }
    Ok(())
}

pub fn yuv420_to_rgb_into(
    y: YuvPlane<'_>,
    u: YuvPlane<'_>,
    v: YuvPlane<'_>,
    width: usize,
    height: usize,
    rgb: &mut [u8],
) -> Result<()> {
    yuv420_to_rgb_with_coefficients_into(y, u, v, width, height, BT601_FULL_COEFFICIENTS, rgb)
}

// ============================================================================
// 常量定义
// ============================================================================

/// YUV 到 RGB 转换系数（BT.601 标准）
/// 使用定点数运算，精度为 8 位小数
mod yuv_coefficients {
    // 原始公式：
    // R = Y + 1.402 * (V - 128)
    // G = Y - 0.344136 * (U - 128) - 0.714136 * (V - 128)
    // B = Y + 1.772 * (U - 128)
    //
    // 转换为定点数（乘以 256）：
    pub const V_TO_R: i32 = 359; // 1.402 * 256 ≈ 359
    pub const U_TO_G: i32 = 88; // 0.344136 * 256 ≈ 88
    pub const V_TO_G: i32 = 183; // 0.714136 * 256 ≈ 183
    pub const U_TO_B: i32 = 454; // 1.772 * 256 ≈ 454
}

// ============================================================================
// 颜色转换器配置
// ============================================================================

/// 颜色转换选项
#[derive(Debug, Clone, Copy)]
pub struct ColorConvertOptions {
    /// 是否使用 SIMD 加速（如果可用）
    pub use_simd: bool,
}

impl Default for ColorConvertOptions {
    fn default() -> Self {
        Self { use_simd: true }
    }
}

/// 颜色转换器
#[derive(Debug, Clone)]
pub struct ColorConverter {
    options: ColorConvertOptions,
}

impl Default for ColorConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorConverter {
    /// 创建默认配置的转换器
    pub fn new() -> Self {
        Self {
            options: ColorConvertOptions::default(),
        }
    }

    /// 使用自定义选项创建转换器
    pub fn with_options(options: ColorConvertOptions) -> Self {
        Self { options }
    }

    /// YUYV 转 RGB
    ///
    /// YUYV 格式（也称为 YUY2）：每 4 字节表示 2 个像素
    /// [Y0, U, Y1, V] -> [R0, G0, B0], [R1, G1, B1]
    pub fn yuyv_to_rgb(&self, yuyv_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        let pixel_count = checked_packed_422_pixel_count(width, height, "YUYV")?;
        let expected_yuyv_size = checked_frame_size(pixel_count, 2, "YUYV")?;
        let expected_rgb_size = checked_frame_size(pixel_count, 3, "RGB")?;

        if yuyv_data.len() < expected_yuyv_size {
            return Err(CameraError::InvalidFormat(format!(
                "YUYV data size mismatch: expected {}, got {}",
                expected_yuyv_size,
                yuyv_data.len()
            )));
        }

        let mut rgb = vec![0u8; expected_rgb_size];
        let yuyv_data = &yuyv_data[..expected_yuyv_size];
        if self.options.use_simd {
            yuyv_to_rgb_simd_into(yuyv_data, &mut rgb)?;
        } else {
            yuyv_to_rgb_scalar_into(yuyv_data, &mut rgb)?;
        }
        Ok(rgb)
    }

    /// UYVY 转 RGB
    ///
    /// UYVY 格式：每 4 字节表示 2 个像素
    /// [U, Y0, V, Y1] -> [R0, G0, B0], [R1, G1, B1]
    pub fn uyvy_to_rgb(&self, uyvy_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        let pixel_count = checked_packed_422_pixel_count(width, height, "UYVY")?;
        let expected_uyvy_size = checked_frame_size(pixel_count, 2, "UYVY")?;
        let expected_rgb_size = checked_frame_size(pixel_count, 3, "RGB")?;

        if uyvy_data.len() < expected_uyvy_size {
            return Err(CameraError::InvalidFormat(format!(
                "UYVY data size mismatch: expected {}, got {}",
                expected_uyvy_size,
                uyvy_data.len()
            )));
        }

        let mut rgb = vec![0u8; expected_rgb_size];
        let uyvy_data = &uyvy_data[..expected_uyvy_size];
        if self.options.use_simd {
            uyvy_to_rgb_simd_into(uyvy_data, &mut rgb)?;
        } else {
            uyvy_to_rgb_scalar_into(uyvy_data, &mut rgb)?;
        }
        Ok(rgb)
    }

    /// YUYV 转 RGB（使用预分配缓冲区）
    ///
    /// 这个方法避免了内存分配，适合高性能场景
    pub fn yuyv_to_rgb_into(
        &self,
        yuyv_data: &[u8],
        width: u32,
        height: u32,
        rgb_buffer: &mut [u8],
    ) -> Result<()> {
        let pixel_count = checked_packed_422_pixel_count(width, height, "YUYV")?;
        let expected_yuyv_size = checked_frame_size(pixel_count, 2, "YUYV")?;
        let expected_rgb_size = checked_frame_size(pixel_count, 3, "RGB")?;

        if yuyv_data.len() < expected_yuyv_size {
            return Err(CameraError::InvalidFormat(format!(
                "YUYV data size mismatch: expected {}, got {}",
                expected_yuyv_size,
                yuyv_data.len()
            )));
        }

        if rgb_buffer.len() < expected_rgb_size {
            return Err(CameraError::InvalidFormat(format!(
                "RGB buffer too small: expected {}, got {}",
                expected_rgb_size,
                rgb_buffer.len()
            )));
        }

        let yuyv_data = &yuyv_data[..expected_yuyv_size];
        let rgb_buffer = &mut rgb_buffer[..expected_rgb_size];
        if self.options.use_simd {
            yuyv_to_rgb_simd_into(yuyv_data, rgb_buffer)
        } else {
            yuyv_to_rgb_scalar_into(yuyv_data, rgb_buffer)
        }
    }

    /// UYVY 转 RGB（使用预分配缓冲区）
    pub fn uyvy_to_rgb_into(
        &self,
        uyvy_data: &[u8],
        width: u32,
        height: u32,
        rgb_buffer: &mut [u8],
    ) -> Result<()> {
        let pixel_count = checked_packed_422_pixel_count(width, height, "UYVY")?;
        let expected_uyvy_size = checked_frame_size(pixel_count, 2, "UYVY")?;
        let expected_rgb_size = checked_frame_size(pixel_count, 3, "RGB")?;

        if uyvy_data.len() < expected_uyvy_size {
            return Err(CameraError::InvalidFormat(format!(
                "UYVY data size mismatch: expected {}, got {}",
                expected_uyvy_size,
                uyvy_data.len()
            )));
        }

        if rgb_buffer.len() < expected_rgb_size {
            return Err(CameraError::InvalidFormat(format!(
                "RGB buffer too small: expected {}, got {}",
                expected_rgb_size,
                rgb_buffer.len()
            )));
        }

        let uyvy_data = &uyvy_data[..expected_uyvy_size];
        let rgb_buffer = &mut rgb_buffer[..expected_rgb_size];
        if self.options.use_simd {
            uyvy_to_rgb_simd_into(uyvy_data, rgb_buffer)
        } else {
            uyvy_to_rgb_scalar_into(uyvy_data, rgb_buffer)
        }
    }
}

// ============================================================================
// 标量实现（Fallback）
// ============================================================================

/// 标量 YUYV 转 RGB
#[cfg(test)]
fn yuyv_to_rgb_scalar(yuyv_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let pixel_count = checked_packed_422_pixel_count(width, height, "YUYV")?;
    let rgb_size = checked_frame_size(pixel_count, 3, "RGB")?;
    let mut rgb = vec![0u8; rgb_size];
    yuyv_to_rgb_scalar_into(yuyv_data, &mut rgb)?;
    Ok(rgb)
}

/// 标量 YUYV 转 RGB（使用预分配缓冲区）
fn yuyv_to_rgb_scalar_into(yuyv_data: &[u8], rgb_buffer: &mut [u8]) -> Result<()> {
    use yuv_coefficients::*;

    let mut rgb_idx = 0;
    let mut yuyv_idx = 0;

    while yuyv_idx + 3 < yuyv_data.len() && rgb_idx + 5 < rgb_buffer.len() {
        let y0 = yuyv_data[yuyv_idx] as i32;
        let u = yuyv_data[yuyv_idx + 1] as i32 - 128;
        let y1 = yuyv_data[yuyv_idx + 2] as i32;
        let v = yuyv_data[yuyv_idx + 3] as i32 - 128;

        // 预计算共享的 UV 分量
        let v_r = (V_TO_R * v) >> 8;
        let uv_g = (U_TO_G * u + V_TO_G * v) >> 8;
        let u_b = (U_TO_B * u) >> 8;

        // 第一个像素
        rgb_buffer[rgb_idx] = (y0 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 1] = (y0 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 2] = (y0 + u_b).clamp(0, 255) as u8;

        // 第二个像素
        rgb_buffer[rgb_idx + 3] = (y1 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 4] = (y1 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 5] = (y1 + u_b).clamp(0, 255) as u8;

        yuyv_idx += 4;
        rgb_idx += 6;
    }

    Ok(())
}

/// 标量 UYVY 转 RGB（使用预分配缓冲区）
fn uyvy_to_rgb_scalar_into(uyvy_data: &[u8], rgb_buffer: &mut [u8]) -> Result<()> {
    use yuv_coefficients::*;

    let mut rgb_idx = 0;
    let mut uyvy_idx = 0;

    while uyvy_idx + 3 < uyvy_data.len() && rgb_idx + 5 < rgb_buffer.len() {
        let u = uyvy_data[uyvy_idx] as i32 - 128;
        let y0 = uyvy_data[uyvy_idx + 1] as i32;
        let v = uyvy_data[uyvy_idx + 2] as i32 - 128;
        let y1 = uyvy_data[uyvy_idx + 3] as i32;

        // 预计算共享的 UV 分量
        let v_r = (V_TO_R * v) >> 8;
        let uv_g = (U_TO_G * u + V_TO_G * v) >> 8;
        let u_b = (U_TO_B * u) >> 8;

        // 第一个像素
        rgb_buffer[rgb_idx] = (y0 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 1] = (y0 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 2] = (y0 + u_b).clamp(0, 255) as u8;

        // 第二个像素
        rgb_buffer[rgb_idx + 3] = (y1 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 4] = (y1 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 5] = (y1 + u_b).clamp(0, 255) as u8;

        uyvy_idx += 4;
        rgb_idx += 6;
    }

    Ok(())
}

// ============================================================================
// SIMD 优化实现
// ============================================================================

// 根据目标架构选择最优实现：
// - ARM64 (aarch64): 使用 NEON intrinsics
// - x86_64: 运行时检测 AVX2，未支持路径回退到通用实现
// - 其他: 使用标量实现

/// SIMD 优化的 YUYV 转 RGB
///
/// 使用批量处理和循环展开来帮助编译器自动向量化
#[cfg(test)]
fn yuyv_to_rgb_simd(yuyv_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let pixel_count = checked_packed_422_pixel_count(width, height, "YUYV")?;
    let rgb_size = checked_frame_size(pixel_count, 3, "RGB")?;
    let mut rgb = vec![0u8; rgb_size];
    yuyv_to_rgb_simd_into(yuyv_data, &mut rgb)?;
    Ok(rgb)
}

#[cfg(any(
    all(target_arch = "aarch64", target_feature = "neon"),
    all(target_arch = "arm", target_feature = "neon")
))]
mod neon {
    #[cfg(target_arch = "aarch64")]
    use std::arch::aarch64::*;
    #[cfg(target_arch = "arm")]
    use std::arch::arm::*;

    #[cfg(target_arch = "arm")]
    #[inline(always)]
    unsafe fn vzip1_u8(a: uint8x8_t, b: uint8x8_t) -> uint8x8_t {
        vzip_u8(a, b).0
    }

    #[cfg(target_arch = "arm")]
    #[inline(always)]
    unsafe fn vzip2_u8(a: uint8x8_t, b: uint8x8_t) -> uint8x8_t {
        vzip_u8(a, b).1
    }

    // Widen every coefficient product to i32: 454 * (-128) and the green
    // sum overflow i16. Shift before narrowing to match the scalar rounding.
    #[inline(always)]
    unsafe fn rgb8(y: uint8x8_t, u: uint8x8_t, v: uint8x8_t) -> uint8x8x3_t {
        let y = vreinterpretq_s16_u16(vmovl_u8(y));
        let u = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(u)), vdupq_n_s16(128));
        let v = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(v)), vdupq_n_s16(128));
        let r = vcombine_s16(
            vshrn_n_s32::<8>(vmull_n_s16(vget_low_s16(v), 359)),
            vshrn_n_s32::<8>(vmull_n_s16(vget_high_s16(v), 359)),
        );
        let g = vcombine_s16(
            vshrn_n_s32::<8>(vmlal_n_s16(
                vmull_n_s16(vget_low_s16(u), 88),
                vget_low_s16(v),
                183,
            )),
            vshrn_n_s32::<8>(vmlal_n_s16(
                vmull_n_s16(vget_high_s16(u), 88),
                vget_high_s16(v),
                183,
            )),
        );
        let b = vcombine_s16(
            vshrn_n_s32::<8>(vmull_n_s16(vget_low_s16(u), 454)),
            vshrn_n_s32::<8>(vmull_n_s16(vget_high_s16(u), 454)),
        );
        uint8x8x3_t(
            vqmovun_s16(vaddq_s16(y, r)),
            vqmovun_s16(vsubq_s16(y, g)),
            vqmovun_s16(vaddq_s16(y, b)),
        )
    }

    #[inline(always)]
    unsafe fn rgb4_with_coefficients(
        y: int16x4_t,
        u: int16x4_t,
        v: int16x4_t,
        [cy, cr, gu, gv, cb, _]: [i32; 6],
    ) -> (int16x4_t, int16x4_t, int16x4_t) {
        let y = vmull_n_s16(y, cy as i16);
        let rounding = vdupq_n_s32(512);
        let r = vshrn_n_s32::<10>(vaddq_s32(vmlal_n_s16(y, v, cr as i16), rounding));
        let g = vshrn_n_s32::<10>(vaddq_s32(
            vmlsl_n_s16(vmlsl_n_s16(y, u, gu as i16), v, gv as i16),
            rounding,
        ));
        let b = vshrn_n_s32::<10>(vaddq_s32(vmlal_n_s16(y, u, cb as i16), rounding));
        (r, g, b)
    }

    type PreparedChroma4 = (int32x4_t, int32x4_t, int32x4_t);

    #[inline(always)]
    unsafe fn prepare_chroma4(
        u: int16x4_t,
        v: int16x4_t,
        [_, cr, gu, gv, cb, _]: [i32; 6],
    ) -> PreparedChroma4 {
        (
            vmull_n_s16(v, cr as i16),
            vnegq_s32(vmlal_n_s16(vmull_n_s16(u, gu as i16), v, gv as i16)),
            vmull_n_s16(u, cb as i16),
        )
    }

    #[inline(always)]
    unsafe fn rgb4_with_prepared_chroma(
        y: int16x4_t,
        chroma: &PreparedChroma4,
        cy: i16,
    ) -> (int16x4_t, int16x4_t, int16x4_t) {
        let y = vmull_n_s16(y, cy);
        let rounding = vdupq_n_s32(512);
        (
            vshrn_n_s32::<10>(vaddq_s32(vaddq_s32(y, chroma.0), rounding)),
            vshrn_n_s32::<10>(vaddq_s32(vaddq_s32(y, chroma.1), rounding)),
            vshrn_n_s32::<10>(vaddq_s32(vaddq_s32(y, chroma.2), rounding)),
        )
    }

    #[inline(always)]
    unsafe fn rgb8_with_prepared_chroma(
        y: uint8x8_t,
        low_chroma: &PreparedChroma4,
        high_chroma: &PreparedChroma4,
        [cy, _, _, _, _, offset]: [i32; 6],
    ) -> uint8x8x3_t {
        let y = vsubq_s16(
            vreinterpretq_s16_u16(vmovl_u8(y)),
            vdupq_n_s16(offset as i16),
        );
        let low = rgb4_with_prepared_chroma(vget_low_s16(y), low_chroma, cy as i16);
        let high = rgb4_with_prepared_chroma(vget_high_s16(y), high_chroma, cy as i16);
        uint8x8x3_t(
            vqmovun_s16(vcombine_s16(low.0, high.0)),
            vqmovun_s16(vcombine_s16(low.1, high.1)),
            vqmovun_s16(vcombine_s16(low.2, high.2)),
        )
    }

    #[inline(always)]
    unsafe fn rgb8_two_luma_with_coefficients(
        y0: uint8x8_t,
        y1: uint8x8_t,
        u: uint8x8_t,
        v: uint8x8_t,
        coefficients: [i32; 6],
    ) -> (uint8x8x3_t, uint8x8x3_t) {
        let u = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(u)), vdupq_n_s16(128));
        let v = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(v)), vdupq_n_s16(128));
        let low = prepare_chroma4(vget_low_s16(u), vget_low_s16(v), coefficients);
        let high = prepare_chroma4(vget_high_s16(u), vget_high_s16(v), coefficients);
        (
            rgb8_with_prepared_chroma(y0, &low, &high, coefficients),
            rgb8_with_prepared_chroma(y1, &low, &high, coefficients),
        )
    }

    #[inline(always)]
    unsafe fn rgb8_with_coefficients(
        y: uint8x8_t,
        u: uint8x8_t,
        v: uint8x8_t,
        coefficients: [i32; 6],
    ) -> uint8x8x3_t {
        let offset = vdupq_n_s16(coefficients[5] as i16);
        let y = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(y)), offset);
        let u = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(u)), vdupq_n_s16(128));
        let v = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(v)), vdupq_n_s16(128));
        let low = rgb4_with_coefficients(
            vget_low_s16(y),
            vget_low_s16(u),
            vget_low_s16(v),
            coefficients,
        );
        let high = rgb4_with_coefficients(
            vget_high_s16(y),
            vget_high_s16(u),
            vget_high_s16(v),
            coefficients,
        );
        uint8x8x3_t(
            vqmovun_s16(vcombine_s16(low.0, high.0)),
            vqmovun_s16(vcombine_s16(low.1, high.1)),
            vqmovun_s16(vcombine_s16(low.2, high.2)),
        )
    }

    #[inline]
    pub unsafe fn packed422<const UYVY: bool>(source: &[u8], rgb: &mut [u8]) {
        let groups = (source.len() / 32).min(rgb.len() / 48);
        for group in 0..groups {
            let p = vld4_u8(source.as_ptr().add(group * 32));
            let (y0, u, y1, v) = if UYVY {
                (p.1, p.0, p.3, p.2)
            } else {
                (p.0, p.1, p.2, p.3)
            };
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48),
                rgb8(vzip1_u8(y0, y1), vzip1_u8(u, u), vzip1_u8(v, v)),
            );
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48 + 24),
                rgb8(vzip2_u8(y0, y1), vzip2_u8(u, u), vzip2_u8(v, v)),
            );
        }
        let source = &source[groups * 32..];
        let rgb = &mut rgb[groups * 48..];
        if UYVY {
            super::uyvy_to_rgb_scalar_into(source, rgb).unwrap();
        } else {
            super::yuyv_to_rgb_scalar_into(source, rgb).unwrap();
        }
    }

    pub unsafe fn packed422_row_with_coefficients(
        source: &[u8],
        uyvy: bool,
        coefficients: [i32; 6],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (source.len() / 32).min(rgb.len() / 48);
        for group in 0..groups {
            let pixels = vld4_u8(source.as_ptr().add(group * 32));
            let (y0, u, y1, v) = if uyvy {
                (pixels.1, pixels.0, pixels.3, pixels.2)
            } else {
                (pixels.0, pixels.1, pixels.2, pixels.3)
            };
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48),
                rgb8_with_coefficients(
                    vzip1_u8(y0, y1),
                    vzip1_u8(u, u),
                    vzip1_u8(v, v),
                    coefficients,
                ),
            );
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48 + 24),
                rgb8_with_coefficients(
                    vzip2_u8(y0, y1),
                    vzip2_u8(u, u),
                    vzip2_u8(v, v),
                    coefficients,
                ),
            );
        }
        groups * 16
    }

    pub unsafe fn packed422_half_row_with_coefficients(
        source: &[u8],
        uyvy: bool,
        coefficients: [i32; 6],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (source.len() / 32).min(rgb.len() / 24);
        for group in 0..groups {
            let pixels = vld4_u8(source.as_ptr().add(group * 32));
            let (y, u, v) = if uyvy {
                (pixels.1, pixels.0, pixels.2)
            } else {
                (pixels.0, pixels.1, pixels.3)
            };
            vst3_u8(
                rgb.as_mut_ptr().add(group * 24),
                rgb8_with_coefficients(y, u, v, coefficients),
            );
        }
        groups * 8
    }

    pub unsafe fn yuyv_to_rgb_neon(source: &[u8], rgb: &mut [u8]) {
        packed422::<false>(source, rgb);
    }

    #[inline]
    pub unsafe fn packed_8888_row<const R: usize, const G: usize, const B: usize>(
        source: &[u8],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (source.len() / 64).min(rgb.len() / 48);
        for group in 0..groups {
            let pixels = vld4q_u8(source.as_ptr().add(group * 64));
            let channels = [pixels.0, pixels.1, pixels.2, pixels.3];
            vst3q_u8(
                rgb.as_mut_ptr().add(group * 48),
                uint8x16x3_t(channels[R], channels[G], channels[B]),
            );
        }
        groups * 16
    }

    pub unsafe fn planar_row_with_coefficients(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        chroma_stride: (usize, usize),
        coefficients: [i32; 6],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (y.len() / 8).min(rgb.len() / 24);
        for group in 0..groups {
            let offset = group * 4;
            let u = u32::from_le_bytes(std::array::from_fn(|index| {
                u[(offset + index) * chroma_stride.0]
            }));
            let v = u32::from_le_bytes(std::array::from_fn(|index| {
                v[(offset + index) * chroma_stride.1]
            }));
            let u = vcreate_u8(u as u64);
            let v = vcreate_u8(v as u64);
            vst3_u8(
                rgb.as_mut_ptr().add(group * 24),
                rgb8_with_coefficients(
                    vld1_u8(y.as_ptr().add(group * 8)),
                    vzip1_u8(u, u),
                    vzip1_u8(v, v),
                    coefficients,
                ),
            );
        }
        groups * 8
    }

    pub unsafe fn planar_contiguous_row_with_coefficients(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        coefficients: [i32; 6],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (y.len() / 16)
            .min(u.len() / 8)
            .min(v.len() / 8)
            .min(rgb.len() / 48);
        for group in 0..groups {
            let u = vld1_u8(u.as_ptr().add(group * 8));
            let v = vld1_u8(v.as_ptr().add(group * 8));
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48),
                rgb8_with_coefficients(
                    vld1_u8(y.as_ptr().add(group * 16)),
                    vzip1_u8(u, u),
                    vzip1_u8(v, v),
                    coefficients,
                ),
            );
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48 + 24),
                rgb8_with_coefficients(
                    vld1_u8(y.as_ptr().add(group * 16 + 8)),
                    vzip2_u8(u, u),
                    vzip2_u8(v, v),
                    coefficients,
                ),
            );
        }
        groups * 16
    }

    pub unsafe fn planar_contiguous_two_rows_with_coefficients(
        y0: &[u8],
        y1: &[u8],
        u: &[u8],
        v: &[u8],
        coefficients: [i32; 6],
        rgb0: &mut [u8],
        rgb1: &mut [u8],
    ) -> usize {
        let groups = (y0.len() / 16)
            .min(y1.len() / 16)
            .min(u.len() / 8)
            .min(v.len() / 8)
            .min(rgb0.len() / 48)
            .min(rgb1.len() / 48);
        for group in 0..groups {
            let u = vld1_u8(u.as_ptr().add(group * 8));
            let v = vld1_u8(v.as_ptr().add(group * 8));
            let low = rgb8_two_luma_with_coefficients(
                vld1_u8(y0.as_ptr().add(group * 16)),
                vld1_u8(y1.as_ptr().add(group * 16)),
                vzip1_u8(u, u),
                vzip1_u8(v, v),
                coefficients,
            );
            let high = rgb8_two_luma_with_coefficients(
                vld1_u8(y0.as_ptr().add(group * 16 + 8)),
                vld1_u8(y1.as_ptr().add(group * 16 + 8)),
                vzip2_u8(u, u),
                vzip2_u8(v, v),
                coefficients,
            );
            vst3_u8(rgb0.as_mut_ptr().add(group * 48), low.0);
            vst3_u8(rgb1.as_mut_ptr().add(group * 48), low.1);
            vst3_u8(rgb0.as_mut_ptr().add(group * 48 + 24), high.0);
            vst3_u8(rgb1.as_mut_ptr().add(group * 48 + 24), high.1);
        }
        groups * 16
    }

    pub unsafe fn semiplanar_row_with_coefficients(
        y: &[u8],
        uv: &[u8],
        vu_order: bool,
        coefficients: [i32; 6],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (y.len() / 16).min(uv.len() / 16).min(rgb.len() / 48);
        for group in 0..groups {
            let chroma = vld2_u8(uv.as_ptr().add(group * 16));
            let (u, v) = if vu_order {
                (chroma.1, chroma.0)
            } else {
                (chroma.0, chroma.1)
            };
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48),
                rgb8_with_coefficients(
                    vld1_u8(y.as_ptr().add(group * 16)),
                    vzip1_u8(u, u),
                    vzip1_u8(v, v),
                    coefficients,
                ),
            );
            vst3_u8(
                rgb.as_mut_ptr().add(group * 48 + 24),
                rgb8_with_coefficients(
                    vld1_u8(y.as_ptr().add(group * 16 + 8)),
                    vzip2_u8(u, u),
                    vzip2_u8(v, v),
                    coefficients,
                ),
            );
        }
        groups * 16
    }

    pub unsafe fn semiplanar_two_rows_with_coefficients(
        y0: &[u8],
        y1: &[u8],
        uv: &[u8],
        vu_order: bool,
        coefficients: [i32; 6],
        rgb0: &mut [u8],
        rgb1: &mut [u8],
    ) -> usize {
        let groups = (y0.len() / 16)
            .min(y1.len() / 16)
            .min(uv.len() / 16)
            .min(rgb0.len() / 48)
            .min(rgb1.len() / 48);
        for group in 0..groups {
            let chroma = vld2_u8(uv.as_ptr().add(group * 16));
            let (u, v) = if vu_order {
                (chroma.1, chroma.0)
            } else {
                (chroma.0, chroma.1)
            };
            let low = rgb8_two_luma_with_coefficients(
                vld1_u8(y0.as_ptr().add(group * 16)),
                vld1_u8(y1.as_ptr().add(group * 16)),
                vzip1_u8(u, u),
                vzip1_u8(v, v),
                coefficients,
            );
            let high = rgb8_two_luma_with_coefficients(
                vld1_u8(y0.as_ptr().add(group * 16 + 8)),
                vld1_u8(y1.as_ptr().add(group * 16 + 8)),
                vzip2_u8(u, u),
                vzip2_u8(v, v),
                coefficients,
            );
            vst3_u8(rgb0.as_mut_ptr().add(group * 48), low.0);
            vst3_u8(rgb1.as_mut_ptr().add(group * 48), low.1);
            vst3_u8(rgb0.as_mut_ptr().add(group * 48 + 24), high.0);
            vst3_u8(rgb1.as_mut_ptr().add(group * 48 + 24), high.1);
        }
        groups * 16
    }

    pub unsafe fn semiplanar_half_row_with_coefficients(
        y: &[u8],
        uv: &[u8],
        vu_order: bool,
        coefficients: [i32; 6],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (y.len() / 16).min(uv.len() / 16).min(rgb.len() / 24);
        for group in 0..groups {
            let luma = vld2_u8(y.as_ptr().add(group * 16)).0;
            let chroma = vld2_u8(uv.as_ptr().add(group * 16));
            let (u, v) = if vu_order {
                (chroma.1, chroma.0)
            } else {
                (chroma.0, chroma.1)
            };
            vst3_u8(
                rgb.as_mut_ptr().add(group * 24),
                rgb8_with_coefficients(luma, u, v, coefficients),
            );
        }
        groups * 8
    }

    pub unsafe fn planar_half_row_with_coefficients(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        coefficients: [i32; 6],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (y.len() / 16)
            .min(u.len() / 8)
            .min(v.len() / 8)
            .min(rgb.len() / 24);
        for group in 0..groups {
            let luma = vld2_u8(y.as_ptr().add(group * 16)).0;
            let u = vld1_u8(u.as_ptr().add(group * 8));
            let v = vld1_u8(v.as_ptr().add(group * 8));
            vst3_u8(
                rgb.as_mut_ptr().add(group * 24),
                rgb8_with_coefficients(luma, u, v, coefficients),
            );
        }
        groups * 8
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    #[inline(always)]
    unsafe fn combine_shuffled_halves(raw: __m256i, mask: __m128i) -> __m128i {
        let low = _mm_shuffle_epi8(_mm256_castsi256_si128(raw), mask);
        let high = _mm_shuffle_epi8(_mm256_extracti128_si256::<1>(raw), mask);
        _mm_unpacklo_epi64(low, high)
    }

    #[inline(always)]
    unsafe fn combine_shuffled_quarters(raw: __m256i, mask: __m128i) -> __m128i {
        let low = _mm_shuffle_epi8(_mm256_castsi256_si128(raw), mask);
        let high = _mm_shuffle_epi8(_mm256_extracti128_si256::<1>(raw), mask);
        _mm_unpacklo_epi32(low, high)
    }

    #[inline(always)]
    unsafe fn store_twelve_bytes(value: __m128i, output: *mut u8) {
        _mm_storel_epi64(output.cast(), value);
        std::ptr::write_unaligned(
            output.add(8).cast::<u32>(),
            _mm_cvtsi128_si32(_mm_srli_si128::<8>(value)) as u32,
        );
    }

    #[inline(always)]
    unsafe fn prepare_chroma<const ROUNDING: i32, const GREEN_ROUNDING: i32>(
        u_bytes: __m128i,
        v_bytes: __m128i,
        [_, cr, gu, gv, cb, _]: [i32; 6],
    ) -> [__m256i; 3] {
        let center = _mm256_set1_epi32(128);
        let u = _mm256_sub_epi32(_mm256_cvtepu8_epi32(u_bytes), center);
        let v = _mm256_sub_epi32(_mm256_cvtepu8_epi32(v_bytes), center);
        [
            _mm256_add_epi32(
                _mm256_mullo_epi32(v, _mm256_set1_epi32(cr)),
                _mm256_set1_epi32(ROUNDING),
            ),
            _mm256_add_epi32(
                _mm256_sub_epi32(
                    _mm256_sub_epi32(
                        _mm256_setzero_si256(),
                        _mm256_mullo_epi32(u, _mm256_set1_epi32(gu)),
                    ),
                    _mm256_mullo_epi32(v, _mm256_set1_epi32(gv)),
                ),
                _mm256_set1_epi32(GREEN_ROUNDING),
            ),
            _mm256_add_epi32(
                _mm256_mullo_epi32(u, _mm256_set1_epi32(cb)),
                _mm256_set1_epi32(ROUNDING),
            ),
        ]
    }

    #[inline(always)]
    unsafe fn convert_eight_with_chroma<const SHIFT: i32>(
        y_bytes: __m128i,
        chroma: &[__m256i; 3],
        [cy, _, _, _, _, offset]: [i32; 6],
        output: &mut [u8],
    ) {
        let y = _mm256_mullo_epi32(
            _mm256_sub_epi32(_mm256_cvtepu8_epi32(y_bytes), _mm256_set1_epi32(offset)),
            _mm256_set1_epi32(cy),
        );
        let red = _mm256_srai_epi32::<SHIFT>(_mm256_add_epi32(y, chroma[0]));
        let green = _mm256_srai_epi32::<SHIFT>(_mm256_add_epi32(y, chroma[1]));
        let blue = _mm256_srai_epi32::<SHIFT>(_mm256_add_epi32(y, chroma[2]));
        let zero = _mm256_setzero_si256();
        let maximum = _mm256_set1_epi32(255);
        let clamp = |value| _mm256_min_epi32(_mm256_max_epi32(value, zero), maximum);
        let red = clamp(red);
        let green = clamp(green);
        let blue = clamp(blue);
        let pack = |value: __m256i| {
            let words = _mm_packus_epi32(
                _mm256_castsi256_si128(value),
                _mm256_extracti128_si256::<1>(value),
            );
            _mm_packus_epi16(words, words)
        };
        let red = pack(red);
        let green = pack(green);
        let blue = pack(blue);
        let first = _mm_or_si128(
            _mm_or_si128(
                _mm_shuffle_epi8(
                    red,
                    _mm_setr_epi8(0, -1, -1, 1, -1, -1, 2, -1, -1, 3, -1, -1, 4, -1, -1, 5),
                ),
                _mm_shuffle_epi8(
                    green,
                    _mm_setr_epi8(-1, 0, -1, -1, 1, -1, -1, 2, -1, -1, 3, -1, -1, 4, -1, -1),
                ),
            ),
            _mm_shuffle_epi8(
                blue,
                _mm_setr_epi8(-1, -1, 0, -1, -1, 1, -1, -1, 2, -1, -1, 3, -1, -1, 4, -1),
            ),
        );
        let second = _mm_or_si128(
            _mm_or_si128(
                _mm_shuffle_epi8(
                    red,
                    _mm_setr_epi8(-1, -1, 6, -1, -1, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
                ),
                _mm_shuffle_epi8(
                    green,
                    _mm_setr_epi8(5, -1, -1, 6, -1, -1, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1),
                ),
            ),
            _mm_shuffle_epi8(
                blue,
                _mm_setr_epi8(-1, 5, -1, -1, 6, -1, -1, 7, -1, -1, -1, -1, -1, -1, -1, -1),
            ),
        );
        _mm_storeu_si128(output.as_mut_ptr().cast(), first);
        _mm_storel_epi64(output.as_mut_ptr().add(16).cast(), second);
    }

    #[inline(always)]
    unsafe fn convert_eight<const SHIFT: i32, const ROUNDING: i32, const GREEN_ROUNDING: i32>(
        y_bytes: __m128i,
        u_bytes: __m128i,
        v_bytes: __m128i,
        coefficients: [i32; 6],
        output: &mut [u8],
    ) {
        let chroma = prepare_chroma::<ROUNDING, GREEN_ROUNDING>(u_bytes, v_bytes, coefficients);
        convert_eight_with_chroma::<SHIFT>(y_bytes, &chroma, coefficients, output);
    }

    /// Convert complete YUV 4:2:2 pixel pairs. Callers must runtime-check AVX2.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn packed422_avx2<
        const UYVY: bool,
        const SHIFT: i32,
        const ROUNDING: i32,
        const GREEN_ROUNDING: i32,
    >(
        source: &[u8],
        rgb: &mut [u8],
        coefficients: [i32; 6],
    ) {
        let pair_count = (source.len() / 4).min(rgb.len() / 6);
        let (y_mask, u_mask, v_mask) = if UYVY {
            (
                _mm_setr_epi8(1, 3, 5, 7, 9, 11, 13, 15, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(0, 0, 4, 4, 8, 8, 12, 12, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(2, 2, 6, 6, 10, 10, 14, 14, -1, -1, -1, -1, -1, -1, -1, -1),
            )
        } else {
            (
                _mm_setr_epi8(0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(1, 1, 5, 5, 9, 9, 13, 13, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(3, 3, 7, 7, 11, 11, 15, 15, -1, -1, -1, -1, -1, -1, -1, -1),
            )
        };
        let vectorized_pairs = pair_count / 8 * 8;
        for pair in (0..vectorized_pairs).step_by(8) {
            let raw = _mm256_loadu_si256(source.as_ptr().add(pair * 4).cast());
            let y = combine_shuffled_halves(raw, y_mask);
            let u = combine_shuffled_halves(raw, u_mask);
            let v = combine_shuffled_halves(raw, v_mask);
            let destination = &mut rgb[pair * 6..pair * 6 + 48];
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                y,
                u,
                v,
                coefficients,
                &mut destination[..24],
            );
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                _mm_srli_si128::<8>(y),
                _mm_srli_si128::<8>(u),
                _mm_srli_si128::<8>(v),
                coefficients,
                &mut destination[24..],
            );
        }
        for pair in vectorized_pairs..pair_count {
            let source = &source[pair * 4..pair * 4 + 4];
            let (y0, u, y1, v) = if UYVY {
                (source[1], source[0], source[3], source[2])
            } else {
                (source[0], source[1], source[2], source[3])
            };
            let [cy, cr, gu, gv, cb, offset] = coefficients;
            let y0 = (i32::from(y0) - offset) * cy;
            let y1 = (i32::from(y1) - offset) * cy;
            let u = i32::from(u) - 128;
            let v = i32::from(v) - 128;
            let red_chroma = cr * v + ROUNDING;
            let green_chroma = -gu * u - gv * v + GREEN_ROUNDING;
            let blue_chroma = cb * u + ROUNDING;
            let destination = &mut rgb[pair * 6..pair * 6 + 6];
            for (y, output) in [y0, y1].into_iter().zip(destination.chunks_exact_mut(3)) {
                output.copy_from_slice(&[
                    ((y + red_chroma) >> SHIFT).clamp(0, 255) as u8,
                    ((y + green_chroma) >> SHIFT).clamp(0, 255) as u8,
                    ((y + blue_chroma) >> SHIFT).clamp(0, 255) as u8,
                ]);
            }
        }
    }

    /// Convert every other pixel from one packed 4:2:2 row. Each selected
    /// pixel already has one complete chroma pair, so no chroma expansion is
    /// needed before the shared AVX2 color transform.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn packed422_half_row_avx2<const UYVY: bool>(
        source: &[u8],
        rgb: &mut [u8],
        coefficients: [i32; 6],
    ) -> usize {
        let groups = (source.len() / 32).min(rgb.len() / 24);
        let (y_mask, u_mask, v_mask) = if UYVY {
            (
                _mm_setr_epi8(1, 5, 9, 13, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(0, 4, 8, 12, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(2, 6, 10, 14, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
            )
        } else {
            (
                _mm_setr_epi8(0, 4, 8, 12, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(1, 5, 9, 13, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
                _mm_setr_epi8(3, 7, 11, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
            )
        };
        for group in 0..groups {
            let raw = _mm256_loadu_si256(source.as_ptr().add(group * 32).cast());
            let y = combine_shuffled_quarters(raw, y_mask);
            let u = combine_shuffled_quarters(raw, u_mask);
            let v = combine_shuffled_quarters(raw, v_mask);
            convert_eight::<10, 512, 512>(
                y,
                u,
                v,
                coefficients,
                &mut rgb[group * 24..group * 24 + 24],
            );
        }
        groups * 8
    }

    /// Convert complete groups of four packed 8888 pixels into RGB24.
    #[target_feature(enable = "ssse3")]
    pub(super) unsafe fn packed_8888_row_ssse3<const R: usize, const G: usize, const B: usize>(
        source: &[u8],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (source.len() / 16).min(rgb.len() / 12);
        let mask = _mm_setr_epi8(
            R as i8,
            G as i8,
            B as i8,
            (4 + R) as i8,
            (4 + G) as i8,
            (4 + B) as i8,
            (8 + R) as i8,
            (8 + G) as i8,
            (8 + B) as i8,
            (12 + R) as i8,
            (12 + G) as i8,
            (12 + B) as i8,
            -1,
            -1,
            -1,
            -1,
        );
        let output = rgb.as_mut_ptr();
        for group in 0..groups {
            let pixels = _mm_shuffle_epi8(
                _mm_loadu_si128(source.as_ptr().add(group * 16).cast()),
                mask,
            );
            store_twelve_bytes(pixels, output.add(group * 12));
        }
        groups * 4
    }

    /// Convert complete groups of eight packed 8888 pixels into RGB24.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn packed_8888_row_avx2<const R: usize, const G: usize, const B: usize>(
        source: &[u8],
        rgb: &mut [u8],
    ) -> usize {
        let groups = (source.len() / 32).min(rgb.len() / 24);
        let mask = _mm_setr_epi8(
            R as i8,
            G as i8,
            B as i8,
            (4 + R) as i8,
            (4 + G) as i8,
            (4 + B) as i8,
            (8 + R) as i8,
            (8 + G) as i8,
            (8 + B) as i8,
            (12 + R) as i8,
            (12 + G) as i8,
            (12 + B) as i8,
            -1,
            -1,
            -1,
            -1,
        );
        let mask = _mm256_broadcastsi128_si256(mask);
        let output = rgb.as_mut_ptr();
        for group in 0..groups {
            let pixels = _mm256_shuffle_epi8(
                _mm256_loadu_si256(source.as_ptr().add(group * 32).cast()),
                mask,
            );
            store_twelve_bytes(_mm256_castsi256_si128(pixels), output.add(group * 24));
            store_twelve_bytes(
                _mm256_extracti128_si256::<1>(pixels),
                output.add(group * 24 + 12),
            );
        }
        groups * 8
    }

    /// Convert complete groups of sixteen pixels from one planar YUV420 row.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn planar_row_avx2<
        const SHIFT: i32,
        const ROUNDING: i32,
        const GREEN_ROUNDING: i32,
    >(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        rgb: &mut [u8],
        coefficients: [i32; 6],
    ) -> usize {
        let groups = (y.len() / 16)
            .min(u.len() / 8)
            .min(v.len() / 8)
            .min(rgb.len() / 48);
        for group in 0..groups {
            let y = _mm_loadu_si128(y.as_ptr().add(group * 16).cast());
            let u = _mm_loadl_epi64(u.as_ptr().add(group * 8).cast());
            let v = _mm_loadl_epi64(v.as_ptr().add(group * 8).cast());
            let u = _mm_unpacklo_epi8(u, u);
            let v = _mm_unpacklo_epi8(v, v);
            let destination = &mut rgb[group * 48..group * 48 + 48];
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                y,
                u,
                v,
                coefficients,
                &mut destination[..24],
            );
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                _mm_srli_si128::<8>(y),
                _mm_srli_si128::<8>(u),
                _mm_srli_si128::<8>(v),
                coefficients,
                &mut destination[24..],
            );
        }
        groups * 16
    }

    /// Convert every other luma sample from one planar YUV420 row.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn planar_half_row_avx2(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        rgb: &mut [u8],
        coefficients: [i32; 6],
    ) -> usize {
        let groups = (y.len() / 16)
            .min(u.len() / 8)
            .min(v.len() / 8)
            .min(rgb.len() / 24);
        let even_mask = _mm_setr_epi8(0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1);
        for group in 0..groups {
            let y = _mm_shuffle_epi8(
                _mm_loadu_si128(y.as_ptr().add(group * 16).cast()),
                even_mask,
            );
            let u = _mm_loadl_epi64(u.as_ptr().add(group * 8).cast());
            let v = _mm_loadl_epi64(v.as_ptr().add(group * 8).cast());
            convert_eight::<10, 512, 512>(
                y,
                u,
                v,
                coefficients,
                &mut rgb[group * 24..group * 24 + 24],
            );
        }
        groups * 8
    }

    /// Convert two planar YUV420 rows while reusing both chroma expansion and
    /// chroma coefficient products. Callers must runtime-check AVX2.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn planar_two_rows_avx2<
        const SHIFT: i32,
        const ROUNDING: i32,
        const GREEN_ROUNDING: i32,
    >(
        y0: &[u8],
        y1: &[u8],
        u: &[u8],
        v: &[u8],
        rgb0: &mut [u8],
        rgb1: &mut [u8],
        coefficients: [i32; 6],
    ) -> usize {
        let groups = (y0.len() / 16)
            .min(y1.len() / 16)
            .min(u.len() / 8)
            .min(v.len() / 8)
            .min(rgb0.len() / 48)
            .min(rgb1.len() / 48);
        for group in 0..groups {
            let y0 = _mm_loadu_si128(y0.as_ptr().add(group * 16).cast());
            let y1 = _mm_loadu_si128(y1.as_ptr().add(group * 16).cast());
            let u = _mm_loadl_epi64(u.as_ptr().add(group * 8).cast());
            let v = _mm_loadl_epi64(v.as_ptr().add(group * 8).cast());
            let u = _mm_unpacklo_epi8(u, u);
            let v = _mm_unpacklo_epi8(v, v);
            let low = prepare_chroma::<ROUNDING, GREEN_ROUNDING>(u, v, coefficients);
            let high = prepare_chroma::<ROUNDING, GREEN_ROUNDING>(
                _mm_srli_si128::<8>(u),
                _mm_srli_si128::<8>(v),
                coefficients,
            );
            let offset = group * 48;
            convert_eight_with_chroma::<SHIFT>(
                y0,
                &low,
                coefficients,
                &mut rgb0[offset..offset + 24],
            );
            convert_eight_with_chroma::<SHIFT>(
                y1,
                &low,
                coefficients,
                &mut rgb1[offset..offset + 24],
            );
            convert_eight_with_chroma::<SHIFT>(
                _mm_srli_si128::<8>(y0),
                &high,
                coefficients,
                &mut rgb0[offset + 24..offset + 48],
            );
            convert_eight_with_chroma::<SHIFT>(
                _mm_srli_si128::<8>(y1),
                &high,
                coefficients,
                &mut rgb1[offset + 24..offset + 48],
            );
        }
        groups * 16
    }

    /// Convert complete groups of sixteen pixels from one NV12/NV21 row.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn semiplanar_row_avx2<
        const VU_ORDER: bool,
        const SHIFT: i32,
        const ROUNDING: i32,
        const GREEN_ROUNDING: i32,
    >(
        y: &[u8],
        uv: &[u8],
        rgb: &mut [u8],
        coefficients: [i32; 6],
    ) -> usize {
        let groups = (y.len() / 16).min(uv.len() / 16).min(rgb.len() / 48);
        let (u_mask, v_mask) = if VU_ORDER {
            (
                _mm_setr_epi8(1, 1, 3, 3, 5, 5, 7, 7, 9, 9, 11, 11, 13, 13, 15, 15),
                _mm_setr_epi8(0, 0, 2, 2, 4, 4, 6, 6, 8, 8, 10, 10, 12, 12, 14, 14),
            )
        } else {
            (
                _mm_setr_epi8(0, 0, 2, 2, 4, 4, 6, 6, 8, 8, 10, 10, 12, 12, 14, 14),
                _mm_setr_epi8(1, 1, 3, 3, 5, 5, 7, 7, 9, 9, 11, 11, 13, 13, 15, 15),
            )
        };
        for group in 0..groups {
            let y = _mm_loadu_si128(y.as_ptr().add(group * 16).cast());
            let uv = _mm_loadu_si128(uv.as_ptr().add(group * 16).cast());
            let u = _mm_shuffle_epi8(uv, u_mask);
            let v = _mm_shuffle_epi8(uv, v_mask);
            let destination = &mut rgb[group * 48..group * 48 + 48];
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                y,
                u,
                v,
                coefficients,
                &mut destination[..24],
            );
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                _mm_srli_si128::<8>(y),
                _mm_srli_si128::<8>(u),
                _mm_srli_si128::<8>(v),
                coefficients,
                &mut destination[24..],
            );
        }
        groups * 16
    }

    /// Convert two NV12/NV21 rows while reusing deinterleaved chroma products.
    /// Callers must runtime-check AVX2.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn semiplanar_two_rows_avx2<
        const VU_ORDER: bool,
        const SHIFT: i32,
        const ROUNDING: i32,
        const GREEN_ROUNDING: i32,
    >(
        y0: &[u8],
        y1: &[u8],
        uv: &[u8],
        rgb0: &mut [u8],
        rgb1: &mut [u8],
        coefficients: [i32; 6],
    ) -> usize {
        let groups = (y0.len() / 16)
            .min(y1.len() / 16)
            .min(uv.len() / 16)
            .min(rgb0.len() / 48)
            .min(rgb1.len() / 48);
        let (u_mask, v_mask) = if VU_ORDER {
            (
                _mm_setr_epi8(1, 1, 3, 3, 5, 5, 7, 7, 9, 9, 11, 11, 13, 13, 15, 15),
                _mm_setr_epi8(0, 0, 2, 2, 4, 4, 6, 6, 8, 8, 10, 10, 12, 12, 14, 14),
            )
        } else {
            (
                _mm_setr_epi8(0, 0, 2, 2, 4, 4, 6, 6, 8, 8, 10, 10, 12, 12, 14, 14),
                _mm_setr_epi8(1, 1, 3, 3, 5, 5, 7, 7, 9, 9, 11, 11, 13, 13, 15, 15),
            )
        };
        for group in 0..groups {
            let y0 = _mm_loadu_si128(y0.as_ptr().add(group * 16).cast());
            let y1 = _mm_loadu_si128(y1.as_ptr().add(group * 16).cast());
            let uv = _mm_loadu_si128(uv.as_ptr().add(group * 16).cast());
            let u = _mm_shuffle_epi8(uv, u_mask);
            let v = _mm_shuffle_epi8(uv, v_mask);
            let low = prepare_chroma::<ROUNDING, GREEN_ROUNDING>(u, v, coefficients);
            let high = prepare_chroma::<ROUNDING, GREEN_ROUNDING>(
                _mm_srli_si128::<8>(u),
                _mm_srli_si128::<8>(v),
                coefficients,
            );
            let offset = group * 48;
            convert_eight_with_chroma::<SHIFT>(
                y0,
                &low,
                coefficients,
                &mut rgb0[offset..offset + 24],
            );
            convert_eight_with_chroma::<SHIFT>(
                y1,
                &low,
                coefficients,
                &mut rgb1[offset..offset + 24],
            );
            convert_eight_with_chroma::<SHIFT>(
                _mm_srli_si128::<8>(y0),
                &high,
                coefficients,
                &mut rgb0[offset + 24..offset + 48],
            );
            convert_eight_with_chroma::<SHIFT>(
                _mm_srli_si128::<8>(y1),
                &high,
                coefficients,
                &mut rgb1[offset + 24..offset + 48],
            );
        }
        groups * 16
    }

    /// Convert one directly downsampled NV12/NV21 row (every second luma sample).
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn semiplanar_half_row_avx2<
        const VU_ORDER: bool,
        const SHIFT: i32,
        const ROUNDING: i32,
        const GREEN_ROUNDING: i32,
    >(
        y: &[u8],
        uv: &[u8],
        rgb: &mut [u8],
        coefficients: [i32; 6],
    ) -> usize {
        let groups = (y.len() / 32).min(uv.len() / 32).min(rgb.len() / 48);
        let even_mask = _mm_setr_epi8(0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1);
        let odd_mask = _mm_setr_epi8(1, 3, 5, 7, 9, 11, 13, 15, -1, -1, -1, -1, -1, -1, -1, -1);
        for group in 0..groups {
            let y = _mm256_loadu_si256(y.as_ptr().add(group * 32).cast());
            let uv = _mm256_loadu_si256(uv.as_ptr().add(group * 32).cast());
            let y = combine_shuffled_halves(y, even_mask);
            let (u, v) = if VU_ORDER {
                (
                    combine_shuffled_halves(uv, odd_mask),
                    combine_shuffled_halves(uv, even_mask),
                )
            } else {
                (
                    combine_shuffled_halves(uv, even_mask),
                    combine_shuffled_halves(uv, odd_mask),
                )
            };
            let destination = &mut rgb[group * 48..group * 48 + 48];
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                y,
                u,
                v,
                coefficients,
                &mut destination[..24],
            );
            convert_eight::<SHIFT, ROUNDING, GREEN_ROUNDING>(
                _mm_srli_si128::<8>(y),
                _mm_srli_si128::<8>(u),
                _mm_srli_si128::<8>(v),
                coefficients,
                &mut destination[24..],
            );
        }
        groups * 16
    }
}

fn yuyv_to_rgb_simd_into(yuyv_data: &[u8], rgb_buffer: &mut [u8]) -> Result<()> {
    // ARM64 NEON 优化路径
    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    {
        unsafe {
            neon::yuyv_to_rgb_neon(yuyv_data, rgb_buffer);
        }
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        unsafe {
            x86::packed422_avx2::<false, 8, 0, 255>(
                yuyv_data,
                rgb_buffer,
                [256, 359, 88, 183, 454, 0],
            );
        }
        return Ok(());
    }

    // 通用 SIMD 优化路径（循环展开 + 编译器自动向量化）
    #[cfg(not(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    )))]
    {
        yuyv_to_rgb_simd_generic(yuyv_data, rgb_buffer)
    }
}

/// 通用 SIMD 优化实现（使用循环展开帮助编译器自动向量化）
#[cfg(not(any(
    all(target_arch = "aarch64", target_feature = "neon"),
    all(target_arch = "arm", target_feature = "neon")
)))]
#[inline(always)]
fn yuyv_to_rgb_simd_generic(yuyv_data: &[u8], rgb_buffer: &mut [u8]) -> Result<()> {
    use yuv_coefficients::*;

    let yuyv_len = yuyv_data.len();
    let rgb_len = rgb_buffer.len();

    // 计算可以批量处理的部分（8 像素 = 16 字节 YUYV = 24 字节 RGB）
    let batch_size = 16; // 8 像素对应 16 字节 YUYV
    let rgb_batch_size = 24; // 8 像素对应 24 字节 RGB
    let num_batches = yuyv_len / batch_size;

    let mut yuyv_idx = 0;
    let mut rgb_idx = 0;

    // 批量处理主循环
    for _ in 0..num_batches {
        if rgb_idx + rgb_batch_size > rgb_len {
            break;
        }

        // 展开处理 4 对像素（8 像素总计）
        // 每对像素：4 字节 YUYV -> 6 字节 RGB
        macro_rules! process_pair {
            ($yuyv_off:expr, $rgb_off:expr) => {
                let y0 = yuyv_data[yuyv_idx + $yuyv_off] as i32;
                let u = yuyv_data[yuyv_idx + $yuyv_off + 1] as i32 - 128;
                let y1 = yuyv_data[yuyv_idx + $yuyv_off + 2] as i32;
                let v = yuyv_data[yuyv_idx + $yuyv_off + 3] as i32 - 128;

                let v_r = (V_TO_R * v) >> 8;
                let uv_g = (U_TO_G * u + V_TO_G * v) >> 8;
                let u_b = (U_TO_B * u) >> 8;

                rgb_buffer[rgb_idx + $rgb_off] = (y0 + v_r).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 1] = (y0 - uv_g).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 2] = (y0 + u_b).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 3] = (y1 + v_r).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 4] = (y1 - uv_g).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 5] = (y1 + u_b).clamp(0, 255) as u8;
            };
        }

        process_pair!(0, 0);
        process_pair!(4, 6);
        process_pair!(8, 12);
        process_pair!(12, 18);

        yuyv_idx += batch_size;
        rgb_idx += rgb_batch_size;
    }

    // 处理剩余数据
    while yuyv_idx + 3 < yuyv_len && rgb_idx + 5 < rgb_len {
        let y0 = yuyv_data[yuyv_idx] as i32;
        let u = yuyv_data[yuyv_idx + 1] as i32 - 128;
        let y1 = yuyv_data[yuyv_idx + 2] as i32;
        let v = yuyv_data[yuyv_idx + 3] as i32 - 128;

        let v_r = (V_TO_R * v) >> 8;
        let uv_g = (U_TO_G * u + V_TO_G * v) >> 8;
        let u_b = (U_TO_B * u) >> 8;

        rgb_buffer[rgb_idx] = (y0 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 1] = (y0 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 2] = (y0 + u_b).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 3] = (y1 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 4] = (y1 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 5] = (y1 + u_b).clamp(0, 255) as u8;

        yuyv_idx += 4;
        rgb_idx += 6;
    }

    Ok(())
}

fn checked_pixel_count(width: u32, height: u32) -> Result<usize> {
    if width == 0 || height == 0 {
        return Err(CameraError::InvalidFormat(format!(
            "Frame dimensions must be non-zero: {}x{}",
            width, height
        )));
    }

    let width = usize::try_from(width)
        .map_err(|_| CameraError::InvalidFormat(format!("Frame width is too large: {}", width)))?;
    let height = usize::try_from(height).map_err(|_| {
        CameraError::InvalidFormat(format!("Frame height is too large: {}", height))
    })?;

    width.checked_mul(height).ok_or_else(|| {
        CameraError::InvalidFormat(format!(
            "Frame dimensions are too large: {}x{}",
            width, height
        ))
    })
}

fn checked_packed_422_pixel_count(width: u32, height: u32, format: &str) -> Result<usize> {
    if !width.is_multiple_of(2) {
        return Err(CameraError::InvalidFormat(format!(
            "{} frame width must be even for packed 4:2:2 data: {}",
            format, width
        )));
    }

    checked_pixel_count(width, height)
}

fn checked_frame_size(pixel_count: usize, bytes_per_pixel: usize, format: &str) -> Result<usize> {
    pixel_count.checked_mul(bytes_per_pixel).ok_or_else(|| {
        CameraError::InvalidFormat(format!(
            "{} frame size is too large: {} pixels * {} bytes",
            format, pixel_count, bytes_per_pixel
        ))
    })
}

/// SIMD 优化的 UYVY 转 RGB（使用预分配缓冲区）
#[inline(always)]
fn uyvy_to_rgb_simd_into(uyvy_data: &[u8], rgb_buffer: &mut [u8]) -> Result<()> {
    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    {
        unsafe {
            neon::packed422::<true>(uyvy_data, rgb_buffer);
        }
        Ok(())
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        unsafe {
            x86::packed422_avx2::<true, 8, 0, 255>(
                uyvy_data,
                rgb_buffer,
                [256, 359, 88, 183, 454, 0],
            );
        }
        return Ok(());
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    )))]
    uyvy_to_rgb_simd_generic(uyvy_data, rgb_buffer)
}
#[cfg(not(any(
    all(target_arch = "aarch64", target_feature = "neon"),
    all(target_arch = "arm", target_feature = "neon")
)))]
fn uyvy_to_rgb_simd_generic(uyvy_data: &[u8], rgb_buffer: &mut [u8]) -> Result<()> {
    use yuv_coefficients::*;

    let uyvy_len = uyvy_data.len();
    let rgb_len = rgb_buffer.len();

    let batch_size = 16;
    let rgb_batch_size = 24;
    let num_batches = uyvy_len / batch_size;

    let mut uyvy_idx = 0;
    let mut rgb_idx = 0;

    for _ in 0..num_batches {
        if rgb_idx + rgb_batch_size > rgb_len {
            break;
        }

        macro_rules! process_pair {
            ($uyvy_off:expr, $rgb_off:expr) => {
                let u = uyvy_data[uyvy_idx + $uyvy_off] as i32 - 128;
                let y0 = uyvy_data[uyvy_idx + $uyvy_off + 1] as i32;
                let v = uyvy_data[uyvy_idx + $uyvy_off + 2] as i32 - 128;
                let y1 = uyvy_data[uyvy_idx + $uyvy_off + 3] as i32;

                let v_r = (V_TO_R * v) >> 8;
                let uv_g = (U_TO_G * u + V_TO_G * v) >> 8;
                let u_b = (U_TO_B * u) >> 8;

                rgb_buffer[rgb_idx + $rgb_off] = (y0 + v_r).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 1] = (y0 - uv_g).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 2] = (y0 + u_b).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 3] = (y1 + v_r).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 4] = (y1 - uv_g).clamp(0, 255) as u8;
                rgb_buffer[rgb_idx + $rgb_off + 5] = (y1 + u_b).clamp(0, 255) as u8;
            };
        }

        process_pair!(0, 0);
        process_pair!(4, 6);
        process_pair!(8, 12);
        process_pair!(12, 18);

        uyvy_idx += batch_size;
        rgb_idx += rgb_batch_size;
    }

    // 处理剩余数据
    while uyvy_idx + 3 < uyvy_len && rgb_idx + 5 < rgb_len {
        let u = uyvy_data[uyvy_idx] as i32 - 128;
        let y0 = uyvy_data[uyvy_idx + 1] as i32;
        let v = uyvy_data[uyvy_idx + 2] as i32 - 128;
        let y1 = uyvy_data[uyvy_idx + 3] as i32;

        let v_r = (V_TO_R * v) >> 8;
        let uv_g = (U_TO_G * u + V_TO_G * v) >> 8;
        let u_b = (U_TO_B * u) >> 8;

        rgb_buffer[rgb_idx] = (y0 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 1] = (y0 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 2] = (y0 + u_b).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 3] = (y1 + v_r).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 4] = (y1 - uv_g).clamp(0, 255) as u8;
        rgb_buffer[rgb_idx + 5] = (y1 + u_b).clamp(0, 255) as u8;

        uyvy_idx += 4;
        rgb_idx += 6;
    }

    Ok(())
}

fn packed_8888_to_rgb_into<const R: usize, const G: usize, const B: usize>(
    packed: &[u8],
    width: usize,
    height: usize,
    packed_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    let packed_row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| CameraError::InvalidFormat("packed 8888 size overflow".to_string()))?;
    let rgb_row_bytes = width
        .checked_mul(3)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".to_string()))?;
    if width == 0 || height == 0 || packed_stride < packed_row_bytes {
        return Err(CameraError::InvalidFormat(
            "invalid packed 8888 dimensions or stride".to_string(),
        ));
    }

    let required_packed = (height - 1)
        .checked_mul(packed_stride)
        .and_then(|size| size.checked_add(packed_row_bytes))
        .ok_or_else(|| CameraError::InvalidFormat("packed 8888 size overflow".to_string()))?;
    let required_rgb = height
        .checked_mul(rgb_row_bytes)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".to_string()))?;
    if packed.len() < required_packed || rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "packed 8888 or RGB buffer is too small".to_string(),
        ));
    }

    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    #[cfg(target_arch = "x86_64")]
    let ssse3 = !avx2 && std::arch::is_x86_feature_detected!("ssse3");
    for row in 0..height {
        let source = &packed[row * packed_stride..row * packed_stride + packed_row_bytes];
        let destination = &mut rgb[row * rgb_row_bytes..(row + 1) * rgb_row_bytes];
        #[cfg(any(
            all(target_arch = "aarch64", target_feature = "neon"),
            all(target_arch = "arm", target_feature = "neon")
        ))]
        let converted = unsafe { neon::packed_8888_row::<R, G, B>(source, destination) };
        #[cfg(target_arch = "x86_64")]
        let converted = if avx2 {
            unsafe { x86::packed_8888_row_avx2::<R, G, B>(source, destination) }
        } else if ssse3 {
            unsafe { x86::packed_8888_row_ssse3::<R, G, B>(source, destination) }
        } else {
            0
        };
        #[cfg(all(
            not(any(
                all(target_arch = "aarch64", target_feature = "neon"),
                all(target_arch = "arm", target_feature = "neon")
            )),
            not(target_arch = "x86_64")
        ))]
        let converted = 0;
        for x in converted..width {
            let source_pixel = &source[x * 4..x * 4 + 4];
            let destination_pixel = &mut destination[x * 3..x * 3 + 3];
            destination_pixel.copy_from_slice(&[source_pixel[R], source_pixel[G], source_pixel[B]]);
        }
    }

    Ok(())
}

/// 将带行跨度的 RGBA8888 缓冲区压缩成紧凑 RGB。
pub fn rgba8888_to_rgb_into(
    rgba: &[u8],
    width: usize,
    height: usize,
    rgba_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    packed_8888_to_rgb_into::<0, 1, 2>(rgba, width, height, rgba_stride, rgb)
}

/// 将带行跨度的 BGRA8888 缓冲区压缩成紧凑 RGB。
pub fn bgra8888_to_rgb_into(
    bgra: &[u8],
    width: usize,
    height: usize,
    bgra_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    packed_8888_to_rgb_into::<2, 1, 0>(bgra, width, height, bgra_stride, rgb)
}

/// 将带行跨度的 ARGB8888 缓冲区压缩成紧凑 RGB。
pub fn argb8888_to_rgb_into(
    argb: &[u8],
    width: usize,
    height: usize,
    argb_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    packed_8888_to_rgb_into::<1, 2, 3>(argb, width, height, argb_stride, rgb)
}

/// 将双平面 YUV420SP（UV 交错）转换为紧凑 RGB。
///
/// 每个 UV 对服务同一行中的两个像素，避免逐像素重复读取色度和执行除法。
#[cfg(not(any(
    all(target_arch = "aarch64", target_feature = "neon"),
    all(target_arch = "arm", target_feature = "neon")
)))]
fn yuv420sp_bt601_full_to_rgb_into(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: usize,
    height: usize,
    y_stride: usize,
    uv_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    if width == 0 || height == 0 || y_stride < width {
        return Err(CameraError::InvalidFormat(
            "invalid YUV420 dimensions or Y stride".to_string(),
        ));
    }

    let chroma_row_bytes = width
        .div_ceil(2)
        .checked_mul(2)
        .ok_or_else(|| CameraError::InvalidFormat("YUV420 size overflow".to_string()))?;
    if uv_stride < chroma_row_bytes {
        return Err(CameraError::InvalidFormat(
            "invalid YUV420 UV stride".to_string(),
        ));
    }

    let required_y = (height - 1)
        .checked_mul(y_stride)
        .and_then(|size| size.checked_add(width))
        .ok_or_else(|| CameraError::InvalidFormat("YUV420 Y size overflow".to_string()))?;
    let chroma_rows = height.div_ceil(2);
    let required_uv = (chroma_rows - 1)
        .checked_mul(uv_stride)
        .and_then(|size| size.checked_add(chroma_row_bytes))
        .ok_or_else(|| CameraError::InvalidFormat("YUV420 UV size overflow".to_string()))?;
    let required_rgb = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".to_string()))?;

    if y_plane.len() < required_y || uv_plane.len() < required_uv || rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "YUV420 or RGB buffer is too small".to_string(),
        ));
    }

    for row in 0..height {
        let y_row = &y_plane[row * y_stride..];
        let uv_row = &uv_plane[(row / 2) * uv_stride..];
        let rgb_row_start = row * width * 3;

        for column in (0..width).step_by(2) {
            let uv_offset = (column / 2) * 2;
            let u = uv_row[uv_offset] as i32 - 128;
            let v = uv_row[uv_offset + 1] as i32 - 128;
            let red_chroma = BT601_FULL_COEFFICIENTS[1] * v + 512;
            let green_chroma =
                -BT601_FULL_COEFFICIENTS[2] * u - BT601_FULL_COEFFICIENTS[3] * v + 512;
            let blue_chroma = BT601_FULL_COEFFICIENTS[4] * u + 512;

            let write_pixel = |y: u8, destination: &mut [u8]| {
                let y = i32::from(y) * BT601_FULL_COEFFICIENTS[0];
                destination[0] = ((y + red_chroma) >> 10).clamp(0, 255) as u8;
                destination[1] = ((y + green_chroma) >> 10).clamp(0, 255) as u8;
                destination[2] = ((y + blue_chroma) >> 10).clamp(0, 255) as u8;
            };

            let first_rgb = rgb_row_start + column * 3;
            write_pixel(y_row[column], &mut rgb[first_rgb..first_rgb + 3]);
            if column + 1 < width {
                let second_rgb = first_rgb + 3;
                write_pixel(y_row[column + 1], &mut rgb[second_rgb..second_rgb + 3]);
            }
        }
    }

    Ok(())
}

pub fn yuv420sp_to_rgb_into(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: usize,
    height: usize,
    y_stride: usize,
    uv_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    yuv420sp_to_rgb_with_coefficients_into(
        Yuv420Sp {
            y: y_plane,
            uv: uv_plane,
            y_stride,
            uv_stride,
            vu_order: false,
        },
        width,
        height,
        BT601_FULL_COEFFICIENTS,
        rgb,
    )
}

// ============================================================================
// 便捷函数
// ============================================================================

/// YUYV 转 RGB（使用默认配置）
pub fn yuyv_to_rgb(yuyv_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    ColorConverter::new().yuyv_to_rgb(yuyv_data, width, height)
}

/// UYVY 转 RGB（使用默认配置）
pub fn uyvy_to_rgb(uyvy_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    ColorConverter::new().uyvy_to_rgb(uyvy_data, width, height)
}

/// YUYV 转 RGB（使用预分配缓冲区）
pub fn yuyv_to_rgb_into(
    yuyv_data: &[u8],
    width: u32,
    height: u32,
    rgb_buffer: &mut [u8],
) -> Result<()> {
    ColorConverter::new().yuyv_to_rgb_into(yuyv_data, width, height, rgb_buffer)
}

/// UYVY 转 RGB（使用预分配缓冲区）
pub fn uyvy_to_rgb_into(
    uyvy_data: &[u8],
    width: u32,
    height: u32,
    rgb_buffer: &mut [u8],
) -> Result<()> {
    ColorConverter::new().uyvy_to_rgb_into(uyvy_data, width, height, rgb_buffer)
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yuyv_to_rgb_basic() {
        // 创建一个简单的 YUYV 测试数据（2x2 像素，白色）
        // 白色: Y=255, U=128, V=128
        let yuyv_data = vec![255, 128, 255, 128, 255, 128, 255, 128];
        let result = yuyv_to_rgb(&yuyv_data, 2, 2).unwrap();

        // 验证结果是接近白色的
        assert_eq!(result.len(), 12); // 4 像素 * 3 字节
        for chunk in result.chunks(3) {
            // 允许一定的误差
            assert!(chunk[0] >= 250, "R should be ~255, got {}", chunk[0]);
            assert!(chunk[1] >= 250, "G should be ~255, got {}", chunk[1]);
            assert!(chunk[2] >= 250, "B should be ~255, got {}", chunk[2]);
        }
    }

    #[test]
    fn test_yuyv_to_rgb_black() {
        // 黑色: Y=0, U=128, V=128
        let yuyv_data = vec![0, 128, 0, 128, 0, 128, 0, 128];
        let result = yuyv_to_rgb(&yuyv_data, 2, 2).unwrap();

        assert_eq!(result.len(), 12);
        for chunk in result.chunks(3) {
            assert!(chunk[0] <= 5, "R should be ~0, got {}", chunk[0]);
            assert!(chunk[1] <= 5, "G should be ~0, got {}", chunk[1]);
            assert!(chunk[2] <= 5, "B should be ~0, got {}", chunk[2]);
        }
    }

    #[test]
    fn test_uyvy_to_rgb_basic() {
        // UYVY 格式的白色
        let uyvy_data = vec![128, 255, 128, 255, 128, 255, 128, 255];
        let result = uyvy_to_rgb(&uyvy_data, 2, 2).unwrap();

        assert_eq!(result.len(), 12);
        for chunk in result.chunks(3) {
            assert!(chunk[0] >= 250, "R should be ~255, got {}", chunk[0]);
            assert!(chunk[1] >= 250, "G should be ~255, got {}", chunk[1]);
            assert!(chunk[2] >= 250, "B should be ~255, got {}", chunk[2]);
        }
    }

    #[test]
    fn test_yuyv_to_rgb_into() {
        let yuyv_data = vec![255, 128, 255, 128];
        let mut rgb_buffer = vec![0u8; 6];

        yuyv_to_rgb_into(&yuyv_data, 2, 1, &mut rgb_buffer).unwrap();

        // 验证缓冲区被正确填充
        for chunk in rgb_buffer.chunks(3) {
            assert!(chunk[0] >= 250);
            assert!(chunk[1] >= 250);
            assert!(chunk[2] >= 250);
        }
    }

    #[test]
    fn test_yuv420sp_to_rgb_reuses_each_uv_pair_for_two_pixels() {
        let y_plane = [0, 255, 64, 128];
        let uv_plane = [128, 128];
        let mut rgb = [0u8; 12];

        yuv420sp_to_rgb_into(&y_plane, &uv_plane, 2, 2, 2, 2, &mut rgb).expect("YUV420 conversion");

        assert_eq!(&rgb[0..3], &[0, 0, 0]);
        assert_eq!(&rgb[3..6], &[255, 255, 255]);
        assert_eq!(&rgb[6..9], &[64, 64, 64]);
        assert_eq!(&rgb[9..12], &[128, 128, 128]);
    }

    #[test]
    fn test_yuv420sp_to_rgb_rejects_invalid_plane_stride() {
        let mut rgb = [0u8; 12];

        let error = yuv420sp_to_rgb_into(&[0; 4], &[128; 2], 2, 2, 1, 2, &mut rgb)
            .expect_err("short Y stride must fail");

        assert!(error.to_string().contains("stride"));
    }

    #[test]
    fn test_rgba8888_to_rgb_removes_alpha_and_respects_row_stride() {
        let rgba = [10, 20, 30, 40, 50, 60, 70, 80, 0, 0, 0, 0];
        let mut rgb = [0u8; 6];

        rgba8888_to_rgb_into(&rgba, 2, 1, 12, &mut rgb).expect("RGBA conversion");

        assert_eq!(rgb, [10, 20, 30, 50, 60, 70]);
    }

    #[test]
    fn packed_8888_channel_orders_share_checked_conversion() {
        let bgra = [30, 20, 10, 40, 70, 60, 50, 80, 0, 0, 0, 0];
        let argb = [40, 10, 20, 30, 80, 50, 60, 70, 0, 0, 0, 0];
        let mut rgb = [0u8; 6];

        bgra8888_to_rgb_into(&bgra, 2, 1, 12, &mut rgb).expect("BGRA conversion");
        assert_eq!(rgb, [10, 20, 30, 50, 60, 70]);
        argb8888_to_rgb_into(&argb, 2, 1, 12, &mut rgb).expect("ARGB conversion");
        assert_eq!(rgb, [10, 20, 30, 50, 60, 70]);

        assert!(bgra8888_to_rgb_into(&bgra[..7], 2, 1, 8, &mut rgb).is_err());
        assert!(argb8888_to_rgb_into(&argb, 2, 1, 7, &mut rgb).is_err());
    }

    #[test]
    fn configured_packed_422_kernel_matches_color_reference() {
        let color = crate::ColorInfo {
            matrix: crate::ColorMatrix::Bt709,
            range: crate::ColorRange::Limited,
            ..Default::default()
        };
        let coefficients = color_coefficients(color).unwrap();
        let width = 34;
        let height = 3;
        let stride = width * 2 + 6;
        let mut source = vec![0; stride * height];
        for (index, byte) in source.iter_mut().enumerate() {
            *byte = index.wrapping_mul(37).wrapping_add(19) as u8;
        }
        let mut actual = vec![0; width * height * 3];
        yuv422_to_rgb_with_coefficients_into(
            &source,
            width,
            height,
            stride,
            false,
            coefficients,
            &mut actual,
        )
        .unwrap();

        let mut expected = vec![0; actual.len()];
        for row in 0..height {
            for column in (0..width).step_by(2) {
                let input = row * stride + column * 2;
                let output = (row * width + column) * 3;
                yuv_pixel(
                    coefficients,
                    source[input],
                    source[input + 1],
                    source[input + 3],
                    &mut expected[output..output + 3],
                );
                yuv_pixel(
                    coefficients,
                    source[input + 2],
                    source[input + 1],
                    source[input + 3],
                    &mut expected[output + 3..output + 6],
                );
            }
        }
        assert_eq!(actual, expected);

        let mut uyvy = source.clone();
        for row in 0..height {
            for column in (0..width).step_by(2) {
                let offset = row * stride + column * 2;
                let [y0, u, y1, v] = source[offset..offset + 4] else {
                    unreachable!()
                };
                uyvy[offset..offset + 4].copy_from_slice(&[u, y0, v, y1]);
            }
        }
        yuv422_to_rgb_with_coefficients_into(
            &uyvy,
            width,
            height,
            stride,
            true,
            coefficients,
            &mut actual,
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn configured_yuv420_kernels_match_reference_for_all_color_modes_and_tails() {
        let (width, height): (usize, usize) = (19, 7);
        let y_stride = width + 3;
        let uv_stride = width.div_ceil(2) * 2 + 4;
        let chroma_stride = width.div_ceil(2) + 2;
        let mut y = vec![0; y_stride * height];
        let mut uv = vec![0; uv_stride * height.div_ceil(2)];
        let mut u = vec![0; chroma_stride * height.div_ceil(2)];
        let mut v = vec![0; chroma_stride * height.div_ceil(2)];
        for row in 0..height {
            for column in 0..width {
                y[row * y_stride + column] = match column % 11 {
                    0 => 0,
                    1 => 255,
                    _ => row.wrapping_mul(31).wrapping_add(column * 17) as u8,
                };
            }
        }
        for row in 0..height.div_ceil(2) {
            for column in 0..width.div_ceil(2) {
                let uu = match column % 7 {
                    0 => 0,
                    1 => 255,
                    _ => row.wrapping_mul(47).wrapping_add(column * 29) as u8,
                };
                let vv = match column % 7 {
                    0 => 255,
                    1 => 0,
                    _ => row.wrapping_mul(13).wrapping_add(column * 53) as u8,
                };
                uv[row * uv_stride + column * 2] = uu;
                uv[row * uv_stride + column * 2 + 1] = vv;
                u[row * chroma_stride + column] = uu;
                v[row * chroma_stride + column] = vv;
            }
        }
        let mut vu = uv.clone();
        for row in 0..height.div_ceil(2) {
            for column in 0..width.div_ceil(2) {
                let offset = row * uv_stride + column * 2;
                vu.swap(offset, offset + 1);
            }
        }
        for matrix in [
            crate::ColorMatrix::Bt601,
            crate::ColorMatrix::Bt709,
            crate::ColorMatrix::Bt2020,
            crate::ColorMatrix::Smpte240M,
        ] {
            for range in [crate::ColorRange::Full, crate::ColorRange::Limited] {
                let coefficients = color_coefficients(crate::ColorInfo {
                    matrix,
                    range,
                    ..Default::default()
                })
                .unwrap();
                let mut expected = vec![0; width * height * 3];
                for row in 0..height {
                    for column in 0..width {
                        yuv_pixel(
                            coefficients,
                            y[row * y_stride + column],
                            u[(row / 2) * chroma_stride + column / 2],
                            v[(row / 2) * chroma_stride + column / 2],
                            &mut expected[(row * width + column) * 3..][..3],
                        );
                    }
                }
                let mut semiplanar = vec![0; expected.len()];
                yuv420sp_to_rgb_with_coefficients_into(
                    Yuv420Sp {
                        y: &y,
                        uv: &uv,
                        y_stride,
                        uv_stride,
                        vu_order: false,
                    },
                    width,
                    height,
                    coefficients,
                    &mut semiplanar,
                )
                .unwrap();
                assert_eq!(semiplanar, expected, "{matrix:?} {range:?} NV12");

                yuv420sp_to_rgb_with_coefficients_into(
                    Yuv420Sp {
                        y: &y,
                        uv: &vu,
                        y_stride,
                        uv_stride,
                        vu_order: true,
                    },
                    width,
                    height,
                    coefficients,
                    &mut semiplanar,
                )
                .unwrap();
                assert_eq!(semiplanar, expected, "{matrix:?} {range:?} NV21");

                let mut planar = vec![0; expected.len()];
                yuv420_to_rgb_with_coefficients_into(
                    YuvPlane {
                        data: &y,
                        row_stride: y_stride,
                        pixel_stride: 1,
                    },
                    YuvPlane {
                        data: &u,
                        row_stride: chroma_stride,
                        pixel_stride: 1,
                    },
                    YuvPlane {
                        data: &v,
                        row_stride: chroma_stride,
                        pixel_stride: 1,
                    },
                    width,
                    height,
                    coefficients,
                    &mut planar,
                )
                .unwrap();
                assert_eq!(planar, expected, "{matrix:?} {range:?} I420");
            }
        }
    }

    #[test]
    fn configured_nv12_half_kernel_matches_direct_sampling_reference() {
        let (width, height): (usize, usize) = (34, 10);
        let y_stride = width + 5;
        let uv_stride = width + 4;
        let mut y = vec![0; y_stride * height];
        let mut uv = vec![0; uv_stride * height.div_ceil(2)];
        for (index, byte) in y.iter_mut().enumerate() {
            *byte = index.wrapping_mul(31).wrapping_add(17) as u8;
        }
        for (index, byte) in uv.iter_mut().enumerate() {
            *byte = index.wrapping_mul(43).wrapping_add(23) as u8;
        }
        let coefficients = color_coefficients(crate::ColorInfo {
            matrix: crate::ColorMatrix::Bt709,
            range: crate::ColorRange::Limited,
            ..Default::default()
        })
        .unwrap();
        let (output_width, output_height) = (width / 2, height / 2);
        let mut expected = vec![0; output_width * output_height * 3];
        for row in 0..output_height {
            for column in 0..output_width {
                let chroma = row * uv_stride + column * 2;
                yuv_pixel(
                    coefficients,
                    y[row * 2 * y_stride + column * 2],
                    uv[chroma],
                    uv[chroma + 1],
                    &mut expected[(row * output_width + column) * 3..][..3],
                );
            }
        }
        let mut actual = vec![0; expected.len()];
        yuv420sp_to_rgb_half_with_coefficients_into(
            Yuv420Sp {
                y: &y,
                uv: &uv,
                y_stride,
                uv_stride,
                vu_order: false,
            },
            width,
            height,
            coefficients,
            &mut actual,
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn configured_packed_and_planar_half_kernels_match_direct_sampling_reference() {
        let (width, height): (usize, usize) = (34, 10);
        let y_stride = width + 5;
        let chroma_stride = width / 2 + 3;
        let packed_stride = width * 2 + 7;
        let mut y = vec![0; y_stride * height];
        let mut u = vec![0; chroma_stride * (height / 2)];
        let mut v = vec![0; chroma_stride * (height / 2)];
        let mut yuyv = vec![0; packed_stride * height];
        let mut uyvy = vec![0; packed_stride * height];
        for row in 0..height {
            for column in 0..width {
                y[row * y_stride + column] = row.wrapping_mul(37).wrapping_add(column * 19) as u8;
            }
            for pair in 0..width / 2 {
                let chroma_row = row / 2;
                let uu = chroma_row.wrapping_mul(41).wrapping_add(pair * 23) as u8;
                let vv = chroma_row.wrapping_mul(29).wrapping_add(pair * 47) as u8;
                u[chroma_row * chroma_stride + pair] = uu;
                v[chroma_row * chroma_stride + pair] = vv;
                let input = row * packed_stride + pair * 4;
                let y0 = y[row * y_stride + pair * 2];
                let y1 = y[row * y_stride + pair * 2 + 1];
                yuyv[input..input + 4].copy_from_slice(&[y0, uu, y1, vv]);
                uyvy[input..input + 4].copy_from_slice(&[uu, y0, vv, y1]);
            }
        }
        let coefficients = color_coefficients(crate::ColorInfo {
            matrix: crate::ColorMatrix::Bt709,
            range: crate::ColorRange::Limited,
            ..Default::default()
        })
        .unwrap();
        let (output_width, output_height) = (width / 2, height / 2);
        let mut expected = vec![0; output_width * output_height * 3];
        for row in 0..output_height {
            for column in 0..output_width {
                yuv_pixel(
                    coefficients,
                    y[row * 2 * y_stride + column * 2],
                    u[row * chroma_stride + column],
                    v[row * chroma_stride + column],
                    &mut expected[(row * output_width + column) * 3..][..3],
                );
            }
        }

        for (packed, is_uyvy) in [(&yuyv, false), (&uyvy, true)] {
            let mut actual = vec![0; expected.len()];
            yuv422_to_rgb_half_with_coefficients_into(
                packed,
                width,
                height,
                packed_stride,
                is_uyvy,
                coefficients,
                &mut actual,
            )
            .unwrap();
            assert_eq!(actual, expected, "packed UYVY={is_uyvy}");
        }

        let mut planar = vec![0; expected.len()];
        yuv420_to_rgb_half_with_coefficients_into(
            YuvPlane {
                data: &y,
                row_stride: y_stride,
                pixel_stride: 1,
            },
            YuvPlane {
                data: &u,
                row_stride: chroma_stride,
                pixel_stride: 1,
            },
            YuvPlane {
                data: &v,
                row_stride: chroma_stride,
                pixel_stride: 1,
            },
            width,
            height,
            coefficients,
            &mut planar,
        )
        .unwrap();
        assert_eq!(planar, expected);
    }

    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    #[test]
    fn neon_extreme_chroma_and_unaligned_tails_match_scalar() {
        for width in (2..130).step_by(2) {
            let bytes: Vec<u8> = (0..width * 2 * 3 + 1)
                .map(|i| {
                    let x = (i as u32).wrapping_mul(747796405).wrapping_add(2891336453);
                    ((x ^ (x >> 13)) >> 16) as u8
                })
                .collect();
            let input = &bytes[1..];
            let mut expected = vec![0; width * 3 * 3];
            let mut output = vec![19; width * 3 * 3 + 2];
            yuyv_to_rgb_scalar_into(input, &mut expected).unwrap();
            unsafe {
                neon::yuyv_to_rgb_neon(input, &mut output[1..=expected.len()]);
            }
            assert_eq!(
                &output[1..=expected.len()],
                expected.as_slice(),
                "width={width}"
            );
            assert_eq!(output[0], 19);
            assert_eq!(output[expected.len() + 1], 19);
        }
    }

    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    #[test]
    fn neon_contiguous_planar_row_matches_reference_without_overwrite() {
        let coefficients = color_coefficients(ColorInfo {
            matrix: ColorMatrix::Bt2020,
            range: ColorRange::Limited,
            ..Default::default()
        })
        .unwrap();
        for width in [16_usize, 32, 48] {
            let mut y_storage = vec![0x5a; width + 2];
            let mut u_storage = vec![0xa5; width / 2 + 2];
            let mut v_storage = vec![0x3c; width / 2 + 2];
            for (index, value) in y_storage[1..=width].iter_mut().enumerate() {
                *value = index.wrapping_mul(37).wrapping_add(19) as u8;
            }
            for (index, value) in u_storage[1..=width / 2].iter_mut().enumerate() {
                *value = index.wrapping_mul(53).wrapping_add(11) as u8;
            }
            for (index, value) in v_storage[1..=width / 2].iter_mut().enumerate() {
                *value = index.wrapping_mul(71).wrapping_add(23) as u8;
            }
            let y = &y_storage[1..=width];
            let u = &u_storage[1..=width / 2];
            let v = &v_storage[1..=width / 2];
            let mut actual = vec![0xcc; width * 3 + 2];
            let converted = unsafe {
                neon::planar_contiguous_row_with_coefficients(
                    y,
                    u,
                    v,
                    coefficients,
                    &mut actual[1..=width * 3],
                )
            };
            assert_eq!(converted, width);
            let mut expected = vec![0; width * 3];
            for column in 0..width {
                yuv_pixel(
                    coefficients,
                    y[column],
                    u[column / 2],
                    v[column / 2],
                    &mut expected[column * 3..column * 3 + 3],
                );
            }
            assert_eq!(&actual[1..=width * 3], expected);
            assert_eq!((actual[0], actual[width * 3 + 1]), (0xcc, 0xcc));
        }
    }

    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    #[test]
    fn neon_contiguous_planar_two_rows_match_reference_without_overwrite() {
        let coefficients = color_coefficients(ColorInfo {
            matrix: ColorMatrix::Bt709,
            range: ColorRange::Limited,
            ..Default::default()
        })
        .unwrap();
        for width in [16_usize, 32, 48] {
            let y0: Vec<u8> = (0..width).map(|x| (x * 37 + 19) as u8).collect();
            let y1: Vec<u8> = (0..width).map(|x| (x * 29 + 7) as u8).collect();
            let u: Vec<u8> = (0..width / 2).map(|x| (x * 53 + 11) as u8).collect();
            let v: Vec<u8> = (0..width / 2).map(|x| (x * 71 + 23) as u8).collect();
            let mut row0 = vec![0xcc; width * 3 + 2];
            let mut row1 = vec![0xdd; width * 3 + 2];
            let converted = unsafe {
                neon::planar_contiguous_two_rows_with_coefficients(
                    &y0,
                    &y1,
                    &u,
                    &v,
                    coefficients,
                    &mut row0[1..=width * 3],
                    &mut row1[1..=width * 3],
                )
            };
            assert_eq!(converted, width);
            for column in 0..width {
                let mut expected0 = [0; 3];
                let mut expected1 = [0; 3];
                yuv_pixel(
                    coefficients,
                    y0[column],
                    u[column / 2],
                    v[column / 2],
                    &mut expected0,
                );
                yuv_pixel(
                    coefficients,
                    y1[column],
                    u[column / 2],
                    v[column / 2],
                    &mut expected1,
                );
                assert_eq!(&row0[1 + column * 3..][..3], &expected0);
                assert_eq!(&row1[1 + column * 3..][..3], &expected1);
            }
            assert_eq!((row0[0], row0[width * 3 + 1]), (0xcc, 0xcc));
            assert_eq!((row1[0], row1[width * 3 + 1]), (0xdd, 0xdd));
        }
    }

    #[cfg(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "arm", target_feature = "neon")
    ))]
    #[test]
    fn neon_semiplanar_two_rows_match_reference_without_overwrite() {
        let coefficients = color_coefficients(ColorInfo {
            matrix: ColorMatrix::Bt709,
            range: ColorRange::Limited,
            ..Default::default()
        })
        .unwrap();
        for vu_order in [false, true] {
            for width in [16_usize, 32, 48] {
                let y0: Vec<u8> = (0..width).map(|x| (x * 37 + 19) as u8).collect();
                let y1: Vec<u8> = (0..width).map(|x| (x * 29 + 7) as u8).collect();
                let uv: Vec<u8> = (0..width).map(|x| (x * 53 + 11) as u8).collect();
                let mut row0 = vec![0xcc; width * 3 + 2];
                let mut row1 = vec![0xdd; width * 3 + 2];
                let converted = unsafe {
                    neon::semiplanar_two_rows_with_coefficients(
                        &y0,
                        &y1,
                        &uv,
                        vu_order,
                        coefficients,
                        &mut row0[1..=width * 3],
                        &mut row1[1..=width * 3],
                    )
                };
                assert_eq!(converted, width);
                for column in 0..width {
                    let chroma = column / 2 * 2;
                    let (u, v) = if vu_order {
                        (uv[chroma + 1], uv[chroma])
                    } else {
                        (uv[chroma], uv[chroma + 1])
                    };
                    let mut expected0 = [0; 3];
                    let mut expected1 = [0; 3];
                    yuv_pixel(coefficients, y0[column], u, v, &mut expected0);
                    yuv_pixel(coefficients, y1[column], u, v, &mut expected1);
                    assert_eq!(&row0[1 + column * 3..][..3], &expected0);
                    assert_eq!(&row1[1 + column * 3..][..3], &expected1);
                }
                assert_eq!((row0[0], row0[width * 3 + 1]), (0xcc, 0xcc));
                assert_eq!((row1[0], row1[width * 3 + 1]), (0xdd, 0xdd));
            }
        }
    }

    #[test]
    fn packed_dispatch_matches_scalar_for_all_uv_pairs() {
        let input: Vec<u8> = (0..65536u32)
            .flat_map(|uv| [uv as u8 ^ 91, (uv >> 8) as u8, uv as u8 ^ 203, uv as u8])
            .collect();
        let mut expected = vec![0; input.len() / 2 * 3];
        let mut actual = expected.clone();
        yuyv_to_rgb_scalar_into(&input, &mut expected).unwrap();
        yuyv_to_rgb_simd_into(&input, &mut actual).unwrap();
        assert_eq!(actual, expected);
        uyvy_to_rgb_scalar_into(&input, &mut expected).unwrap();
        uyvy_to_rgb_simd_into(&input, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn planar_simd_matches_reference_with_padding_and_short_last_rows() {
        for width in [1usize, 2, 7, 8, 9, 17, 32, 35] {
            for pixel_stride in [1, 2] {
                let height = 3;
                let ys = width + 3;
                let cs = (width.div_ceil(2) - 1) * pixel_stride + 3;
                let yy: Vec<u8> = (0..ys * 2 + width).map(|i| (i * 71) as u8).collect();
                let uu: Vec<u8> = (0..cs + (width.div_ceil(2) - 1) * pixel_stride + 1)
                    .map(|i| (i * 139) as u8)
                    .collect();
                let vv: Vec<u8> = (0..uu.len()).map(|i| (i * 29 + 255) as u8).collect();
                let mut actual = vec![0; width * height * 3];
                yuv420_to_rgb_into(
                    YuvPlane {
                        data: &yy,
                        row_stride: ys,
                        pixel_stride: 1,
                    },
                    YuvPlane {
                        data: &uu,
                        row_stride: cs,
                        pixel_stride,
                    },
                    YuvPlane {
                        data: &vv,
                        row_stride: cs,
                        pixel_stride,
                    },
                    width,
                    height,
                    &mut actual,
                )
                .unwrap();
                for row in 0..height {
                    for col in 0..width {
                        let mut expected = [0; 3];
                        yuv_pixel(
                            BT601_FULL_COEFFICIENTS,
                            yy[row * ys + col],
                            uu[row / 2 * cs + col / 2 * pixel_stride],
                            vv[row / 2 * cs + col / 2 * pixel_stride],
                            &mut expected,
                        );
                        assert_eq!(&actual[(row * width + col) * 3..][..3], &expected);
                    }
                }
            }
        }
    }

    #[test]
    fn test_scalar_vs_simd_consistency() {
        // 创建随机测试数据
        let width = 64;
        let height = 48;
        let yuyv_data: Vec<u8> = (0..(width * height * 2)).map(|i| (i % 256) as u8).collect();

        // 使用标量实现
        let scalar_result = yuyv_to_rgb_scalar(&yuyv_data, width, height).unwrap();

        // 使用 SIMD 实现
        let simd_result = yuyv_to_rgb_simd(&yuyv_data, width, height).unwrap();

        // 验证结果一致
        assert_eq!(scalar_result.len(), simd_result.len());
        for (i, (s, m)) in scalar_result.iter().zip(simd_result.iter()).enumerate() {
            assert_eq!(s, m, "Mismatch at index {}", i);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_avx2_packed_422_matches_scalar_for_unaligned_inputs_and_tails() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        for uyvy in [false, true] {
            for pairs in 1..=33 {
                let input_len = pairs * 4;
                let output_len = pairs * 6;
                let mut state = (pairs as u32).wrapping_mul(0x9e37_79b9);
                let mut input = vec![0x5a; input_len + 2];
                for value in &mut input[1..=input_len] {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    *value = (state >> 24) as u8;
                }
                let source = &input[1..=input_len];
                let mut expected = vec![0xa5; output_len + 2];
                let mut actual = vec![0xa5; output_len + 2];
                if uyvy {
                    uyvy_to_rgb_scalar_into(source, &mut expected[1..=output_len]).unwrap();
                    unsafe {
                        x86::packed422_avx2::<true, 8, 0, 255>(
                            source,
                            &mut actual[1..=output_len],
                            [256, 359, 88, 183, 454, 0],
                        );
                    }
                } else {
                    yuyv_to_rgb_scalar_into(source, &mut expected[1..=output_len]).unwrap();
                    unsafe {
                        x86::packed422_avx2::<false, 8, 0, 255>(
                            source,
                            &mut actual[1..=output_len],
                            [256, 359, 88, 183, 454, 0],
                        );
                    }
                }
                assert_eq!(actual, expected, "uyvy={uyvy} pairs={pairs}");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_avx2_packed_8888_matches_all_channel_orders_without_overwrite() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        fn check<const R: usize, const G: usize, const B: usize>() {
            for pixels in 1..=41 {
                let mut source = vec![0x5a; pixels * 4 + 2];
                for (index, value) in source[1..=pixels * 4].iter_mut().enumerate() {
                    *value = index.wrapping_mul(37).wrapping_add(19) as u8;
                }
                let source = &source[1..=pixels * 4];
                let converted = pixels / 8 * 8;
                let mut actual = vec![0xcc; pixels * 3 + 2];
                let result = unsafe {
                    x86::packed_8888_row_avx2::<R, G, B>(source, &mut actual[1..=pixels * 3])
                };
                assert_eq!(result, converted);
                for pixel in 0..converted {
                    assert_eq!(
                        &actual[1 + pixel * 3..][..3],
                        &[
                            source[pixel * 4 + R],
                            source[pixel * 4 + G],
                            source[pixel * 4 + B],
                        ]
                    );
                }
                assert!(actual[1 + converted * 3..=pixels * 3]
                    .iter()
                    .all(|value| *value == 0xcc));
                assert_eq!((actual[0], actual[pixels * 3 + 1]), (0xcc, 0xcc));
            }
        }
        check::<0, 1, 2>();
        check::<2, 1, 0>();
        check::<1, 2, 3>();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_ssse3_packed_8888_matches_all_channel_orders_without_overwrite() {
        if !std::arch::is_x86_feature_detected!("ssse3") {
            return;
        }
        fn check<const R: usize, const G: usize, const B: usize>() {
            for pixels in 1..=41 {
                let mut source = vec![0x5a; pixels * 4 + 2];
                for (index, value) in source[1..=pixels * 4].iter_mut().enumerate() {
                    *value = index.wrapping_mul(37).wrapping_add(19) as u8;
                }
                let source = &source[1..=pixels * 4];
                let converted = pixels / 4 * 4;
                let mut actual = vec![0xcc; pixels * 3 + 2];
                let result = unsafe {
                    x86::packed_8888_row_ssse3::<R, G, B>(source, &mut actual[1..=pixels * 3])
                };
                assert_eq!(result, converted);
                for pixel in 0..converted {
                    assert_eq!(
                        &actual[1 + pixel * 3..][..3],
                        &[
                            source[pixel * 4 + R],
                            source[pixel * 4 + G],
                            source[pixel * 4 + B],
                        ]
                    );
                }
                assert!(actual[1 + converted * 3..=pixels * 3]
                    .iter()
                    .all(|value| *value == 0xcc));
                assert_eq!((actual[0], actual[pixels * 3 + 1]), (0xcc, 0xcc));
            }
        }
        check::<0, 1, 2>();
        check::<2, 1, 0>();
        check::<1, 2, 3>();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_avx2_semiplanar_rows_match_configured_reference() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let coefficients = color_coefficients(ColorInfo {
            matrix: ColorMatrix::Bt709,
            range: ColorRange::Limited,
            ..Default::default()
        })
        .unwrap();
        for vu_order in [false, true] {
            for width in [16_usize, 32, 48] {
                let mut y_storage = vec![0x5a; width + 2];
                let mut uv_storage = vec![0xa5; width + 2];
                for column in 0..width {
                    y_storage[column + 1] = column.wrapping_mul(37).wrapping_add(19) as u8;
                    uv_storage[column + 1] = column.wrapping_mul(53).wrapping_add(11) as u8;
                }
                let y = &y_storage[1..=width];
                let uv = &uv_storage[1..=width];
                let mut actual = vec![0; width * 3];
                let converted = unsafe {
                    if vu_order {
                        x86::semiplanar_row_avx2::<true, 10, 512, 512>(
                            y,
                            uv,
                            &mut actual,
                            coefficients,
                        )
                    } else {
                        x86::semiplanar_row_avx2::<false, 10, 512, 512>(
                            y,
                            uv,
                            &mut actual,
                            coefficients,
                        )
                    }
                };
                assert_eq!(converted, width);
                let mut expected = vec![0; actual.len()];
                for column in 0..width {
                    let chroma = column / 2 * 2;
                    let (u, v) = if vu_order {
                        (uv[chroma + 1], uv[chroma])
                    } else {
                        (uv[chroma], uv[chroma + 1])
                    };
                    yuv_pixel(
                        coefficients,
                        y[column],
                        u,
                        v,
                        &mut expected[column * 3..column * 3 + 3],
                    );
                }
                assert_eq!(actual, expected, "vu_order={vu_order} width={width}");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_avx2_planar_rows_match_configured_reference() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let coefficients = color_coefficients(ColorInfo {
            matrix: ColorMatrix::Bt2020,
            range: ColorRange::Limited,
            ..Default::default()
        })
        .unwrap();
        for width in [16_usize, 32, 48] {
            let chroma_width = width / 2;
            let mut y_storage = vec![0x5a; width + 2];
            let mut u_storage = vec![0xa5; chroma_width + 2];
            let mut v_storage = vec![0x3c; chroma_width + 2];
            for column in 0..width {
                y_storage[column + 1] = column.wrapping_mul(37).wrapping_add(19) as u8;
            }
            for column in 0..chroma_width {
                u_storage[column + 1] = column.wrapping_mul(53).wrapping_add(11) as u8;
                v_storage[column + 1] = column.wrapping_mul(71).wrapping_add(23) as u8;
            }
            let y = &y_storage[1..=width];
            let u = &u_storage[1..=chroma_width];
            let v = &v_storage[1..=chroma_width];
            let mut actual = vec![0; width * 3];
            let converted =
                unsafe { x86::planar_row_avx2::<10, 512, 512>(y, u, v, &mut actual, coefficients) };
            assert_eq!(converted, width);
            let mut expected = vec![0; actual.len()];
            for column in 0..width {
                yuv_pixel(
                    coefficients,
                    y[column],
                    u[column / 2],
                    v[column / 2],
                    &mut expected[column * 3..column * 3 + 3],
                );
            }
            assert_eq!(actual, expected, "width={width}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_avx2_half_semiplanar_rows_match_direct_sampling_reference() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let coefficients = BT601_FULL_COEFFICIENTS;
        for vu_order in [false, true] {
            for output_width in [16_usize, 32] {
                let source_width = output_width * 2;
                let mut y_storage = vec![0x5a; source_width + 2];
                let mut uv_storage = vec![0xa5; source_width + 2];
                for column in 0..source_width {
                    y_storage[column + 1] = column.wrapping_mul(37).wrapping_add(19) as u8;
                    uv_storage[column + 1] = column.wrapping_mul(53).wrapping_add(11) as u8;
                }
                let y = &y_storage[1..=source_width];
                let uv = &uv_storage[1..=source_width];
                let mut actual = vec![0; output_width * 3];
                let converted = unsafe {
                    if vu_order {
                        x86::semiplanar_half_row_avx2::<true, 10, 512, 512>(
                            y,
                            uv,
                            &mut actual,
                            coefficients,
                        )
                    } else {
                        x86::semiplanar_half_row_avx2::<false, 10, 512, 512>(
                            y,
                            uv,
                            &mut actual,
                            coefficients,
                        )
                    }
                };
                assert_eq!(converted, output_width);
                let mut expected = vec![0; actual.len()];
                for column in 0..output_width {
                    let chroma = column * 2;
                    let (u, v) = if vu_order {
                        (uv[chroma + 1], uv[chroma])
                    } else {
                        (uv[chroma], uv[chroma + 1])
                    };
                    yuv_pixel(
                        coefficients,
                        y[column * 2],
                        u,
                        v,
                        &mut expected[column * 3..column * 3 + 3],
                    );
                }
                assert_eq!(actual, expected, "vu_order={vu_order} width={output_width}");
            }
        }
    }

    #[test]
    fn test_invalid_input_size() {
        let yuyv_data = vec![255, 128, 255]; // 太短
        let result = yuyv_to_rgb(&yuyv_data, 2, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_buffer_too_small() {
        let yuyv_data = vec![255, 128, 255, 128];
        let mut rgb_buffer = vec![0u8; 3]; // 太小

        let result = yuyv_to_rgb_into(&yuyv_data, 2, 1, &mut rgb_buffer);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_dimensions_that_overflow_frame_size_calculation() {
        let converter = ColorConverter::with_options(ColorConvertOptions { use_simd: false });
        let result = converter.yuyv_to_rgb(&[], u32::MAX, u32::MAX);
        assert!(matches!(result, Err(CameraError::InvalidFormat(_))));

        let mut rgb = [];
        let result = converter.uyvy_to_rgb_into(&[], u32::MAX, u32::MAX, &mut rgb);
        assert!(matches!(result, Err(CameraError::InvalidFormat(_))));
    }

    #[test]
    fn rejects_odd_width_for_packed_422_formats() {
        let converter = ColorConverter::with_options(ColorConvertOptions { use_simd: false });

        let result = converter.yuyv_to_rgb(&[16, 128, 16, 128, 16, 128], 3, 1);
        assert!(matches!(result, Err(CameraError::InvalidFormat(_))));

        let mut rgb = [0u8; 9];
        let result = converter.uyvy_to_rgb_into(&[128, 16, 128, 16, 128, 16], 3, 1, &mut rgb);
        assert!(matches!(result, Err(CameraError::InvalidFormat(_))));
    }

    #[test]
    fn rejects_zero_sized_frames() {
        let converter = ColorConverter::with_options(ColorConvertOptions { use_simd: false });

        assert!(matches!(
            converter.yuyv_to_rgb(&[], 0, 2),
            Err(CameraError::InvalidFormat(_))
        ));

        let mut rgb = [];
        assert!(matches!(
            converter.uyvy_to_rgb_into(&[], 2, 0, &mut rgb),
            Err(CameraError::InvalidFormat(_))
        ));
    }
}
