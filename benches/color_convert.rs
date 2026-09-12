use camera::{
    selected_conversion_path, ColorInfo, ColorMatrix, ColorRange, ConversionRequest, FrameLayout,
    Orientation, PixelFormat, PlaneLayout, RgbConverter,
};
#[cfg(feature = "benchmark-internals")]
use camera::{FrameHub, SubscriptionOptions};
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
        data.to_vec(),
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

fn nv12_padded(width: usize, height: usize, padding: usize) -> (FrameLayout, Vec<u8>) {
    let y_stride = width + padding;
    let uv_stride = width + padding;
    let y_len = y_stride * height;
    let uv_len = uv_stride * height.div_ceil(2);
    (
        FrameLayout {
            width: width as u32,
            height: height as u32,
            format: PixelFormat::Nv12,
            planes: vec![
                PlaneLayout {
                    offset: 0,
                    length: y_len,
                    row_stride: y_stride,
                    pixel_stride: 1,
                },
                PlaneLayout {
                    offset: y_len,
                    length: uv_len,
                    row_stride: uv_stride,
                    pixel_stride: 2,
                },
            ],
            color: color(ColorMatrix::Bt709, ColorRange::Limited),
            orientation: Orientation::default(),
            bottom_up: false,
        },
        vec![128; y_len + uv_len],
    )
}

#[cfg(feature = "decode-mjpeg")]
fn mjpeg(width: usize, height: usize) -> (FrameLayout, Vec<u8>) {
    let pixels = vec![128; width * height * 3];
    let data = turbojpeg::compress(
        turbojpeg::Image {
            pixels: &pixels,
            width,
            height,
            pitch: width * 3,
            format: turbojpeg::PixelFormat::RGB,
        },
        90,
        turbojpeg::Subsamp::None,
    )
    .unwrap();
    (
        FrameLayout {
            width: width as u32,
            height: height as u32,
            format: PixelFormat::Mjpeg,
            planes: vec![PlaneLayout {
                offset: 0,
                length: data.len(),
                row_stride: 0,
                pixel_stride: 1,
            }],
            color: ColorInfo::default(),
            orientation: Orientation::default(),
            bottom_up: false,
        },
        data.to_vec(),
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

fn run(label: &str, layout: &FrameLayout, data: &[u8], request: ConversionRequest) -> f64 {
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
    let kernel = selected_conversion_path(layout, request);
    let strides: Vec<_> = layout.planes.iter().map(|plane| plane.row_stride).collect();
    let p50_ms = percentile(50) as f64 / 1_000_000.0;
    let megapixels_per_second = request.width as f64 * request.height as f64 / p50_ms / 1_000.0;
    let checksum = output
        .iter()
        .fold(0u64, |sum, &value| sum.wrapping_add(u64::from(value)));
    black_box(checksum);
    println!(
        "case={label} kernel={kernel} strides={strides:?} output={}x{}",
        request.width, request.height
    );
    println!(
        "{label:<28} mean={:.3} p50={:.3} p95={:.3} p99={:.3} ms throughput={megapixels_per_second:.1} MP/s checksum={checksum}",
        mean / 1_000_000.0,
        percentile(50) as f64 / 1_000_000.0,
        percentile(95) as f64 / 1_000_000.0,
        percentile(99) as f64 / 1_000_000.0,
    );
    p50_ms
}

#[cfg(feature = "decode-mjpeg")]
fn benchmark_rejected_mjpeg(label: &str, layout: &FrameLayout, data: &[u8]) {
    let mut converter = RgbConverter::new();
    let request = ConversionRequest::for_layout(layout);
    let mut output = vec![0; request.output_len().unwrap()];
    let mut samples = Vec::with_capacity(240);
    for _ in 0..240 {
        let begin = Instant::now();
        assert!(converter
            .convert_layout_into(
                black_box(layout),
                black_box(data),
                request,
                black_box(&mut output),
            )
            .is_err());
        samples.push(begin.elapsed().as_nanos() as u64);
    }
    samples.sort_unstable();
    println!(
        "{label:<28} p50={:.3} p95={:.3} us (rejected, no output published)",
        samples[samples.len() / 2] as f64 / 1_000.0,
        samples[samples.len() * 95 / 100] as f64 / 1_000.0,
    );
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
    let (layout, data) = nv12_padded(width, height, 64);
    run(
        &format!("NV12 padded {width}x{height}"),
        &layout,
        &data,
        ConversionRequest::for_layout(&layout),
    );
    let (layout, data) = nv12(width, height, PixelFormat::Nv12, full_601);
    run(
        &format!("NV12 half {width}x{height}"),
        &layout,
        &data,
        ConversionRequest::new((width / 2) as u32, (height / 2) as u32).unwrap(),
    );
    for (label, (layout, data)) in [
        (
            "NV12 arbitrary 2:3",
            nv12(width, height, PixelFormat::Nv12, full_601),
        ),
        (
            "YUYV arbitrary 2:3",
            packed(width, height, PixelFormat::Yuyv, 2, full_601),
        ),
        ("I420 arbitrary 2:3", i420(width, height, full_601)),
    ] {
        run(
            &format!("{label} {width}x{height}"),
            &layout,
            &data,
            ConversionRequest::new((width * 2 / 3) as u32, (height * 2 / 3) as u32).unwrap(),
        );
    }
    let (layout, data) = packed(width, height, PixelFormat::Rgb8, 3, ColorInfo::default());
    run(
        &format!("RGB8 arbitrary 2:3 {width}x{height}"),
        &layout,
        &data,
        ConversionRequest::new((width * 2 / 3) as u32, (height * 2 / 3) as u32).unwrap(),
    );
    run(
        &format!("RGB8 arbitrary 3:4 {width}x{height}"),
        &layout,
        &data,
        ConversionRequest::new((width * 3 / 4) as u32, (height * 3 / 4) as u32).unwrap(),
    );
    for (label, (layout, data)) in [
        (
            "NV12 arbitrary 3:4",
            nv12(width, height, PixelFormat::Nv12, full_601),
        ),
        (
            "YUYV arbitrary 3:4",
            packed(width, height, PixelFormat::Yuyv, 2, full_601),
        ),
        ("I420 arbitrary 3:4", i420(width, height, full_601)),
    ] {
        run(
            &format!("{label} {width}x{height}"),
            &layout,
            &data,
            ConversionRequest::new((width * 3 / 4) as u32, (height * 3 / 4) as u32).unwrap(),
        );
    }
    for (label, (layout, data)) in [
        (
            "YUYV half",
            packed(width, height, PixelFormat::Yuyv, 2, full_601),
        ),
        ("I420 half", i420(width, height, full_601)),
    ] {
        run(
            &format!("{label} {width}x{height}"),
            &layout,
            &data,
            ConversionRequest::new((width / 2) as u32, (height / 2) as u32).unwrap(),
        );
    }
}

#[cfg(feature = "benchmark-internals")]
fn benchmark_publish(width: usize, height: usize) {
    let hub = FrameHub::new(6);
    let session = hub.start();
    let _receiver = hub.subscribe_with(SubscriptionOptions::latest()).unwrap();
    for _ in 0..8 {
        hub.publish_rgb(session, width as u32, height as u32, None, |rgb| {
            black_box(rgb);
            Ok(())
        })
        .unwrap();
    }
    let mut samples = Vec::with_capacity(120);
    for _ in 0..120 {
        let begin = Instant::now();
        hub.publish_rgb(session, width as u32, height as u32, None, |rgb| {
            black_box(rgb);
            Ok(())
        })
        .unwrap();
        samples.push(begin.elapsed().as_nanos() as u64);
    }
    samples.sort_unstable();
    println!(
        "FrameHub publish {width}x{height} p50={:.3} us latest_sequence={}",
        samples[samples.len() / 2] as f64 / 1_000.0,
        hub.latest().unwrap().key.sequence
    );
}

fn assert_scaling_regression() {
    let width = 1920;
    let height = 1080;
    let full = color(ColorMatrix::Bt601, ColorRange::Full);
    let (layout, data) = nv12(width, height, PixelFormat::Nv12, full);
    let direct = run(
        "NV12 regression direct",
        &layout,
        &data,
        ConversionRequest::for_layout(&layout),
    );
    let two_thirds = run(
        "NV12 regression exact 2:3",
        &layout,
        &data,
        ConversionRequest::new(1280, 720).unwrap(),
    );
    assert!(
        two_thirds <= direct * 1.25,
        "exact 2:3 NV12 scaling regressed: {two_thirds:.3}ms vs direct {direct:.3}ms"
    );
    let three_quarters = run(
        "NV12 regression exact 3:4",
        &layout,
        &data,
        ConversionRequest::new(1440, 810).unwrap(),
    );
    assert!(
        three_quarters <= direct * 1.25,
        "exact 3:4 NV12 scaling regressed: {three_quarters:.3}ms vs direct {direct:.3}ms"
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
    let resolutions: &[(usize, usize)] = if std::env::var_os("CAMERA_BENCH_QUICK").is_some() {
        &[(1920, 1080)]
    } else {
        &[(1280, 720), (1920, 1080), (3840, 2160)]
    };
    for &(width, height) in resolutions {
        benchmark_resolution(width, height);
    }
    #[cfg(feature = "benchmark-internals")]
    benchmark_publish(1920, 1080);
    #[cfg(feature = "decode-mjpeg")]
    {
        let (layout, data) = mjpeg(1920, 1080);
        run(
            "MJPEG 1080p full",
            &layout,
            &data,
            ConversionRequest::for_layout(&layout),
        );
        run(
            "MJPEG 1080p to 720p",
            &layout,
            &data,
            ConversionRequest::new(1280, 720).unwrap(),
        );
        run(
            "MJPEG 1080p exact 3/4",
            &layout,
            &data,
            ConversionRequest::new(1440, 810).unwrap(),
        );
        run(
            "MJPEG 1080p half",
            &layout,
            &data,
            ConversionRequest::new(960, 540).unwrap(),
        );
        run(
            "MJPEG 1080p to 360p",
            &layout,
            &data,
            ConversionRequest::new(640, 360).unwrap(),
        );
        let mut rejected_layout = layout.clone();
        let rejected = vec![0; data.len() / 4];
        rejected_layout.planes[0].length = rejected.len();
        benchmark_rejected_mjpeg("MJPEG missing SOI", &rejected_layout, &rejected);
        let truncated = &data[..data.len() / 4];
        rejected_layout.planes[0].length = truncated.len();
        benchmark_rejected_mjpeg("MJPEG truncated", &rejected_layout, truncated);
    }
    if std::env::var_os("CAMERA_BENCH_ASSERT").is_some() {
        assert_scaling_regression();
    }
}
