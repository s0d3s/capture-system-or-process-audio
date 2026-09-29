# capture-system-or-process-audio-linux

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

> **Looking to capture system, application, or microphone audio in Rust?**
> Depend on the unified safe-Rust facade crate **[`capture-system-or-process-audio`](https://crates.io/crates/capture-system-or-process-audio)** instead of importing this internal Linux backend crate directly.

`capture-system-or-process-audio-linux` implements the runtime-loaded PipeWire (`libpipewire-0.3.so.0 >= 0.3.65`) and PulseAudio (`libpulse.so.0 >= 15.0`) backend (`PlatformStream`, `PlatformDiscovery`, `PlatformManager`, `PlatformPermissions`, `PlatformDefaultOutputObserver`, `PlatformDefaultInputObserver`, and `PlatformTopologyController`) for `capture-system-or-process-audio`:

- **Runtime Client Loading (`0` Compile-Time `NEEDED` Links):** No compile-time `-lpipewire-0.3` or `-lpulse` link dependencies are required. Absent or symbol-incomplete client libraries report `CapabilityError::ClientLibraryUnavailable` and outdated library versions report `CapabilityError::BackendVersionTooOld`, disabling only the affected backend while preserving cross-backend fallback when configured.
- **Process Identity & Lazy Discovery:** `PlatformDiscovery` reads `/proc/<pid>/stat` `(pid, ppid, starttime)` and resolves originating PipeWire Client `application.process.id` (never `pipewire.sec.pid`) and PulseAudio `sink_input` / `client` `application.process.id`, excluding `.monitor` sink monitors from `input_devices()`, rolling child helpers up to owning root applications, and supporting `DiscoveryQuery` (`ByLogicalTarget` & `PerBackend`) plus bounded `256`-event cursor subscriptions with zero background threads left after one-shot calls.
- **Topologies, `0`-Alloc `CspaMixer`, & Routing Safety:** `PlatformManager` orchestrates `PassThrough`, PipeWire `SharedNativeGraphMix` (`PlatformTopologyController` `support.null-audio-sink` + passive non-lingering FL/FR port links preserving speaker playback), and heterogeneous / cross-backend `CspaMixer` (`PipeWire` + `PulseAudio`, `InputNormalizer + FrameReblocker` + optional `OutputConverter`, `0` steady-state `read_into` heap allocations) with live `update_targets` hot-swap and `RouteRollbackGuard`.

## Links & License

- **Official User Manual & Platform Docs:** <https://s0d3s.github.io/capture-system-or-process-audio/concepts/platforms/>
- **Main Crate & Documentation:** <https://crates.io/crates/capture-system-or-process-audio> · <https://docs.rs/capture-system-or-process-audio>
- **Repository & Notices:** <https://github.com/s0d3s/capture-system-or-process-audio> · [THIRD_PARTY_NOTICES.md](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/crates/capture-linux/THIRD_PARTY_NOTICES.md)
- **License:** Dual-licensed under [MIT](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-APACHE), at your option.
