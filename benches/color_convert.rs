use camera::{
    ColorInfo, ColorMatrix, ColorRange, FrameLayout, Orientation, PixelFormat, PlaneLayout,
    RgbConverter,
};
use std::{hint::black_box, time::Instant};
fn main() {
    for (w, h) in [(1280, 720), (1920, 1080), (3840, 2160)] {
        let data = vec![128; w * h * 2];
        let mut out = vec![0; w * h * 3];
        let mut converter = RgbConverter::new();
        let layout = FrameLayout {
            width: w as u32,
            height: h as u32,
            format: PixelFormat::Yuyv,
            planes: vec![PlaneLayout {
                offset: 0,
                length: data.len(),
                row_stride: w * 2,
                pixel_stride: 2,
            }],
            color: ColorInfo {
                matrix: ColorMatrix::Bt601,
                range: ColorRange::Full,
                ..Default::default()
            },
            orientation: Orientation::default(),
            bottom_up: false,
        };
        for _ in 0..3 {
            converter
                .convert_layout_into(&layout, &data, None, &mut out)
                .unwrap();
        }
        let begin = Instant::now();
        for _ in 0..60 {
            converter
                .convert_layout_into(&layout, black_box(&data), None, black_box(&mut out))
                .unwrap();
        }
        println!(
            "YUYV {w}x{h}: {:.3} ms/frame",
            begin.elapsed().as_secs_f64() * 1000.0 / 60.0
        );
    }
}
