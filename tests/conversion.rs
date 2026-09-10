use camera::{
    ColorInfo, ColorMatrix, ColorRange, FrameLayout, Orientation, PixelFormat, PlaneLayout,
    RgbConverter,
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
            None,
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
            None,
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
        .convert_layout_into(&layout, &[128; 20], None, &mut out)
        .is_err());
    layout = packed(PixelFormat::Yuyv, 4, 2, 8);
    layout.color = ColorInfo::default();
    assert!(c
        .convert_layout_into(&layout, &[128; 8], None, &mut out)
        .is_err());
    assert!(c
        .convert_layout_into(
            &layout,
            &[128; 7],
            Some(ColorInfo {
                matrix: ColorMatrix::Bt601,
                range: ColorRange::Full,
                ..Default::default()
            }),
            &mut out
        )
        .is_err());
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
