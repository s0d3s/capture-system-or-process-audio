# Dependency Notices

`cspa-tui-demo` is a standalone interactive studio demonstration tool (`publish = false`) licensed under `MIT OR Apache-2.0` and containing `#![forbid(unsafe_code)]` Rust code only. It depends on the unified `capture-system-or-process-audio` Rust facade ([crates/capture/THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture/THIRD_PARTY_NOTICES.md)) and the following third-party crates:

| Dependency | Version | License | Upstream |
| --- | --- | --- | --- |
| `ratatui` | `0.30.2` | `MIT` | https://github.com/ratatui/ratatui |
| `crossterm` | `0.29.0` | `MIT` | https://github.com/crossterm-rs/crossterm |
| `rustfft` | `6.4.1` | `MIT OR Apache-2.0` | https://github.com/ejmahler/RustFFT |
| `cpal` | `0.18.2` | `Apache-2.0` | https://github.com/RustAudio/cpal |
