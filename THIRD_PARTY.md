# Third-party code

The camera-rs Rust library and its original bridge code are licensed under MIT OR Apache-2.0. Vendored dependencies retain their own licenses:

- `3rdparty/libuvc`: based on upstream v0.0.7; BSD-3-Clause; see
  `LICENSE.txt` in that directory.
- `3rdparty/libusb`: based on an upstream post-v1.0.29 snapshot whose public
  version remains 1.0.29; LGPL-2.1-or-later; see `COPYING`. Enabled only by
  the opt-in `backend-uvc` feature, which builds a static native library.
  Distributing a linked application also requires complying with that license,
  including applicable relinking/source obligations.
- Optional JPEG decoding uses the `turbojpeg` Rust crate and libjpeg-turbo. Their licenses and notices remain applicable to distributed binaries.

The former unused vendored libyuv and libjpeg-turbo source trees have been removed. Native platform APIs are supplied by the operating system. Rust dependency licenses are recorded in their respective packages.

The bundled trees include platform adaptations and should not be treated as
pristine upstream release archives. Exact upstream revisions and the local
change categories are recorded in [VENDORING.md](VENDORING.md). Their exact
source ships in the crate; downstream patch provenance is the camera-rs Git
history.
