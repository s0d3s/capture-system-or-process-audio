# Dependency Notices

`capture-system-or-process-audio` is licensed under `MIT OR Apache-2.0` and contains `#![forbid(unsafe_code)]` Rust code only. Platform backend crates retain their respective dependency notices:

- Portable core: [crates/capture-core/THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture-core/THIRD_PARTY_NOTICES.md)
- Windows WASAPI backend: [crates/capture-windows/THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture-windows/THIRD_PARTY_NOTICES.md)
- Linux PipeWire & PulseAudio backend: [crates/capture-linux/THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture-linux/THIRD_PARTY_NOTICES.md)
- macOS Core Audio & ScreenCaptureKit backend: [crates/capture-macos/THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture-macos/THIRD_PARTY_NOTICES.md)

| Development Dependency | Version | License | Upstream |
| --- | --- | --- | --- |
| `allocation-counter` | `0.8.1` | `MIT OR Apache-2.0` | https://github.com/fornwall/allocation-counter |
