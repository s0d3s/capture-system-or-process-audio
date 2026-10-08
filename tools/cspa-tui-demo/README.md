# CSPA Studio Demo (`cspa-tui-demo`)

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

Interactive terminal audio capture, multi-source live mixer, OS recording-permissions inspector, 3-mode real-time spectroscope, and in-memory playback studio built with `ratatui` (`#![forbid(unsafe_code)]`) directly on top of the unified [`capture-system-or-process-audio`](https://github.com/s0d3s/capture-system-or-process-audio) Rust facade.

---

## Quick Run

### 1. Prebuilt Standalone Binary *(No Rust Toolchain Required)*

Download `cspa-tui-demo-<version>-<target>.zip` (Windows `x64`/`ARM64`) or `.tar.gz` (Linux `x86_64`/`aarch64`, macOS `arm64`) from [GitHub Releases](https://github.com/s0d3s/capture-system-or-process-audio/releases/latest) and run it in any truecolor terminal (`100x36` or larger recommended):

```bash
# Linux / macOS:
./cspa-tui-demo

# Windows PowerShell / Terminal:
.\cspa-tui-demo.exe
```

### 2. Build & Run From Source

```bash
cargo run --release --manifest-path tools/cspa-tui-demo/Cargo.toml
cargo run --release --manifest-path tools/cspa-tui-demo/Cargo.toml -- --smoke-test
```

---

## Keyboard & Mouse Shortcut Cheat Sheet

Every button, tab, and toggle is also clickable directly with the mouse!

| Key / Click | Action |
| :---: | --- |
| `A` / `[+ Add Source]` | Open live **Add Capture Sources** modal (**Applications**, **Processes**, **Output Devices**, **Microphones**) |
| `R` / `[Record]` · `[Stop]` | Start or stop recording into a RAM `MemoryTake` (`S` also stops recording or active playback) |
| `Space` / `[x]` checkbox | Enable or disable the selected staged source row (live hot-swap!) or toggle picker selection |
| `P` / `[Play]` | Play / pause the selected in-memory take via `cpal` (click progress bar to seek) |
| `W` / `[Save WAV]` | Export the selected take in its exact captured format (`F32`, `I16`, `I24`, `I32`) to `./recordings/` |
| `V` / Spectroscope Tabs | Cycle real-time **2048-pt FFT Spectrum**, **Truecolor Waterfall**, and **Braille Oscilloscope** |
| `K` / `[Perms: ... (K)]` | Open **OS Recording Permissions & Consent** inspector (`1`/`2`/`3` prompt/probe, `O` OS Privacy Settings) |
| `B` / `[AUTO]` chip | Filter backend in picker modal or pin backend (`WASAPI`, `PW`, `PA`, `CA`, `SCK`) on staged source |
| `G` / `[1.0x]` chip | Cycle per-source gain (`1.0x`, `0.5x`, `1.5x`, `0.25x`) or trigger tab consent in picker modal |
| `C` | Cycle session `CombineMode` (`Auto` \| `NativeOnly` \| `CspaMixer`) |
| `Q` / `Esc` | Close active modal or exit cleanly |

---

## Highlights

- **Staged Audio Sources & Live Modal Picker (`[+ Add Source]` / `A`):**
  - Browse 4 live tabs: **Applications** (parent-tree roll-up), **Processes** (PIDs), **Output Devices** (`default_output` + endpoints), and **Microphones** (`microphone` + physical/virtual audio inputs with `.monitor` sinks excluded).
  - Unified `DiscoveryQuery` controls in the modal: filter by **Backend** (`B`), toggle **Dedupe** (`U`: `ByLogicalTarget` vs `PerBackend` `<backend>:<kind>:...` entries), and toggle **Order** (`O`: `KindThenBackend` vs `BackendThenKind`), backed by bounded `256`-event `subscribe_query` cursor updates.
  - Per-source controls on the staged mixing board: `[MIC]`/`[IN]`/`[SYS]`/`[DEV]`/`[APP]`/`[PID]` kind badges, per-target backend pinning, per-target gain, `Follow`/`Pin` endpoint tracking on both `DefaultOutput` and `DefaultInput`, `Tree`/`Exact` scope, and `Stop`/`Wait` root-exit policy.
- **Two-Tier Live Hot-Swap During Recording:**
  - Checking, unchecking, or retargeting sources while recording is active hot-swaps the underlying `CaptureManager` (`update_targets` across `SharedNativeGraphMix` and `CspaMixer`, with automatic session transition if working format promotes) and seamlessly appends a new segment into the active in-memory take without losing previously recorded audio.
- **Capture Plan, Mixing & Converter Configuration + Live Telemetry:**
  - Side-effect-free `CaptureRequest::inspect()` preview displaying session `CombineResolution` (`PassThrough`, `SharedNativeGraphMix`, or `CspaMixer`), per-target `(resolved_backend, acquisition)`, `OverlapCombinationSemantics` (`SingleTarget`, `SoftwareSummation`, `NativeSetUnion`), active stream count, and estimated PCM memory.
  - Interactive toggles for session `CombineMode` (`Auto` | `NativeOnly` | `CspaMixer`), `ProcessTreeRange` (`NativeKernelCapture..SoftwareStreamMix` | `NativeKernelCapture..NativeGraphMix` | `NativeKernelCapture` | `SoftwareStreamMix`), `InputConversionPolicy` (`AutoWhenMixing` | `StrictNoResample`), `silence_on_idle`, `Overflow`, and `OutputConverter` (`48000`/`44100`/`16000` Hz, `Stereo`/`Mono`, `F32`/`I16`/`I24`/`I32`, `TPDF`/`None` dither).
  - Live `[4] Active Stream & Mixer Telemetry` panel displays clock-domain-gated skew (`Skew X.XXms` vs `Skew cross-clock`) and per-input `InputStatusHandle` normalizer badges (`48k->48k BYPASS` vs `16k 1ch->48k 2ch SYNC`).
- **Cross-Platform Recording Permissions Inspector & Consent Controls (`[Perms: ... (K)]` / `K`):**
  - Header status pill (`OK`, `ASK?`, `UNPRB`, `DENY!`, `RLNCH`, `ASK..`) + inline `Mic`/`Sys`/`Scr` domain chips, `PermissionPolicy` toggle (`[Pol:AutoRq]` vs `[Pol:ChkOnl]`), and `[Re-Check]` / `[Ask Needed]` / `[Perms UI (K)]` actions.
  - Dedicated `OS Recording Permissions & Consent` modal (`K`) with non-blocking background `request_permissions` execution, opt-in OS Privacy Settings fallback (`O`), and platform-specific diagnostics across Windows (`ConsentStore` vs Group Policy `Restricted`), macOS (`AVMediaTypeAudio`, Core Audio `CATap` probe, `ScreenCaptureKit` relaunch notice), and Linux (`PipeWire`/`PulseAudio` sockets & Flatpak sandboxes).
- **Acoustic Feedback Guard:**
  - Live monitoring is strictly visual (never routed to speakers, preventing microphone feedback loops) and automatically pauses during `cpal` take playback.

## Links & License

- **Complete Studio Demo Panel & Toggle Manual:** <https://s0d3s.github.io/capture-system-or-process-audio/studio-demo/>
- **Repository & SDKs:** <https://github.com/s0d3s/capture-system-or-process-audio>
- **License:** Dual-licensed under [MIT](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-APACHE), at your option.
