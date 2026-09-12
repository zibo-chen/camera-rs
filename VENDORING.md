# Vendored native sources

The `backend-uvc` feature is opt-in and compiles the source below into the
consumer's binary. Do not replace either tree without updating this file,
`THIRD_PARTY.md`, the native lifecycle tests and the source archive allowlist.

## libusb

- Upstream: <https://github.com/libusb/libusb>
- Base revision: `c9f02b27b2696bf7fe267bfa0cac0a43c81c58b3`
- Public source version: 1.0.29
- License: LGPL-2.1-or-later
- Local scope: Android build integration, logging control and platform source
  selection used by `build.rs`.

The base revision is after the v1.0.29 tag but before the public version was
bumped. The generated `config.h` deliberately reports the same 1.0.29 version
as the vendored `libusb/version.h`.

## libuvc

- Upstream: <https://github.com/libuvc/libuvc>
- Base tag: v0.0.7
- Base revision: `68d07a00e11d1944e27b7295ee69673239c00b4b`
- License: BSD-3-Clause
- Local scope: bounded frame/metadata allocation, checked stream negotiation,
  synchronous transfer shutdown and opt-in native diagnostics.

The camera-rs copies of `device.c` and `stream.c` intentionally differ from
upstream. Regression coverage lives in `tests/uvc_lifecycle_test.c` and
`tests/uvc_negotiation_test.c`, both run under ASan and UBSan.

## Distribution obligations

Applications distributing a statically linked UVC build must retain the
notices and satisfy the LGPL relinking/source requirements for libusb. The
complete corresponding native source is included in the `.crate` archive.
Application distributors remain responsible for providing any required
linkable objects or another practical relinking mechanism for their binaries.
