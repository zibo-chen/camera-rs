use crate::utils::color_convert::{yuv420_to_rgb_into, yuv420sp_to_rgb_into, YuvPlane};

#[test]
fn planar_and_nv21_aliases_match_nv12_with_trimmed_last_row() {
    let y = [10, 30, 50, 0, 70, 90, 110, 0, 130, 150, 170];
    let u = [20, 200, 0, 0, 180, 70];
    let v = [220, 40, 0, 0, 160, 10];
    let nv12 = [20, 220, 200, 40, 180, 160, 70, 10];
    let nv21 = [220, 20, 40, 200, 160, 180, 10, 70];
    let mut expected = [0; 27];
    let mut actual = [0; 27];
    yuv420sp_to_rgb_into(&y, &nv12, 3, 3, 4, 4, &mut expected).unwrap();
    let luma = YuvPlane {
        data: &y,
        row_stride: 4,
        pixel_stride: 1,
    };
    yuv420_to_rgb_into(
        luma,
        YuvPlane {
            data: &u,
            row_stride: 4,
            pixel_stride: 1,
        },
        YuvPlane {
            data: &v,
            row_stride: 4,
            pixel_stride: 1,
        },
        3,
        3,
        &mut actual,
    )
    .unwrap();
    assert_eq!(expected, actual);
    yuv420_to_rgb_into(
        luma,
        YuvPlane {
            data: &nv21[1..],
            row_stride: 4,
            pixel_stride: 2,
        },
        YuvPlane {
            data: &nv21[..7],
            row_stride: 4,
            pixel_stride: 2,
        },
        3,
        3,
        &mut actual,
    )
    .unwrap();
    assert_eq!(expected, actual);
}

#[test]
fn invalid_planes_fail_before_writing_destination() {
    let p = YuvPlane {
        data: &[128; 4],
        row_stride: 2,
        pixel_stride: 1,
    };
    let mut rgb = [91; 12];
    for bad in [
        YuvPlane { data: &[], ..p },
        YuvPlane {
            pixel_stride: 0,
            ..p
        },
        YuvPlane {
            row_stride: usize::MAX,
            ..p
        },
    ] {
        assert!(yuv420_to_rgb_into(bad, p, p, 2, 2, &mut rgb).is_err());
        assert_eq!(rgb, [91; 12]);
    }
}
