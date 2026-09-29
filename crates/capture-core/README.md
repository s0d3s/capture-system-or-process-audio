# capture-system-or-process-audio-core

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

> **Looking to capture system, application, or microphone audio in Rust?**
> Depend on the unified safe-Rust facade crate **[`capture-system-or-process-audio`](https://crates.io/crates/capture-system-or-process-audio)** instead of importing this internal portable core crate directly.

`capture-system-or-process-audio-core` (`#![forbid(unsafe_code)]`) owns the platform-independent schemas, lock-free bounded SPSC handoff, DSP pipeline, planning engine, and backend trait hierarchy shared by the Windows, Linux, and macOS backend crates:

- **Core Backend Trait Hierarchy (`traits`):** Defines `AudioStreamBackend`, `OpenableStreamBackend`, `DiscoveryBackend`, `CaptureManagerBackend`, `PermissionsBackend`, `DefaultEndpointObserver`, and `TopologyControllerBackend`, alongside zero-cost `#[repr(transparent)]` static-dispatch wrappers (`AudioStream<B>`, `Discovery<B>`, `CaptureManager<B>`, `Permissions<B>`).
- **Bounded Lock-Free PCM Ownership (`storage`, `ordering`, `reads`, `lifecycle`):** Preallocated `storage::Pool` (`Pool::new_reserved` charged against `manager::StorageBudget`) and ticket-watermarked single-producer / single-consumer `ordering::channel` supporting zero-extra-copy variable reads, reusable-buffer `read_into`, per-operation `Cancellation`, sequential `CallbackStream` delivery with unwind containment, and `Stop::Drain` vs `Stop::Abort` teardown.
- **Target Schema, Discovery & Planner (`schema`, `discovery`, `planner`, `routing`):** Deterministic `CaptureTarget` (`process`, `application`, `device`, `default_output`, `default_input` / `microphone`, `input_device`), `PermissionReport` (`PermissionKind`, 5-state `PermissionState`, `PermissionPolicy`), `DiscoveryService` (parent-tree application roll-up, `ByLogicalTarget` & `PerBackend` deduplication, bounded `256`-event cursor subscriptions), side-effect-free `CaptureRequest::inspect()`, and LIFO `RouteRollbackGuard`.
- **Zero-Allocation Steady-State Mixing & Conversion (`converter`, `mixer`, `manager`):** Stateful pre-mix `InputNormalizer` (64-tap 4-term Blackman-Harris polyphase sinc resampler + channel mapper) and `FrameReblocker`, `CspaMixer` (`20 ms` alignment, `f32` accumulation, `NaN`/`±Inf` sanitization, clock-domain-gated skew and `5 ms` / `64`-frame crossfade drift compensation, `silence_on_idle`), post-mix `OutputConverter` (`F32`/`I16`/`I24`/`I32` + stateless SplitMix64 TPDF dither), and `MultiInputManager` (`0` steady-state heap allocations on `read_mixed_into`).

## Links & License

- **Official User Manual & Architecture Docs:** <https://s0d3s.github.io/capture-system-or-process-audio/>
- **Main Crate & Documentation:** <https://crates.io/crates/capture-system-or-process-audio> · <https://docs.rs/capture-system-or-process-audio>
- **Repository:** <https://github.com/s0d3s/capture-system-or-process-audio>
- **License:** Dual-licensed under [MIT](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-APACHE), at your option.