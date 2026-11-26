<div align="center">

  <img src="https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg" alt="capture-system-or-process-audio — Cross-platform system speaker loopback, single application/process, and microphone PCM audio capture library for Python" width="100%">

  <h1 align="center">capture-system-or-process-audio</h1>

  <p>
    <b>Capture system speakers, isolate any single app/process, or mix microphones into one live PCM stream across Windows, Linux &amp; macOS.</b><br>
    <sub>Native WASAPI · PipeWire · PulseAudio · Core Audio Process Taps · ScreenCaptureKit — zero virtual audio cables, zero silent fallbacks.</sub>
  </p>

  <p>
    <i>Created by <a href="https://github.com/s0d3s">@s0d3s</a> — author of <a href="https://github.com/s0d3s/PyAudioWPatch"><b>PyAudioWPatch</b></a> (140k+ monthly PyPI downloads), rebuilt from scratch in safe Rust + PyO3.</i>
  </p>

  <p>
    <a href="https://s0d3s.github.io/capture-system-or-process-audio/sdks/python/"><img src="https://img.shields.io/badge/docs-Python%20Handbook%20%26%20API-22d3ee?logo=readthedocs&logoColor=white" alt="Python SDK Handbook and API Reference"></a>
    <a href="https://pypi.org/project/capture-system-or-process-audio/"><img src="https://img.shields.io/pypi/v/capture-system-or-process-audio?label=PyPI&color=3776AB&logo=pypi&logoColor=white" alt="PyPI Version"></a>
    <a href="https://pypi.org/project/capture-system-or-process-audio/"><img src="https://img.shields.io/badge/Python-3.11–3.15%20%7C%203.14t–3.15t-3776AB?logo=python&logoColor=ffdd54" alt="Supported Python Versions: 3.11 through 3.15 and Free-Threaded 3.14t/3.15t"></a>
    <a href="https://github.com/s0d3s/capture-system-or-process-audio#platform--backend-matrix"><img src="https://img.shields.io/badge/OS-Windows%20%7C%20Linux%20%7C%20macOS-00bcd4" alt="Supported Platforms: Windows 10/11, Linux, macOS"></a>
    <a href="https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-2ecc71" alt="License: MIT OR Apache-2.0"></a>
  </p>

</div>

---

## Installation

Prebuilt binary wheels (`cp311-abi3`, `cp314-cp314t`, and PEP 803 `cp315-abi3.abi3t`) ship for **Windows (`x64`, `ARM64`)**, **Linux (`x86_64`, `aarch64` `manylinux_2_28`)**, and **macOS (`Apple Silicon arm64`, `Intel x86_64`)** — **no Rust or C compiler required**.

```bash
pip install capture-system-or-process-audio
# Optional: zero-copy NumPy ndarray views via block.to_numpy()
pip install "capture-system-or-process-audio[numpy]"
# Inspect active OS backends, permissions, and live CaptureTarget tables:
python -m capture_system_or_process_audio
```

- **GIL & Free-Threaded Python Ready:** Certified across CPython `3.11`–`3.15` plus Free-Threaded `3.14t` & `3.15t` (`-Xgil=0`, `Py_MOD_GIL_NOT_USED` — keeps `sys._is_gil_enabled() == False` without fallback warnings).
- **100% Typed (`py.typed` + `.pyi`) & Runnable (`python -m`):** Full IDE autocomplete and `mypy`/`pyright` type checking (`33` stdlib `enum.Enum` classes accepting `<Enum> | str`) plus a built-in zero-dependency diagnostic CLI (`python -m capture_system_or_process_audio`).

---

## Quick Start Recipes

### 1. Capture System Speaker Output (Loopback)

```python
import capture_system_or_process_audio as cspa

# Follows OS default speaker/headphone switches automatically (or pass default_device_mode="pin"):
with cspa.capture(cspa.default_output()) as stream:
    for outcome in stream:
        for block in outcome.blocks:
            pcm_bytes = memoryview(block)  # Zero-copy read-only PEP 3118 buffer
            print(f"{block.frames} frames @ {block.rate} Hz, {block.channels} ch ({block.sample_format.value}), {len(pcm_bytes)} B")
        break
```

### 2. List, Filter & Capture `CaptureTarget`s Deterministically

`cspa.find_app("Name")` is a quick single-match shortcut, but it raises an error when multiple instances run (for example, multiple browser profiles or Python/Java workers) or when display names vary across platforms. Enumerating `cspa.discover()` lets you inspect every available `CaptureTarget` (`applications`, `processes`, `output_devices`, `input_devices`), filter deterministically on `active_audio`, `pid`, `executable_path`, `app_id`, or `device_id`, refine with `.with_gain(...)` / `.with_backend(...)`, and pass the exact target(s) to `cspa.capture(...)`:

```python
import os
import capture_system_or_process_audio as cspa

catalog = cspa.discover(active_only=False)

for t in catalog.targets:
    print(f"{t.kind.value:12} name={t.name!r} pid={t.pid} active={t.active_audio} id={t.logical_id}")

# Select active audio-producing applications (excluding our own PID), or fall back to default speakers:
my_pid = os.getpid()
playing_apps = [
    app.with_gain(0.9)
    for app in catalog.applications
    if app.active_audio is True and app.pid != my_pid
]
selected = playing_apps[:2] or [cspa.default_output()]

with cspa.capture(selected, silence_on_idle=True) as stream:
    outcome = stream.read(frames=960, timeout_ms=250)
    print(f"Captured {outcome.frames} frames across {len(stream.targets)} target(s)")
```

### 3. Single-Match App Shortcut (`find_app`) or Explicit PID (`process`)

When you already know a unique application name or exact OS PID:

```python
import capture_system_or_process_audio as cspa

# Roll up child audio helpers automatically (e.g. browser/Electron/game audio workers):
spotify = cspa.find_app("Spotify")  # Or cspa.process(1234, scope="tree")

with cspa.capture(spotify) as stream:
    outcome = stream.read(frames=960, timeout_ms=250)
    for block in outcome.blocks:
        raw_bytes = block.to_bytes()
        print("Captured isolated app bytes:", len(raw_bytes))
```

### 4. Mix Microphone + Meeting App at `16 kHz` Mono for Whisper / STT

```python
import capture_system_or_process_audio as cspa

converter = cspa.OutputConverter(rate=16000, channels=1, sample_format="f32")

with cspa.capture(
    [cspa.microphone(), cspa.find_app("Zoom")],
    output_converter=converter,
    silence_on_idle=True,  # Keep emitting silent frames when speakers are quiet
) as stream:
    outcome = stream.read(frames=1600, timeout_ms=200)  # 100 ms @ 16 kHz
    for block in outcome.blocks:
        samples = block.to_numpy()  # 2D C-contiguous ndarray: shape (frames, 1), dtype float32
```

### 5. `asyncio`, Zero-Allocation `read_into`, & Live Target Hot-Swap

```python
import asyncio
import capture_system_or_process_audio as cspa

async def main() -> None:
    # Non-blocking async iteration / read_async:
    async with cspa.capture(cspa.default_output(), silence_on_idle=True) as stream:
        outcome = await stream.read_async(frames=480, timeout_ms=200)
        print("Async frames:", outcome.frames)

        # Hot-swap capture targets mid-session without losing the stream handle:
        stream.update_targets([cspa.default_output(), cspa.microphone()])

    # Zero-allocation steady-state read_into (safe on -Xgil=0 via Python critical sections):
    conv = cspa.OutputConverter(rate=48000, channels=2, sample_format="i16")
    with cspa.capture(cspa.default_output(), output_converter=conv, silence_on_idle=True) as sync_stream:
        buf = bytearray(480 * 2 * 2)
        res = sync_stream.read_into(buf, frames=480, timeout_ms=200)
        print("Copied bytes:", res.copy_bytes)

asyncio.run(main())
```

---

## Migrating From `PyAudioWPatch` & Ecosystem Drop-In `Adapters`

[`PyAudioWPatch`](https://github.com/s0d3s/PyAudioWPatch) (created by `@s0d3s`, **140,000+ monthly PyPI downloads** across **340+ open-source repositories**) brought Windows WASAPI loopback to `PyAudio`, but PortAudio's C architecture made the most-requested user features — single-app isolation, macOS/Linux capture, `[Mic + Speaker]` drift sync, silent-speaker hang protection, and `-Xgil=0` free-threading — impossible to implement there. **`capture-system-or-process-audio` is the result of nearly a year of ground-up safe-Rust development and polish** built specifically to solve them:

| Common Task | `PyAudioWPatch` (Windows Only) | `capture_system_or_process_audio` (Win · Linux · macOS) |
| --- | --- | --- |
| Default speaker loopback | `p.get_default_wasapi_loopback()` | `cspa.default_output()` *(auto-follows device changes!)* |
| Record one specific app | Not supported | `cspa.find_app("Zoom")` or `cspa.process(pid)` |
| Prevent hang during speaker silence | Blocks forever on `stream.read()` | Pass `silence_on_idle=True` to `cspa.capture(...)` |
| Resample to `16 kHz` mono `i16`/`f32` | Manual external resampler | `output_converter=cspa.OutputConverter(rate=16000, channels=1, sample_format="i16")` |
| Zero-copy NumPy array | `np.frombuffer(data, dtype=...)` | `block.to_numpy(copy=False)` -> `(frames, channels)` |

> **Planned for Next Release — Zero-Rewrite Drop-In `Adapters`:** Upcoming compatibility wrappers for **`PyAudio` / `PyAudioWPatch`** (`Stream.read` & `stream_callback`), **`python-sounddevice`** (`InputStream`, `RawInputStream`, `rec`), **`SoundCard`** (`_Recorder`), **`SpeechRecognition`** (`AudioSource`), and **`faster-whisper`** sliding-window streams will let existing Python audio code switch to per-process or cross-platform loopback with a 1-line import swap. Follow progress on [GitHub](https://github.com/s0d3s/capture-system-or-process-audio#drop-in-ecosystem-adapters--nodejs-sdk-planned--next-release).

---

## OS Recording Permissions & Discovery

Inspect or request OS recording consent (`Microphone`, `SystemAudio`, `ScreenRecording`) cleanly across Windows, macOS, and Linux:

```python
import capture_system_or_process_audio as cspa

# Side-effect-free preflight (never triggers modal OS popups on CI or background workers):
report = cspa.check_permissions(cspa.default_output())
print("System audio state:", report.system_audio.state.value, "granted:", report.system_audio.is_granted)

# Enumerate active audio-producing applications, output endpoints, and microphones:
catalog = cspa.discover(active_only=False)
for app in catalog.applications:
    print(f"App: {app.name} (pid={app.pid}, backends={[b.value for b in app.available_backends]})")

# Preview the exact backend & mixer plan without opening hardware streams:
plan = cspa.request([cspa.microphone(), cspa.default_output()]).inspect()
print("Resolution:", plan.combine_resolution.value, "software_mix:", plan.uses_software_mixer)
```

- Streams use `PermissionPolicy.AUTO_REQUEST` by default on interactive runs; pass `request_permissions=False` (`PermissionPolicy.CHECK_ONLY`) to opt out of OS prompts and raise standard Python `PermissionError` (`OSError`) when denied.

---

## Platform Support Matrix

Capabilities differ across operating systems. Unsupported scopes or OS versions raise explicit errors rather than silently falling back to full-system capture:

| Platform | Backends (`Backends`) | Supported Scopes & Capabilities | Explicit Rejections / Notes |
| --- | --- | --- | --- |
| **Windows 10 / 11** (`x86_64`, `ARM64`) | `[BackendId.WASAPI]` | `default_output` & `microphone` (`follow`/`pin`), `device(id)` & `input_device(id)` (`>= 19041`); `process`/`application` `scope="tree"` (`>= 20348`) and `cspa_mixer`. | `scope="exact"` and uncalibrated `timing="precise"` reject explicitly. |
| **Linux** (`x86_64`, `aarch64`) | `[BackendId.PIPEWIRE, BackendId.PULSEAUDIO]` (runtime `dlopen` of `libpipewire-0.3.so.0 >= 0.3.65` & `libpulse.so.0 >= 15.0`, `0` compile-time `NEEDED` links) | PipeWire & PulseAudio output endpoints, microphones (`.monitor` sinks excluded), PipeWire `native_graph_mix` fan-in, PulseAudio per-stream `cspa_mixer`, and cross-backend `[pipewire + pulseaudio]` `cspa_mixer`. | `native_kernel_capture` for `process`/`application` and uncalibrated `timing="precise"` reject explicitly. |
| **macOS** (`Apple Silicon arm64`, `Intel x86_64`) | `[BackendId.COREAUDIO, BackendId.SCREENCAPTUREKIT]` | Core Audio Process Taps (`macOS 14.2+`), Core Audio HAL microphone input (`macOS 13.0+`), ScreenCaptureKit `application` (`scope="exact"`, `macOS 13.0+`), and cross-backend `[screencapturekit + coreaudio]` `cspa_mixer`. | SCK `scope="tree"` and uncalibrated `timing="precise"` reject explicitly. |

---

## Links & License

- **Official Documentation, Python Handbook & `.pyi` API Reference:** <https://s0d3s.github.io/capture-system-or-process-audio/>
- **Source, Rust / C / C++ SDKs & Live TUI Demo:** <https://github.com/s0d3s/capture-system-or-process-audio>
- **Prebuilt Binaries & Releases:** <https://github.com/s0d3s/capture-system-or-process-audio/releases>
- **License:** Dual-licensed under [MIT](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-APACHE), at your option.
