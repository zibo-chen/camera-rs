//! 高性能颜色空间转换模块
//!
//! 提供 YUYV/UYVY 到 RGB 的高效转换，支持：
//! - 标量实现（fallback）
//! - SIMD 优化实现（使用 portable_simd 或平台特定 intrinsics）
//!
//! ARM64 uses checked NEON conversion with scalar-equivalent rounding. Other
//! targets use scalar/compiler-vectorized loops. No parallel/GPU path is enabled;
//! use the benchmark on the target device instead of assuming a speedup.

use crate::error::{CameraError, Result};

/// One Android YUV_420_888 plane. U and V may be disjoint (I420) or
/// overlapping (NV12/NV21). The final row need not include trailing padding.
#[derive(Clone, Copy)]
pub struct YuvPlane<'a> {
    pub data: &'a [u8],
    pub row_stride: usize,
    pub pixel_stride: usize,
}

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

pub fn yuv420_to_rgb_into(
    y: YuvPlane<'_>,
    u: YuvPlane<'_>,
    v: YuvPlane<'_>,
    width: usize,
    height: usize,
    rgb: &mut [u8],
) -> Result<()> {
    y.validate(width, height)?;
    u.validate(width.div_ceil(2), height.div_ceil(2))?;
    v.validate(width.div_ceil(2), height.div_ceil(2))?;
    let length = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".into()))?;
    if rgb.len() < length {
        return Err(CameraError::InvalidFormat("RGB buffer too short".into()));
    }
    for row in 0..height {
        let start = 0;
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        let start = if y.pixel_stride == 1 {
            unsafe {
                neon::planar_row(
                    &y.data[row * y.row_stride..][..width],
                    &u.data[(row / 2) * u.row_stride..],
                    &v.data[(row / 2) * v.row_stride..],
                    (u.pixel_stride, v.pixel_stride),
                    &mut rgb[row * width * 3..][..width * 3],
                )
            }
        } else {
            start
        };
        for col in (start..width).step_by(2) {
            let uu = u.data[(row / 2) * u.row_stride + (col / 2) * u.pixel_stride] as i32 - 128;
            let vv = v.data[(row / 2) * v.row_stride + (col / 2) * v.pixel_stride] as i32 - 128;
            let r = (359 * vv) >> 8;
            let g = (88 * uu + 183 * vv) >> 8;
            let b = (454 * uu) >> 8;
            for x in col..(col + 2).min(width) {
                let yy = y.data[row * y.row_stride + x * y.pixel_stride] as i32;
                let out = &mut rgb[(row * width + x) * 3..][..3];
                out[0] = (yy + r).clamp(0, 255) as u8;
                out[1] = (yy - g).clamp(0, 255) as u8;
                out[2] = (yy + b).clamp(0, 255) as u8;
            }
        }
    }
    Ok(())
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
// - x86_64: 使用循环展开帮助编译器自动向量化
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

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod neon {
    use std::arch::aarch64::*;

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

    pub unsafe fn yuyv_to_rgb_neon(source: &[u8], rgb: &mut [u8]) {
        packed422::<false>(source, rgb);
    }

    // Four chroma samples are sufficient for eight pixels. Gather exactly
    // those bytes; Android can omit the padding byte after the last sample.
    pub unsafe fn planar_row(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        chroma_stride: (usize, usize),
        rgb: &mut [u8],
    ) -> usize {
        let groups = (y.len() / 8).min(rgb.len() / 24);
        for group in 0..groups {
            let offset = group * 4;
            let uu = u32::from_le_bytes(std::array::from_fn(|i| u[(offset + i) * chroma_stride.0]));
            let vv = u32::from_le_bytes(std::array::from_fn(|i| v[(offset + i) * chroma_stride.1]));
            let u = vcreate_u8(uu as u64);
            let v = vcreate_u8(vv as u64);
            vst3_u8(
                rgb.as_mut_ptr().add(group * 24),
                rgb8(
                    vld1_u8(y.as_ptr().add(group * 8)),
                    vzip1_u8(u, u),
                    vzip1_u8(v, v),
                ),
            );
        }
        groups * 8
    }
}

fn yuyv_to_rgb_simd_into(yuyv_data: &[u8], rgb_buffer: &mut [u8]) -> Result<()> {
    // ARM64 NEON 优化路径
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        unsafe {
            neon::yuyv_to_rgb_neon(yuyv_data, rgb_buffer);
        }
        Ok(())
    }

    // 通用 SIMD 优化路径（循环展开 + 编译器自动向量化）
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
    {
        yuyv_to_rgb_simd_generic(yuyv_data, rgb_buffer)
    }
}

/// 通用 SIMD 优化实现（使用循环展开帮助编译器自动向量化）
#[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
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
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        unsafe {
            neon::packed422::<true>(uyvy_data, rgb_buffer);
        }
        Ok(())
    }
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
    uyvy_to_rgb_simd_generic(uyvy_data, rgb_buffer)
}
#[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
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

/// 将带行跨度的 RGBA8888 缓冲区压缩成紧凑 RGB。
pub fn rgba8888_to_rgb_into(
    rgba: &[u8],
    width: usize,
    height: usize,
    rgba_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    let rgba_row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| CameraError::InvalidFormat("RGBA size overflow".to_string()))?;
    let rgb_row_bytes = width
        .checked_mul(3)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".to_string()))?;
    if width == 0 || height == 0 || rgba_stride < rgba_row_bytes {
        return Err(CameraError::InvalidFormat(
            "invalid RGBA dimensions or stride".to_string(),
        ));
    }

    let required_rgba = (height - 1)
        .checked_mul(rgba_stride)
        .and_then(|size| size.checked_add(rgba_row_bytes))
        .ok_or_else(|| CameraError::InvalidFormat("RGBA size overflow".to_string()))?;
    let required_rgb = height
        .checked_mul(rgb_row_bytes)
        .ok_or_else(|| CameraError::InvalidFormat("RGB size overflow".to_string()))?;
    if rgba.len() < required_rgba || rgb.len() < required_rgb {
        return Err(CameraError::InvalidFormat(
            "RGBA or RGB buffer is too small".to_string(),
        ));
    }

    for row in 0..height {
        let source = &rgba[row * rgba_stride..row * rgba_stride + rgba_row_bytes];
        let destination = &mut rgb[row * rgb_row_bytes..(row + 1) * rgb_row_bytes];
        for (source_pixel, destination_pixel) in source
            .as_chunks::<4>()
            .0
            .iter()
            .zip(destination.as_chunks_mut::<3>().0.iter_mut())
        {
            destination_pixel.copy_from_slice(&source_pixel[..3]);
        }
    }

    Ok(())
}

/// 将双平面 YUV420SP（UV 交错）转换为紧凑 RGB。
///
/// 每个 UV 对服务同一行中的两个像素，避免逐像素重复读取色度和执行除法。
pub fn yuv420sp_to_rgb_into(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: usize,
    height: usize,
    y_stride: usize,
    uv_stride: usize,
    rgb: &mut [u8],
) -> Result<()> {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        if uv_plane.is_empty() {
            return Err(CameraError::InvalidFormat("Empty UV plane".into()));
        }
        yuv420_to_rgb_into(
            YuvPlane {
                data: y_plane,
                row_stride: y_stride,
                pixel_stride: 1,
            },
            YuvPlane {
                data: uv_plane,
                row_stride: uv_stride,
                pixel_stride: 2,
            },
            YuvPlane {
                data: &uv_plane[1..],
                row_stride: uv_stride,
                pixel_stride: 2,
            },
            width,
            height,
            rgb,
        )
    }
    // Keep the specialized packed-UV scalar loop on x86; the generic plane
    // indexing is measurably slower there. Both paths share reference tests.
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
    {
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
                let v_r = (yuv_coefficients::V_TO_R * v) >> 8;
                let uv_g = (yuv_coefficients::U_TO_G * u + yuv_coefficients::V_TO_G * v) >> 8;
                let u_b = (yuv_coefficients::U_TO_B * u) >> 8;

                let write_pixel = |y: i32, destination: &mut [u8]| {
                    destination[0] = (y + v_r).clamp(0, 255) as u8;
                    destination[1] = (y - uv_g).clamp(0, 255) as u8;
                    destination[2] = (y + u_b).clamp(0, 255) as u8;
                };

                let first_rgb = rgb_row_start + column * 3;
                write_pixel(y_row[column] as i32, &mut rgb[first_rgb..first_rgb + 3]);
                if column + 1 < width {
                    let second_rgb = first_rgb + 3;
                    write_pixel(
                        y_row[column + 1] as i32,
                        &mut rgb[second_rgb..second_rgb + 3],
                    );
                }
            }
        }

        Ok(())
    }
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

    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
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
                        let y = yy[row * ys + col] as i32;
                        let u = uu[row / 2 * cs + col / 2 * pixel_stride] as i32 - 128;
                        let v = vv[row / 2 * cs + col / 2 * pixel_stride] as i32 - 128;
                        let expected = [
                            (y + ((359 * v) >> 8)).clamp(0, 255) as u8,
                            (y - ((88 * u + 183 * v) >> 8)).clamp(0, 255) as u8,
                            (y + ((454 * u) >> 8)).clamp(0, 255) as u8,
                        ];
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
