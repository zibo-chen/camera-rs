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
    let kernel = selected_kernel(layout, request);
    let strides: Vec<_> = layout.planes.iter().map(|plane| plane.row_stride).collect();
    println!(
        "case={label} kernel={kernel} strides={strides:?} output={}x{}",
        request.width, request.height
    );
    println!(
        "{label:<28} mean={:.3} p50={:.3} p95={:.3} p99={:.3} ms",
        mean / 1_000_000.0,
        percentile(50) as f64 / 1_000_000.0,
        percentile(95) as f64 / 1_000_000.0,
        percentile(99) as f64 / 1_000_000.0,
    );
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
fn selected_kernel(layout: &FrameLayout, request: ConversionRequest) -> &'static str {
    match layout.format {
        PixelFormat::Yuv420p => "neon-planar-contiguous-two-row",
        PixelFormat::Nv12 | PixelFormat::Nv21
            if request.width < layout.width || request.height < layout.height =>
        {
            "neon-semiplanar-direct-half"
        }
        PixelFormat::Nv12 | PixelFormat::Nv21 => "neon-semiplanar-two-row",
        PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Argb8 => "neon-packed8888",
        PixelFormat::Yuyv | PixelFormat::Uyvy => "neon-packed422",
        _ => "scalar",
    }
}

#[cfg(target_arch = "x86_64")]
fn selected_kernel(layout: &FrameLayout, request: ConversionRequest) -> &'static str {
    if std::arch::is_x86_feature_detected!("avx2") {
        return match layout.format {
            PixelFormat::Yuv420p => "avx2-planar",
            PixelFormat::Nv12 | PixelFormat::Nv21
                if request.width < layout.width || request.height < layout.height =>
            {
                "avx2-semiplanar-direct-half"
            }
            PixelFormat::Nv12 | PixelFormat::Nv21 => "avx2-semiplanar",
            PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Argb8 => "avx2-packed8888",
            PixelFormat::Yuyv | PixelFormat::Uyvy => "avx2-packed422",
            _ => "scalar",
        };
    }
    if std::arch::is_x86_feature_detected!("ssse3")
        && matches!(
            layout.format,
            PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Argb8
        )
    {
        "ssse3-packed8888"
    } else {
        "scalar"
    }
}

#[cfg(not(any(
    all(target_arch = "aarch64", target_feature = "neon"),
    target_arch = "x86_64"
)))]
fn selected_kernel(_layout: &FrameLayout, _request: ConversionRequest) -> &'static str {
    "scalar"
}

fn benchmark_resolution(width: usize, height: usize) {
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
        run(
            &format!("{label} {width}x{height}"),
            layout,
            data,
            ConversionRequest::for_layout(layout),
        );
    }
    let (layout, data) = nv12(width, height, PixelFormat::Nv12, full_601);
    run(
        &format!("NV12 half {width}x{height}"),
        &layout,
        &data,
        ConversionRequest::new((width / 2) as u32, (height / 2) as u32).unwrap(),
    );
}

fn main() {
    let revision = std::env::var("CAMERA_BENCH_REVISION").unwrap_or_else(|_| "unknown".into());
    println!(
        "environment arch={} os={} profile={} revision={revision}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    for (width, height) in [(1280, 720), (1920, 1080), (3840, 2160)] {
        benchmark_resolution(width, height);
    }
}
