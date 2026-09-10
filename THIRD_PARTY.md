# Third-party code

The camera-rs Rust library and its original bridge code are licensed under MIT OR Apache-2.0. Vendored dependencies retain their own licenses:

- `3rdparty/libuvc`: bundled headers identify version 0.0.6; BSD-3-Clause; see `LICENSE.txt` in that directory.
- `3rdparty/libusb`: `libusb/version.h` identifies version 1.0.29; LGPL-2.1-or-later; see `COPYING`. Enabled by `backend-uvc`, which builds a static native library. Distributing a linked application also requires complying with that license, including applicable relinking/source obligations.
- Optional JPEG decoding uses the `turbojpeg` Rust crate and libjpeg-turbo. Their licenses and notices remain applicable to distributed binaries.

Unused vendored libyuv and libjpeg-turbo source trees are excluded from the published crate. The native platform APIs are supplied by the operating system. Rust dependency licenses are recorded in their respective packages.

The bundled trees include platform adaptations and should not be treated as pristine upstream release archives. Their exact source ships in the crate; downstream patch provenance is the camera-rs Git history.
