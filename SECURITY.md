# Security policy

## Supported versions

Security fixes are provided for the latest released minor version. Older
pre-1.0 releases may receive a backport when the affected code is still widely
used.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Use the private
security-advisory feature of the public source host. Include the affected
backend and target, a minimal reproducer, impact, and whether untrusted camera
data or application input is required.

Maintainers will acknowledge a complete report within seven days and will
coordinate disclosure after a fix and supported release are available.

Camera frame bytes, USB descriptors and JNI inputs are treated as untrusted.
Reports involving FFI lifetime, buffer bounds, device permissions or vendored
native dependencies are in scope.
