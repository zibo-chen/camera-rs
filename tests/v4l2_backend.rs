use camera::BackendType;

#[test]
fn v4l2_is_available_only_on_supported_platforms() {
    let expected = cfg!(any(
        all(target_os = "linux", feature = "native"),
        all(
            any(target_os = "linux", target_os = "android"),
            feature = "backend-v4l2"
        )
    ));
    assert_eq!(BackendType::V4l2.is_available(), expected);
    assert_eq!(BackendType::V4l2.display_name(), "Linux V4L2");
    if expected && cfg!(target_os = "linux") {
        assert_eq!(BackendType::default_for_platform(), BackendType::V4l2);
    }
}
