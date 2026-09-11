use camera::{
    ColorInfo, ColorMatrix, ColorRange, ConversionRequest, FrameLayout, Orientation, PixelFormat,
    PlaneLayout, RgbConverter,
};
use std::{hint::black_box, time::Instant};

fn color(matrix: ColorMatrix, range: ColorRange) -> ColorInfo {
    ColorInfo {
        matrix,
        range,
        ..Default::default()
    }
}

fn packed(
    width: usize,
    height: usize,
    format: PixelFormat,
    pixel_stride: usize,
    color: ColorInfo,
) -> (FrameLayout, Vec<u8>) {
    let row_stride = width * pixel_stride;
    let data = vec![128; row_stride * height];
    (
        FrameLayout {
            width: width as u32,
            height: height as u32,
            format,
            planes: vec![PlaneLayout {
                offset: 0,
                length: data.len(),
                row_stride,
                pixel_stride,
            }],
            color,
            orientation: Orientation::default(),
            bottom_up: false,
        },
        data,
    )
}

fn nv12(
    width: usize,
    height: usize,
    format: PixelFormat,
    color: ColorInfo,
) -> (FrameLayout, Vec<u8>) {
    let y_len = width * height;
    let uv_len = width * height.div_ceil(2);
    (
        FrameLayout {
            width: width as u32,
            height: height as u32,
            format,
            planes: vec![
                PlaneLayout {
                    offset: 0,
                    length: y_len,
                    row_stride: width,
                    pixel_stride: 1,
                },
                PlaneLayout {
                    offset: y_len,
                    length: uv_len,
                    row_stride: width,
                    pixel_stride: 2,
                },
            ],
            color,
            orientation: Orientation::default(),
            bottom_up: false,
        },
        vec![128; y_len + uv_len],
    )
}

fn i420(width: usize, height: usize, color: ColorInfo) -> (FrameLayout, Vec<u8>) {
    let y_len = width * height;
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let chroma_len = chroma_width * chroma_height;
    (
        FrameLayout {
            width: width as u32,
            height: height as u32,
            format: PixelFormat::Yuv420p,
            planes: vec![
                PlaneLayout {
                    offset: 0,
                    length: y_len,
                    row_stride: width,
                    pixel_stride: 1,
                },
                PlaneLayout {
                    offset: y_len,
                    length: chroma_len,
                    row_stride: chroma_width,
                    pixel_stride: 1,
                },
                PlaneLayout {
                    offset: y_len + chroma_len,
                    length: chroma_len,
                    row_stride: chroma_width,
                    pixel_stride: 1,
                },
            ],
            color,
            orientation: Orientation::default(),
            bottom_up: false,
        },
        vec![128; y_len + chroma_len * 2],
    )
}

fn run(label: &str, layout: &FrameLayout, data: &[u8], request: ConversionRequest) {
    let mut converter = RgbConverter::new();
    let mut output = vec![0; request.output_len().unwrap()];
    for _ in 0..5 {
        converter
            .convert_layout_into(layout, data, request, &mut output)
            .unwrap();
    }
    let mut samples = Vec::with_capacity(120);
    for _ in 0..120 {
        let begin = Instant::now();
        converter
            .convert_layout_into(
                black_box(layout),
                black_box(data),
                request,
                black_box(&mut output),
            )
            .unwrap();
        samples.push(begin.elapsed().as_nanos() as u64);
    }
    samples.sort_unstable();
    let percentile = |percent: usize| samples[(samples.len() * percent).div_ceil(100) - 1];
    let mean = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
    println!(
        "{label:<28} mean={:.3} p50={:.3} p95={:.3} p99={:.3} ms",
        mean / 1_000_000.0,
        percentile(50) as f64 / 1_000_000.0,
        percentile(95) as f64 / 1_000_000.0,
        percentile(99) as f64 / 1_000_000.0,
    );
}

fn main() {
    let (width, height) = (1920, 1080);
    let full_601 = color(ColorMatrix::Bt601, ColorRange::Full);
    let limited_709 = color(ColorMatrix::Bt709, ColorRange::Limited);
    let cases = [
        (
            "YUYV BT.601 full",
            packed(width, height, PixelFormat::Yuyv, 2, full_601),
        ),
        (
            "YUYV BT.709 limited",
            packed(width, height, PixelFormat::Yuyv, 2, limited_709),
        ),
        (
            "NV12 BT.601 full",
            nv12(width, height, PixelFormat::Nv12, full_601),
        ),
        (
            "NV12 BT.709 limited",
            nv12(width, height, PixelFormat::Nv12, limited_709),
        ),
        (
            "NV21 BT.601 full",
            nv12(width, height, PixelFormat::Nv21, full_601),
        ),
        ("I420 BT.601 full", i420(width, height, full_601)),
        (
            "BGRA 8888",
            packed(width, height, PixelFormat::Bgra8, 4, ColorInfo::default()),
        ),
    ];
    for (label, (layout, data)) in &cases {
        run(label, layout, data, ConversionRequest::for_layout(layout));
    }
    let (layout, data) = nv12(width, height, PixelFormat::Nv12, full_601);
    run(
        "NV12 1080p -> 540p",
        &layout,
        &data,
        ConversionRequest::new(960, 540).unwrap(),
    );
}
