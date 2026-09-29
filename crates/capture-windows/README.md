# capture-system-or-process-audio-windows

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

> **Looking to capture system, application, or microphone audio in Rust?**
> Depend on the unified safe-Rust facade crate **[`capture-system-or-process-audio`](https://crates.io/crates/capture-system-or-process-audio)** instead of importing this internal Windows backend crate directly.

`capture-system-or-process-audio-windows` implements the Windows WASAPI backend (`PlatformStream`, `PlatformDiscovery`, `#![deny(unsafe_code)]` `PlatformManager`, `PlatformPermissions`, `PlatformDefaultOutputObserver`, and `PlatformDefaultInputObserver`) for `capture-system-or-process-audio`:

- **Supported Scopes & Windows Build Floors:**
  - `default_output` & `microphone` (`Follow` or `Pin`) and `device(id)` & `input_device(id)` on Windows 10/11 build `19041+` (`eRender` loopback and `eCapture` microphones with strict `IMMEndpoint::GetDataFlow()` direction isolation).
  - `process` and `application` (`ProcessScope::Tree`) via WASAPI kernel process-tree loopback (`ActivateAudioInterfaceAsync`) on Windows 10/11 build `20348+`.
  - Unsupported requests (`ProcessScope::Exact`, older OS builds, or uncalibrated `TimingRequirement::Precise`) fail explicitly with typed `CapabilityError` variants rather than silently broadening to system loopback.
- **Real-Time Acquisition & Session Orchestration:**
  - Dedicated COM-affine MMCSS (`Capture`) worker thread draining bounded 32-packet turns into preallocated 4096-frame `Pool::new_reserved` slots (`0` scratch silence buffers, OS packets released prior to reader wake).
  - `PlatformManager` orchestrates zero-extra-copy single-target `PassThrough`, heterogeneous multi-target `CspaMixer` (`InputNormalizer + FrameReblocker` + optional `OutputConverter`, `0` steady-state `read_into` heap allocations), event-driven `IMMNotificationClient` `Follow` endpoint switching, `OnRootExit` relaunch handling, and live `update_targets` hot-swap.
- **Recording Permissions Inspection:**
  - `PlatformPermissions` inspects Group Policy (`LetAppsAccessMicrophone`) and `CapabilityAccessManager\ConsentStore\microphone` (`NonPackaged`) without triggering modal dialogs during preflight, with opt-in `ms-settings:privacy-microphone` navigation.

## Links & License

- **Official User Manual & Platform Docs:** <https://s0d3s.github.io/capture-system-or-process-audio/concepts/platforms/>
- **Main Crate & Documentation:** <https://crates.io/crates/capture-system-or-process-audio> · <https://docs.rs/capture-system-or-process-audio>
- **Repository & Notices:** <https://github.com/s0d3s/capture-system-or-process-audio> · [THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture-windows/THIRD_PARTY_NOTICES.md)
- **License:** Dual-licensed under [MIT](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-APACHE), at your option.