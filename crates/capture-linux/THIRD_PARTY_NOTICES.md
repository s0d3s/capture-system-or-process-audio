# Dependency Attribution

No third-party implementation source is copied into this crate. Registry checksums
are pinned by Cargo.lock. Retain applicable upstream licenses on redistribution.

| Dependency | Version | Selected License | Upstream |
| --- | --- | --- | --- |
| crossbeam-utils (via core) | 0.8.23 | MIT | https://github.com/crossbeam-rs/crossbeam |
| libloading | 0.8.9 | ISC | https://github.com/nagisa/rust_libloading |
| cfg-if | 1.0.5 | MIT | https://github.com/rust-lang/cfg-if |

At runtime on Linux, this crate dynamically loads system-installed client libraries
(`libpipewire-0.3.so.0` under MIT and `libpulse.so.0` under LGPL-2.1+) without
bundling or statically linking them.
