#[test]
fn package_identity_is_safe_for_publication() {
    assert_eq!(env!("CARGO_PKG_NAME"), "camera-rs");
    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.lines().any(|line| line == "[lib]"));
    assert!(manifest.lines().any(|line| line == "name = \"camera\""));
    assert!(manifest.contains("authors = [\"ChenZibo <qw.54@163.com>\"]"));
    assert!(manifest.contains("license = \"MIT OR Apache-2.0\""));
    assert!(!manifest.contains("ssh://"));
    assert!(!manifest.contains("file://"));
    assert!(manifest.contains("documentation = \"https://docs.rs/camera-rs\""));
    assert!(manifest.contains("repository = \"https://github.com/zibo-chen/camera-rs\""));
}

#[test]
fn copyright_and_dual_license_are_explicit() {
    for notice in [
        include_str!("../LICENSE-MIT"),
        include_str!("../camera-android/LICENSE-MIT"),
    ] {
        assert!(notice.contains("Copyright (c) 2026 ChenZibo"));
    }

    let adapter_manifest = include_str!("../camera-android/Cargo.toml");
    assert!(adapter_manifest.contains("authors = [\"ChenZibo <qw.54@163.com>\"]"));
    assert!(adapter_manifest.contains("license = \"MIT OR Apache-2.0\""));
    assert!(adapter_manifest.contains("repository = \"https://github.com/zibo-chen/camera-rs\""));
}

#[test]
fn default_features_keep_native_capture_lightweight() {
    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.contains("default = [\"native\", \"runtime-tokio\"]"));
    assert!(!manifest.contains("viewer = ["));
}

#[test]
fn published_docs_do_not_contain_private_workspace_details() {
    for document in [
        include_str!("../README.md"),
        include_str!("../BUILD.md"),
        include_str!("../docs/API.md"),
        include_str!("../docs/ARCHITECTURE.md"),
        include_str!("../docs/V4L2.md"),
    ] {
        assert!(!document.contains("/Users/"));
        assert!(!document.contains("ssh://"));
    }
}
