use super::convert::*;
use crate::{CameraConfig, VideoFormat};

fn convert(
    format: VideoFormat,
    width: u32,
    height: u32,
    data: &[u8],
    stride: usize,
) -> crate::CameraResult<Vec<u8>> {
    let mut rgb = vec![0; width as usize * height as usize * 3];
    Converter::new()?.convert(
        &CameraConfig::new(format, width, height, 30),
        &[Plane { data, stride }],
        &mut rgb,
    )?;
    Ok(rgb)
}

#[test]
fn rgb_and_gray_skip_row_padding_and_accept_unpadded_last_row() {
    assert_eq!(
        convert(VideoFormat::RGB, 1, 2, &[1, 2, 3, 99, 4, 5, 6], 4).unwrap(),
        vec![1, 2, 3, 4, 5, 6]
    );
    assert_eq!(
        convert(VideoFormat::Gray, 2, 2, &[1, 2, 99, 3, 4], 3).unwrap(),
        vec![1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4]
    );
}

#[test]
fn packed_yuv_honors_stride() {
    for format in [VideoFormat::YUYV, VideoFormat::UYVY] {
        let row = if format == VideoFormat::YUYV {
            [40, 80, 200, 180]
        } else {
            [80, 40, 180, 200]
        };
        let tight = [row.as_slice(), row.as_slice()].concat();
        let padded = [row.as_slice(), &[1, 2], row.as_slice()].concat();
        assert_eq!(
            convert(format, 2, 2, &tight, 4).unwrap(),
            convert(format, 2, 2, &padded, 6).unwrap()
        );
    }
}

#[test]
fn nv12_supports_contiguous_and_separate_padded_planes() {
    let config = CameraConfig::new(VideoFormat::NV12, 2, 2, 30);
    let mut converter = Converter::new().unwrap();
    let mut separate = [0; 12];
    converter
        .convert(
            &config,
            &[
                Plane {
                    data: &[60, 80, 99, 99, 100, 120],
                    stride: 4,
                },
                Plane {
                    data: &[128, 128],
                    stride: 4,
                },
            ],
            &mut separate,
        )
        .unwrap();
    assert_eq!(
        convert(
            VideoFormat::NV12,
            2,
            2,
            &[60, 80, 99, 99, 100, 120, 99, 99, 128, 128],
            4
        )
        .unwrap(),
        separate
    );
}

#[test]
fn invalid_lengths_strides_dimensions_and_jpeg_are_errors() {
    assert!(convert(VideoFormat::RGB, 2, 2, &[0; 11], 6).is_err());
    assert!(convert(VideoFormat::RGB, 2, 2, &[0; 12], 5).is_err());
    assert!(convert(VideoFormat::NV12, 2, 2, &[0; 5], 2).is_err());
    assert!(convert(VideoFormat::YUYV, 3, 2, &[0; 12], 6).is_err());
    assert!(convert(VideoFormat::MJPEG, 2, 2, &[0; 10], 0).is_err());
    assert!(convert(VideoFormat::H264, 2, 2, &[0; 10], 0).is_err());
    assert!(convert(VideoFormat::RGB, 0, 1, &[], 0).is_err());
}

#[test]
fn fourcc_round_trip_and_unsupported_formats() {
    for format in [
        VideoFormat::MJPEG,
        VideoFormat::YUYV,
        VideoFormat::UYVY,
        VideoFormat::NV12,
        VideoFormat::RGB,
        VideoFormat::Gray,
    ] {
        assert_eq!(from_fourcc(to_fourcc(format).unwrap()), Some(format));
    }
    assert_eq!(
        from_fourcc(u32::from_le_bytes(*b"NM12")),
        Some(VideoFormat::NV12)
    );
    assert_eq!(from_fourcc(u32::from_le_bytes(*b"H264")), None);
    assert!(to_fourcc(VideoFormat::H264).is_err());
}

#[test]
fn limited_range_black_white_and_709_matrix_are_respected() {
    let config = CameraConfig::new(VideoFormat::YUYV, 2, 1, 30);
    let mut rgb = [0; 6];
    let mut converter = Converter::new().unwrap();
    converter.set_colorimetry(2, false).unwrap();
    converter
        .convert(
            &config,
            &[Plane {
                data: &[16, 128, 235, 128],
                stride: 4,
            }],
            &mut rgb,
        )
        .unwrap();
    assert_eq!(rgb, [0, 0, 0, 255, 255, 255]);
    converter
        .convert(
            &config,
            &[Plane {
                data: &[81, 90, 81, 240],
                stride: 4,
            }],
            &mut rgb,
        )
        .unwrap();
    assert_eq!(rgb, [255, 24, 0, 255, 24, 0]);
    assert!(converter.set_colorimetry(7, false).is_err());
}

#[cfg(feature = "decode-mjpeg")]
#[test]
fn jpeg_decodes_into_rgb_and_rejects_mismatched_dimensions() {
    let pixels = [100u8; 12];
    let jpeg = turbojpeg::compress(
        turbojpeg::Image {
            pixels: &pixels,
            width: 2,
            pitch: 6,
            height: 2,
            format: turbojpeg::PixelFormat::RGB,
        },
        100,
        turbojpeg::Subsamp::None,
    )
    .unwrap();
    let rgb = convert(VideoFormat::MJPEG, 2, 2, &jpeg, 0).unwrap();
    assert!(rgb.iter().all(|&v| v.abs_diff(100) <= 1));
    assert!(convert(VideoFormat::MJPEG, 4, 2, &jpeg, 0).is_err());
}
