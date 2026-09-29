# capture-system-or-process-audio-macos

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

> **Looking to capture system, application, or microphone audio in Rust?**
> Depend on the unified safe-Rust facade crate **[`capture-system-or-process-audio`](https://crates.io/crates/capture-system-or-process-audio)** instead of importing this internal macOS backend crate directly.

`capture-system-or-process-audio-macos` implements the Apple Silicon (`aarch64-apple-darwin`) backend (`PlatformStream`, `PlatformDiscovery`, `PlatformManager`, `PlatformPermissions`, `PlatformDefaultOutputObserver`, `PlatformDefaultInputObserver`, and `PlatformTopologyController`) for `capture-system-or-process-audio`:

- **Core Audio Process Taps (`"coreaudio"`, macOS `14.2+`):** Supports `SharedNativeGraphMix` (`PlatformTopologyController` + `[parent, child]` explicit `AudioObjectID` set union), `device(id)`, and `default_output()` (`Follow` vs `Pin`).
- **Core Audio HAL Input (`"coreaudio"`, macOS `13.0+`):** Supports `microphone()` (`Follow` vs `Pin`) and `input_device(id)` with `AVMediaTypeAudio` TCC inspection/prompts, bundled `Info.plist` `SIGABRT` protection, and strict input/output direction isolation.
- **ScreenCaptureKit (`"screencapturekit"`, macOS `13.0+`):** Captures discovered `application` targets (`ProcessScope::Exact`, continuing across hidden or closed windows while excluding windowless child helpers) with fused planar-to-interleaved `AudioBufferList` packing and `0`-steady-state-allocation cross-backend `[ScreenCaptureKit + CoreAudio]` `CspaMixer`.
- **Process Identity Safety:** Verifies `(pid, process_start_time, ppid)` via `proc_pidinfo(PROC_PIDTBSDINFO)` to reject stale post-exit `AudioObjectID`s and PID reuse.

## Links & License

- **Official User Manual & Platform Docs:** <https://s0d3s.github.io/capture-system-or-process-audio/concepts/platforms/>
- **Main Crate & Documentation:** <https://crates.io/crates/capture-system-or-process-audio> · <https://docs.rs/capture-system-or-process-audio>
- **Repository & Notices:** <https://github.com/s0d3s/capture-system-or-process-audio> · [THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture-macos/THIRD_PARTY_NOTICES.md)
- **License:** Dual-licensed under [MIT](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-APACHE), at your option.
