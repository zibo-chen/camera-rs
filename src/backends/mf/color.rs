//! Media Foundation color attributes and the compatibility policy for cameras
//! that omit them. Numeric values are defined by MFVideoTransferMatrix and
//! MFNominalRange, not by our public color enums.
use crate::{CameraError, CameraResult, ColorInfo, ColorMatrix, ColorRange};

pub(super) fn resolve_color(
    matrix: Option<u32>,
    range: Option<u32>,
    width: u32,
    height: u32,
) -> CameraResult<ColorInfo> {
    // Missing/Unknown attributes are common on capture devices. This is an
    // explicit MF-backend compatibility assumption: SD uses BT.601, HD uses
    // BT.709, and YUV defaults to studio range. Keep the generic converter
    // strict and never override an explicitly declared supported value.
    let matrix = match matrix.unwrap_or(0) {
        0 if width <= 1024 && height <= 576 => ColorMatrix::Bt601,
        0 | 1 => ColorMatrix::Bt709,
        2 => ColorMatrix::Bt601,
        3 => ColorMatrix::Smpte240M,
        4 | 5 => ColorMatrix::Bt2020,
        value => {
            return Err(CameraError::unsupported_format(format!(
                "MF YUV matrix {value}"
            )))
        }
    };
    let range = match range.unwrap_or(0) {
        0 | 2 => ColorRange::Limited,
        1 => ColorRange::Full,
        value => {
            return Err(CameraError::unsupported_format(format!(
                "MF nominal range {value}"
            )))
        }
    };
    Ok(ColorInfo {
        matrix,
        range,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorMatrix, ColorRange};

    #[test]
    fn explicit_mf_attributes_override_resolution_defaults() {
        for (value, expected) in [
            (1, ColorMatrix::Bt709),
            (2, ColorMatrix::Bt601),
            (3, ColorMatrix::Smpte240M),
            (4, ColorMatrix::Bt2020),
            (5, ColorMatrix::Bt2020),
        ] {
            for (range, expected_range) in [(1, ColorRange::Full), (2, ColorRange::Limited)] {
                let color = resolve_color(Some(value), Some(range), 640, 480).unwrap();
                assert_eq!(color.matrix, expected);
                assert_eq!(color.range, expected_range);
            }
        }
        assert_eq!(
            resolve_color(Some(2), Some(1), 1920, 1080).unwrap().matrix,
            ColorMatrix::Bt601
        );
    }

    #[test]
    fn absent_or_unspecified_attributes_use_sd_hd_limited_policy() {
        for value in [None, Some(0)] {
            for (w, h, matrix) in [
                (640, 480, ColorMatrix::Bt601),
                (720, 576, ColorMatrix::Bt601),
                (1280, 720, ColorMatrix::Bt709),
                (1920, 1080, ColorMatrix::Bt709),
            ] {
                let color = resolve_color(value, value, w, h).unwrap();
                assert_eq!(color.matrix, matrix);
                assert_eq!(color.range, ColorRange::Limited);
            }
        }
        assert_eq!(
            resolve_color(None, Some(1), 640, 480).unwrap().range,
            ColorRange::Full
        );
        assert_eq!(
            resolve_color(Some(1), None, 640, 480).unwrap().matrix,
            ColorMatrix::Bt709
        );
    }

    #[test]
    fn unsupported_explicit_attributes_are_not_silently_reinterpreted() {
        for matrix in [6, u32::MAX] {
            assert!(resolve_color(Some(matrix), None, 640, 480).is_err());
        }
        for range in [3, 4, 5, u32::MAX] {
            assert!(resolve_color(None, Some(range), 640, 480).is_err());
        }
    }

    #[cfg(feature = "convert-rgb")]
    #[test]
    fn missing_camera_attributes_allow_yuyv_and_nv12_rgb_preview() {
        use crate::{ConversionRequest, FrameLayout, PixelFormat, PlaneLayout, RgbConverter};
        for (format, data, width, height) in [
            (
                PixelFormat::Yuyv,
                vec![16, 128, 235, 128, 16, 128, 235, 128],
                640,
                480,
            ),
            (
                PixelFormat::Nv12,
                vec![16, 235, 16, 235, 128, 128],
                1280,
                720,
            ),
        ] {
            let stride = if format == PixelFormat::Yuyv { 4 } else { 2 };
            let mut layout = FrameLayout::packed(2, 2, format, stride, data.len());
            layout.color = resolve_color(None, None, width, height).unwrap();
            if format == PixelFormat::Nv12 {
                layout.planes[0].length = 4;
                layout.planes.push(PlaneLayout {
                    offset: 4,
                    length: 2,
                    row_stride: 2,
                    pixel_stride: 2,
                });
            }
            let mut rgb = [0; 12];
            RgbConverter::new()
                .convert_layout_into(
                    &layout,
                    &data,
                    ConversionRequest::for_layout(&layout),
                    &mut rgb,
                )
                .unwrap();
            assert_eq!(rgb, [0, 0, 0, 255, 255, 255, 0, 0, 0, 255, 255, 255]);
        }
    }
}
