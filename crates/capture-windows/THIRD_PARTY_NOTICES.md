# Dependency Attribution

No third-party implementation source is copied into this crate. Registry checksums
are pinned by Cargo.lock. Retain applicable upstream licenses on redistribution.

| Dependency | Version | Selected License | Upstream |
| --- | --- | --- | --- |
| crossbeam-utils (via core) | 0.8.23 | MIT | https://github.com/crossbeam-rs/crossbeam |
| windows / windows-core | 0.62.2 | MIT | https://github.com/microsoft/windows-rs |
| windows-collections / windows-future | 0.3.2 | MIT | https://github.com/microsoft/windows-rs |
| windows-numerics | 0.3.1 | MIT | https://github.com/microsoft/windows-rs |
| windows-implement / windows-interface | 0.60.2 / 0.59.3 | MIT | https://github.com/microsoft/windows-rs |
| windows-result / windows-strings | 0.4.1 / 0.5.1 | MIT | https://github.com/microsoft/windows-rs |
| windows-threading / windows-link | 0.2.1 / 0.2.1 | MIT | https://github.com/microsoft/windows-rs |
| proc-macro2 / quote / syn | 1.0.107 / 1.0.47 / 2.0.119 | MIT | https://github.com/dtolnay |
| unicode-ident | 1.0.26 | MIT AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |

Optional and other-target lockfile packages are not necessarily linked into a Windows build.