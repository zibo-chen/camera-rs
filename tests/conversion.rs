use camera::{
    ColorInfo, ColorMatrix, ColorRange, ConversionRequest, FrameLayout, Orientation, PixelFormat,
    PlaneLayout, RgbConverter,
};
fn packed(format: PixelFormat, stride: usize, pixel_stride: usize, length: usize) -> FrameLayout {
    FrameLayout {
        width: 2,
        height: 2,
        format,
        planes: vec![PlaneLayout {
            offset: 0,
            length,
            row_stride: stride,
            pixel_stride,
        }],
        color: ColorInfo {
            matrix: ColorMatrix::Bt709,
            range: ColorRange::Limited,
            ..Default::default()
        },
        orientation: Orientation::default(),
        bottom_up: false,
    }
}
#[test]
fn limited_range_black_white_and_color_vectors() {
    let layout = packed(PixelFormat::Yuyv, 4, 2, 8);
    let mut converter = RgbConverter::new();
    let mut rgb = [0; 12];
    converter
        .convert_layout_into(
            &layout,
            &[16, 128, 235, 128, 81, 90, 145, 240],
            ConversionRequest::for_layout(&layout),
            &mut rgb,
        )
        .unwrap();
    assert_eq!(&rgb[..6], &[0, 0, 0, 255, 255, 255]);
    assert_eq!(&rgb[6..9], &[255, 24, 0]);
    assert!(rgb[9] > rgb[10] && rgb[10] > rgb[11]);
}
#[test]
fn padded_and_bottom_up_rgb_trimmed_final_row() {
    let mut layout = packed(PixelFormat::Bgr8, 8, 3, 14);
    layout.bottom_up = true;
    let mut out = [0; 12];
    RgbConverter::new()
        .convert_layout_into(
            &layout,
            &[3, 2, 1, 6, 5, 4, 0, 0, 9, 8, 7, 12, 11, 10],
            ConversionRequest::for_layout(&layout),
            &mut out,
        )
        .unwrap();
    assert_eq!(out, [7, 8, 9, 10, 11, 12, 1, 2, 3, 4, 5, 6]);
}
#[test]
fn malformed_stride_and_unknown_color_are_errors_without_panics() {
    let mut layout = packed(PixelFormat::Yuyv, 10, 4, 20);
    let mut out = [0; 12];
    let mut c = RgbConverter::new();
    assert!(c
        .convert_layout_into(
            &layout,
            &[128; 20],
            ConversionRequest::for_layout(&layout),
            &mut out,
        )
        .is_err());
    layout = packed(PixelFormat::Yuyv, 4, 2, 8);
    layout.color = ColorInfo::default();
    assert!(c
        .convert_layout_into(
            &layout,
            &[128; 8],
            ConversionRequest::for_layout(&layout),
            &mut out,
        )
        .is_err());
    assert!(c
        .convert_layout_into(
            &layout,
            &[128; 7],
            ConversionRequest::for_layout(&layout).with_color(ColorInfo {
                matrix: ColorMatrix::Bt601,
                range: ColorRange::Full,
                ..Default::default()
            }),
            &mut out
        )
        .is_err());
}

#[test]
fn conversion_request_downscales_bgra_without_a_full_size_destination() {
    let layout = FrameLayout {
        width: 4,
        height: 2,
        format: PixelFormat::Bgra8,
        planes: vec![PlaneLayout {
            offset: 0,
            length: 36,
            row_stride: 20,
            pixel_stride: 4,
        }],
        color: ColorInfo::default(),
        orientation: Orientation::default(),
        bottom_up: false,
    };
    let data = [
        3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255, 0, 0, 0, 0, 23, 22, 21, 255, 26,
        25, 24, 255, 29, 28, 27, 255, 32, 31, 30, 255,
    ];
    let request = ConversionRequest::new(2, 1).unwrap();
    let mut out = [0; 6];
    RgbConverter::new()
        .convert_layout_into(&layout, &data, request, &mut out)
        .unwrap();
    assert_eq!(out, [1, 2, 3, 7, 8, 9]);
}

#[test]
fn conversion_request_validates_dimensions_and_exact_destination_size() {
    assert!(ConversionRequest::new(0, 1).is_err());
    assert!(ConversionRequest::new(1, 0).is_err());
    let layout = packed(PixelFormat::Rgb8, 6, 3, 12);
    let request = ConversionRequest::new(1, 1).unwrap();
    let mut oversized = [0; 4];
    assert!(RgbConverter::new()
        .convert_layout_into(&layout, &[1; 12], request, &mut oversized)
        .is_err());
}

#[cfg(feature = "turbojpeg")]
#[test]
fn mjpeg_conversion_scales_at_decode_time_and_resets_between_requests() {
    let width = 16;
    let height = 16;
    let pixels: Vec<u8> = (0..height)
        .flat_map(|y| (0..width).flat_map(move |x| [x as u8 * 12, y as u8 * 12, (x + y) as u8 * 6]))
        .collect();
    let jpeg = turbojpeg::compress(
        turbojpeg::Image {
            pixels: pixels.as_slice(),
            width,
            height,
            pitch: width * 3,
            format: turbojpeg::PixelFormat::RGB,
        },
        100,
        turbojpeg::Subsamp::None,
    )
    .unwrap();
    let layout = FrameLayout {
        width: width as u32,
        height: height as u32,
        format: PixelFormat::Mjpeg,
        planes: vec![PlaneLayout {
            offset: 0,
            length: jpeg.len(),
            row_stride: 0,
            pixel_stride: 1,
        }],
        color: ColorInfo::default(),
        orientation: Orientation::default(),
        bottom_up: false,
    };
    let mut converter = RgbConverter::new();
    let mut half = vec![0; 8 * 8 * 3];
    converter
        .convert_layout_into(
            &layout,
            &jpeg,
            ConversionRequest::new(8, 8).unwrap(),
            &mut half,
        )
        .unwrap();
    assert!(half.iter().any(|&channel| channel > 100));

    let mut arbitrary = vec![0; 3 * 3 * 3];
    converter
        .convert_layout_into(
            &layout,
            &jpeg,
            ConversionRequest::new(3, 3).unwrap(),
            &mut arbitrary,
        )
        .unwrap();
    assert!(arbitrary.iter().any(|&channel| channel > 100));

    let mut full = vec![0; width * height * 3];
    converter
        .convert_layout_into(
            &layout,
            &jpeg,
            ConversionRequest::for_layout(&layout),
            &mut full,
        )
        .unwrap();
    assert!(full.iter().any(|&channel| channel > 150));
}
#[test]
fn rational_order_compares_values_not_storage() {
    let a = camera::FrameRate::new(30000, 1001).unwrap();
    let b = camera::FrameRate::new(30, 1).unwrap();
    assert!(a < b);
    assert!(!camera::FrameRate::new(u32::MAX, 1)
        .unwrap()
        .interval()
        .is_zero());
}
