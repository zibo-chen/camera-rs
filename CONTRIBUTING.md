# Contributing

Open an issue before making a large API or backend change. Small correctness,
documentation and platform-compatibility fixes may go directly to a pull
request.

Run the following before submitting a change:

```sh
cargo fmt --all -- --check
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --no-deps
cargo audit --deny warnings
cargo package --locked
```

Native changes must include a hardware-free regression test where practical.
Hardware claims must identify the backend, operating system, architecture,
device family, negotiated mode and whether the result is build-only or was
measured on a physical camera.

Write source comments and API documentation comments in English. User-facing
application text and localized documentation may use their target language.

By contributing, you agree that your contribution is licensed under MIT OR
Apache-2.0, matching the project.
