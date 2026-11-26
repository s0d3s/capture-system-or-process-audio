<div align="center">

  <img src="assets/repo_banner.svg" alt="capture-system-or-process-audio — Cross-platform system speaker loopback, single application/process, and microphone PCM audio capture library for Python, Rust, C, and C++" width="780" height="260">

  <h1 align="center">capture-system-or-process-audio</h1>

  <p>
    <b>Capture system speakers, isolate any single app/process, or mix microphones into one live PCM stream across Windows, Linux &amp; macOS.</b><br>
    <sub>Native WASAPI · PipeWire · PulseAudio · Core Audio Process Taps · ScreenCaptureKit — zero virtual audio cables, zero silent fallbacks.</sub><br>
  </p>
  <p>
    <a href="https://s0d3s.github.io/capture-system-or-process-audio/"><img src="https://img.shields.io/badge/docs-User%20Manual%20%26%20API%20Refs-22d3ee?logo=readthedocs&logoColor=white" alt="Official Documentation, User Manuals, and Multi-SDK API Reference"></a>
    <a href="https://github.com/s0d3s/capture-system-or-process-audio/releases/latest"><img src="https://img.shields.io/github/v/release/s0d3s/capture-system-or-process-audio?include_prereleases&label=release&color=e040fb&logo=github" alt="Latest GitHub Release"></a>
    <a href="https://github.com/s0d3s/capture-system-or-process-audio/actions/workflows/publish-release.yml"><img src="https://img.shields.io/github/actions/workflow/status/s0d3s/capture-system-or-process-audio/publish-release.yml?label=build%20%26%20smoke&logo=githubactions&logoColor=white" alt="Build and Smoke Test Status"></a>
    <a href="#platform--backend-matrix"><img src="https://img.shields.io/badge/OS-Windows%20%7C%20Linux%20%7C%20macOS-00bcd4" alt="Supported OS: Windows 10/11, Linux PipeWire/PulseAudio, macOS 13+/14.2+"></a>
    <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-2ecc71" alt="License: MIT OR Apache-2.0"></a>
  </p>
  <p>
    <i>Created by <a href="https://github.com/s0d3s">@s0d3s</a> — author of <a href="https://github.com/s0d3s/PyAudioWPatch"><b>PyAudioWPatch</b></a> (140k+ monthly PyPI downloads, 340+ dependent repos) — released after nearly a year of ground-up safe-Rust engineering &amp; polish.</i>
  </p>


| SDK | Package / Distribution | Quick Start |
| :--- | :---: | :---: |
| ![Python](https://img.shields.io/badge/Python-3.11–3.15t-3776AB?style=for-the-badge&logo=python&logoColor=ffdd54) | [![PyPI](https://img.shields.io/pypi/v/capture-system-or-process-audio?label=PyPI&color=3776AB)](https://pypi.org/project/capture-system-or-process-audio/) [![PyPI Downloads](https://img.shields.io/pypi/dm/capture-system-or-process-audio?label=downloads&color=3776ab)](https://pypi.org/project/capture-system-or-process-audio/) | **[⚡ Quick Start ↓](#-python-quick-start)** |
| ![Rust](https://img.shields.io/badge/Rust-1.88%2B-CE422B?style=for-the-badge&logo=rust&logoColor=white) | [![crates.io](https://img.shields.io/crates/v/capture-system-or-process-audio?label=crates.io&color=CE422B)](https://crates.io/crates/capture-system-or-process-audio) [![docs.rs](https://img.shields.io/docsrs/capture-system-or-process-audio?label=docs.rs)](https://docs.rs/capture-system-or-process-audio) | **[⚡ Quick Start ↓](#-rust-quick-start)** |
| ![C++17](https://img.shields.io/badge/C%2B%2B-C%2B%2B17_RAII-00599C?style=for-the-badge&logo=cplusplus&logoColor=white) | [![CMake](https://img.shields.io/badge/CMake-Cspa%3A%3Acspa__ffi-064F8C?logo=cmake&logoColor=white)](https://github.com/s0d3s/capture-system-or-process-audio/releases/latest) | **[⚡ Quick Start ↓](#%EF%B8%8F-c17-quick-start-header-only-raii)** |
| ![C11](https://img.shields.io/badge/C-C11_ABI-555555?style=for-the-badge&logo=c&logoColor=A8B9CC) | [![Prebuilt SDK](https://img.shields.io/github/v/release/s0d3s/capture-system-or-process-audio?include_prereleases&label=cspa--ffi%20SDK&color=555555)](https://github.com/s0d3s/capture-system-or-process-audio/releases/latest) | **[⚡ Quick Start ↓](#-c11-quick-start-stable-ffi-boundary)** |
| ![Node.js](https://img.shields.io/badge/Node.js_%2F_TS-N--API-339933?style=for-the-badge&logo=nodedotjs&logoColor=white) | ![Planned](https://img.shields.io/badge/npm-planned-6e7681?logo=npm&logoColor=white) | **[🚧 Planned ↓](#drop-in-ecosystem-adapters--nodejs-sdk-planned--next-release)** |

  <p>
    <sub>
      <a href="https://s0d3s.github.io/capture-system-or-process-audio/"><b>Full Documentation &amp; API Reference</b></a> •
      <a href="#what-can-you-capture">Overview</a> •
      <a href="#interactive-studio-demo">Live TUI Demo</a> •
      <a href="#drop-in-ecosystem-adapters--nodejs-sdk-planned--next-release">Drop-In Adapters Roadmap</a> •
      <a href="#why-this-exists--beyond-pyaudiowpatch">Why Not PyAudioWPatch?</a> •
      <a href="#platform--backend-matrix">OS Support</a> •
      <a href="#recording-permissions">Permissions</a> •
      <a href="#documentation--guides">Docs Map</a>
    </sub>
  </p>

</div>

---

## What Can You Capture?

`capture-system-or-process-audio` (`pip install capture-system-or-process-audio` / `cargo add capture-system-or-process-audio`) is a cross-platform native audio capture and live-mixing engine for **Windows 10/11** (`WASAPI`), **Linux** (`PipeWire` & `PulseAudio`), and **macOS** (`Core Audio Process Taps` & `ScreenCaptureKit` on Apple Silicon & Intel). Describe *what* audio you want, and receive decoded PCM frames ready for `NumPy`, `faster-whisper` / `Whisper` STT, `OpenAI Realtime` / `LiveKit` voice agents, `WebRTC`, `Tauri` / desktop apps, DSP, or file encoders — **without installing VB-Cable, BlackHole, or Soundflower**:

- 🔊 **System / Speaker Loopback (`default_output`, `device`):** Capture audio playing on the user's speakers or headphones — automatically following OS default-device switches (`follow` default) or pinning a specific endpoint (`pin`).
- 🎯 **Single Application or Process Tree (`find_app`, `process`):** Isolate audio from *just* `Zoom`, `Microsoft Teams`, `Google Meet` (browser), `Spotify`, `Firefox`, or a game (including helper child processes where supported) without recording Discord notifications or unrelated apps.
- 🎙️ **Microphones & Audio Inputs (`microphone`, `input_device`):** Capture default or specific microphones with automatic sink-monitor exclusion and live device-switch tracking.
- 🎛️ **Drift-Compensated Multi-Source Mixing + Format Conversion:** Pass `[microphone(), find_app("Zoom")]` to receive **one synchronized PCM stream** converted on the fly (`16 kHz` mono `f32`/`i16` for `faster-whisper` & STT, `24 kHz` `i16` for Realtime Voice AI, or `48 kHz` stereo `f32` for `WebRTC` & studio recording) with **zero steady-state heap allocations** on `read_into`.

<div align="center">

  <img src="assets/cspa_demo_tui_app.webp" alt="cspa-tui-demo live terminal audio mixer, spectroscope, and per-process capture picker" width="800" height="477">
  <br>
  <sub><b>Try it in 10 seconds without writing code:</b> Download <code>cspa-tui-demo</code> from <a href="https://github.com/s0d3s/capture-system-or-process-audio/releases/latest">Releases</a> to browse live apps/mics, hot-swap mixes while recording, and inspect real-time FFT, Waterfall, and Braille Oscilloscope views.</sub>

</div>

---

## Quick Starts by SDK

### 🐍 Python Quick Start

Prebuilt binary wheels (`cp311-abi3`, `cp314t`, and PEP 803 `cp315-abi3.abi3t`) support **CPython `3.11`–`3.15`** plus **Free-Threaded Python `3.14t` & `3.15t` (`-Xgil=0`)** across Windows (`x64`/`ARM64`), Linux (`x86_64`/`aarch64` `manylinux_2_28`), and macOS (`Apple Silicon arm64` & `Intel x86_64`) with zero mandatory runtime dependencies and complete `.pyi` type stubs.

```bash
pip install capture-system-or-process-audio
# Optional: zero-copy NumPy ndarray views via block.to_numpy()
pip install "capture-system-or-process-audio[numpy]"
# Inspect current platform/backends and live CaptureTargets in your terminal:
python -m capture_system_or_process_audio
```

**1. Capture default speakers (or isolate a single running application):**

```python
import capture_system_or_process_audio as cspa

# Capture default speakers/headphones (automatically follows OS device switches):
target = cspa.default_output()
# Or isolate one running application (and its audio helper processes):
# target = cspa.find_app("Spotify")

with cspa.capture(target) as stream:
    for outcome in stream:
        for block in outcome.blocks:
            pcm_view = memoryview(block)  # Zero-copy PEP 3118 read-only buffer
            print(f"{block.frames} frames @ {block.rate} Hz, {block.channels} ch ({block.sample_format.value}), {len(pcm_view)} B")
        break
```

**2. List, filter & use available `CaptureTarget`s deterministically:**

`cspa.find_app("Name")` is a quick single-match helper, but real desktops often run multiple instances, localized names, or multiple USB headsets. Enumerate `cspa.discover()` to filter deterministically by `active_audio`, `pid`, `executable_path`, `app_id`, or `device_id` and pass selected `CaptureTarget`s straight into `cspa.capture(...)`:

```python
import os
import capture_system_or_process_audio as cspa

catalog = cspa.discover(active_only=False)

for t in catalog.targets:
    print(f"{t.kind.value:12} name={t.name!r} pid={t.pid} active={t.active_audio} id={t.logical_id}")

# Deterministically pick active audio apps (excluding our own PID) or fall back to default output:
my_pid = os.getpid()
active_apps = [
    app.with_gain(0.9)
    for app in catalog.applications
    if app.active_audio is True and app.pid != my_pid and (app.executable_path or app.app_id or app.name)
]
selected = active_apps[:2] or [cspa.default_output()]

with cspa.capture(selected, silence_on_idle=True) as stream:
    outcome = stream.read(frames=480, timeout_ms=200)
    print(f"Captured {outcome.frames} frames from {len(stream.targets)} target(s)")
```

**3. Mix Microphone + Meeting App (`Zoom` / `Teams`) for `faster-whisper`, STT & Voice Agents:**

```python
import capture_system_or_process_audio as cspa

# 16 kHz mono f32 for faster-whisper / Whisper (or rate=24000, sample_format="i16" for OpenAI Realtime / LiveKit):
converter = cspa.OutputConverter(rate=16000, channels=1, sample_format="f32")

with cspa.capture(
    [cspa.microphone(), cspa.find_app("Zoom")],  # Or "Teams", "Chrome", etc.
    output_converter=converter,
    silence_on_idle=True,  # Emit continuous silent frames when speakers are idle
) as stream:
    outcome = stream.read(frames=1600, timeout_ms=200)  # 100 ms @ 16 kHz
    for block in outcome.blocks:
        samples = block.to_numpy()  # shape: (frames, 1), dtype: float32
```

<details>
<summary><b>More Python patterns: <code>asyncio</code>, zero-allocation <code>read_into</code>, callbacks &amp; live hot-swap</b></summary>

```python
import asyncio
import capture_system_or_process_audio as cspa

async def main() -> None:
    async with cspa.capture(cspa.default_output(), silence_on_idle=True) as stream:
        outcome = await stream.read_async(frames=480, timeout_ms=200)
        print("Async frames:", outcome.frames)

    # Zero-allocation steady-state read_into (safe on free-threaded Python 3.14t/3.15t):
    conv = cspa.OutputConverter(rate=48000, channels=2, sample_format="i16")
    with cspa.capture(cspa.default_output(), output_converter=conv, silence_on_idle=True) as sync_stream:
        buf = bytearray(480 * 2 * 2)
        res = sync_stream.read_into(buf, frames=480, timeout_ms=200)
        print("Copied bytes:", res.copy_bytes)

asyncio.run(main())
```

</details>

📖 **Python Docs:** [Python SDK Handbook](https://s0d3s.github.io/capture-system-or-process-audio/sdks/python/) • [Python API Reference](https://s0d3s.github.io/capture-system-or-process-audio/reference/python/functions/) • [Package README](crates/capture-python/README.md)

<p align="right"><sub><a href="#capture-system-or-process-audio">↑ Back to SDKs</a></sub></p>

---

### 🦀 Rust Quick Start

The unified `capture-system-or-process-audio` facade crate enforces **`#![forbid(unsafe_code)]`** and automatically selects the native OS backend (`WASAPI`, `PipeWire`/`PulseAudio`, or `Core Audio`/`ScreenCaptureKit`) behind one single cross-platform API — ideal for **Tauri** desktop apps, native AI meeting assistants, real-time voice agents, and low-latency DSP engines.

```bash
cargo add capture-system-or-process-audio
```

```rust
use capture_system_or_process_audio::{
    CapabilityError, CaptureManager, CaptureTarget, DefaultDeviceMode, DiscoveryQuery,
    MixerConfig, PartialReason, Stop, capture_single, discover_query, request,
};
use std::time::Duration;

fn main() -> Result<(), CapabilityError> {
    // 1. Capture default output (follows OS default speaker/headphone switches):
    let target = CaptureTarget::default_output(DefaultDeviceMode::Follow);
    // Or isolate one running application (e.g. Zoom, Teams, Spotify):
    // let target = Discovery::new().find_app("Spotify", ProcessScope::Tree, OnRootExit::Stop)
    //     .map_err(|_| CapabilityError::NoSuchProcess)?;
    let mut manager = capture_single(target)?;

    // 2. Zero-allocation steady-state read into a reusable caller buffer (idle-speaker safe):
    let fmt = manager.output_format().expect("active output format").clone();
    let mut pcm_buf = vec![0u8; 480 * fmt.bytes_per_frame()];

    let res = manager
        .read_into(&mut pcm_buf, 480, Duration::from_millis(200))
        .expect("valid destination buffer");
    if res.frames == 0 && res.reason == PartialReason::Timeout {
        println!("Speakers are currently idle (enable silence_on_idle for continuous silence)");
    } else {
        println!(
            "Read {} frames ({} Hz, {} ch, {:?}, reason={:?})",
            res.frames,
            fmt.rate(),
            fmt.channels(),
            fmt.sample(),
            res.reason
        );
    }

    // 3. Deterministically list, filter & mix available CaptureTargets (excluding our own PID):
    let catalog = discover_query(&DiscoveryQuery::default())?;
    let self_pid = std::process::id();
    let mut selected: Vec<CaptureTarget> = catalog
        .applications
        .iter()
        .filter(|app| app.active_audio() == Some(true) && app.pid() != Some(self_pid))
        .take(2)
        .filter_map(|app| app.clone().with_gain(0.85).ok())
        .collect();
    selected.push(CaptureTarget::microphone(DefaultDeviceMode::Follow));

    let mix_req = request(selected).with_mixer_config(MixerConfig {
        silence_on_idle: true,
        ..MixerConfig::default()
    });
    let mut mix = CaptureManager::open(mix_req)?;
    if let Ok(block) = mix.read(480, Duration::from_millis(200)) {
        println!("Mixed {} frames ready for STT / WebRTC / Tauri IPC", block.frames);
    }
    mix.stop(Stop::Drain);
    mix.wait_cleanup(Duration::from_secs(2));

    manager.stop(Stop::Drain);
    manager.wait_cleanup(Duration::from_secs(2));
    Ok(())
}
```

📖 **Rust Docs:** [Rust SDK Handbook](https://s0d3s.github.io/capture-system-or-process-audio/sdks/rust/) • [Rust API Reference](https://s0d3s.github.io/capture-system-or-process-audio/reference/rust/overview/) • [docs.rs](https://docs.rs/capture-system-or-process-audio) • [Crate README](crates/capture/README.md)

<p align="right"><sub><a href="#capture-system-or-process-audio">↑ Back to SDKs</a></sub></p>

---

### ⚙️ C++17 Quick Start (Header-Only RAII)

Download the prebuilt `cspa-ffi-<version>-<target>` archive from [GitHub Releases](https://github.com/s0d3s/capture-system-or-process-audio/releases/latest) containing `include/cspa.hpp`, `include/cspa_experimental.h`, shared libraries, and `CspaConfig.cmake` — ready for native desktop recorders, **WebRTC** media clients, game/broadcast overlays, and local `whisper.cpp` / voice-AI pipelines. Supports **both** C++ exceptions (`/EHsc`, `-fexceptions` via `*_or_throw()`) and exception-free builds (`/EHs-c-`, `-fno-exceptions` via `cspa::Result<T>`).

```cmake
cmake_minimum_required(VERSION 3.20)
project(audio_example LANGUAGES CXX)

find_package(Cspa REQUIRED CONFIG)
add_executable(example main.cpp)
target_compile_features(example PRIVATE cxx_std_17)
target_link_libraries(example PRIVATE Cspa::cspa_ffi)
```

```cpp
#include <cspa.hpp>
#include <cstdio>
#include <vector>

int main() {
    auto disc_res = cspa::Discovery::create();
    if (!disc_res.is_ok()) return 1;
    cspa::Discovery disc = disc_res.take_value();

    // Deterministically list & filter available CaptureTargets (active apps + microphone):
    cspa::DiscoveryQuery query;
    auto catalog_res = disc.query(query);
    if (!catalog_res.is_ok()) return 1;

    std::vector<cspa::Target> selected;
    for (const cspa::Target &t : catalog_res.value()) {
        if (t.kind() == cspa::TargetKind::Application &&
            t.raw().active_audio == CSPA_ACTIVE_AUDIO_ACTIVE) {
            auto scaled = t.with_gain(0.85f);
            if (scaled.is_ok()) selected.push_back(scaled.take_value());
        }
    }
    if (selected.empty()) {
        selected.push_back(cspa::Target::default_output(cspa::DefaultDeviceMode::Follow).take_value());
    }
    selected.push_back(cspa::Target::microphone(cspa::DefaultDeviceMode::Follow).take_value());

    cspa::MixerConfig mixer;
    mixer.set_silence_on_idle(true);

    cspa::CaptureRequest req(std::move(selected));
    req.set_mixer_config(mixer)
       .set_output_converter(cspa::OutputConverter(48000, 2, cspa::SampleFormat::F32));

    auto stream_res = cspa::ManagedStream::open(req);
    if (!stream_res.is_ok()) return 1;
    cspa::ManagedStream stream = stream_res.take_value();

    std::vector<uint8_t> pcm(480 * 2 * sizeof(float));
    auto rd = stream.read_into(pcm.data(), pcm.size(), 480, 200);
    if (rd.is_ok()) {
        std::printf("Captured %llu mixed frames\n", static_cast<unsigned long long>(rd.value().frames));
    }

    stream.stop(false);
    stream.wait_cleanup(2000);
    return 0;
}
```

📖 **C++17 Docs:** [C++17 SDK Handbook](https://s0d3s.github.io/capture-system-or-process-audio/sdks/cpp/) • [C++17 API Reference](https://s0d3s.github.io/capture-system-or-process-audio/reference/cpp/classes-and-streams/) • [CMake & FFI SDK README](crates/capture-ffi/README.md)

<p align="right"><sub><a href="#capture-system-or-process-audio">↑ Back to SDKs</a></sub></p>

---

### 🧩 C11 Quick Start (Stable FFI Boundary)

`cspa_experimental.h` provides a size- and schema-revision-checked C11 ABI (`cspa_abi_check`) with `catch_unwind` panic containment across every exported symbol and **zero compile-time `NEEDED` links** to `libpipewire` or `libpulse` on Linux — ideal for C host applications, audio plugins, game engines, and custom FFI bindings (`Go`, `Zig`, `C#`, `Swift`).

```c
#include <cspa_experimental.h>
#include <stdint.h>
#include <stdio.h>

int main(void) {
    if (cspa_abi_check(CSPA_EXPERIMENTAL_ABI_MAJOR, CSPA_EXPERIMENTAL_SCHEMA_REVISION) != CSPA_STATUS_OK) {
        return 1;
    }

    /* 1. Deterministically enumerate & filter available CaptureTargets
          (or single-match by name via cspa_discovery_find_app(disc, "Spotify", ...)): */
    cspa_discovery_query_record query = {0};
    cspa_discovery_query_init(&query);
    cspa_capture_target_record discovered[32];
    size_t count = 0;

    cspa_capture_target_record target = {0};
    target.struct_size = (uint32_t)sizeof(target);
    target.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    int selected_active_app = 0;

    if (cspa_discover_query(&query, discovered, 32u, &count) == CSPA_STATUS_OK) {
        size_t n = count < 32u ? count : 32u;
        for (size_t i = 0; i < n; ++i) {
            if (discovered[i].kind == CSPA_TARGET_KIND_APPLICATION &&
                discovered[i].active_audio == CSPA_ACTIVE_AUDIO_ACTIVE) {
                target = discovered[i];
                selected_active_app = 1;
                break;
            }
        }
    }
    if (!selected_active_app &&
        cspa_target_default_output(CSPA_DEFAULT_DEVICE_FOLLOW, 1.0f, &target) != CSPA_STATUS_OK) {
        return 1;
    }

    /* 2. Open a managed session for the selected CaptureTarget: */
    cspa_capture_request_record req = {0};
    if (cspa_capture_request_init(&req, &target, 1u) != CSPA_STATUS_OK) return 1;

    cspa_manager *mgr = NULL;
    if (cspa_manager_open(&req, &mgr) != CSPA_STATUS_OK) return 1;

    uint8_t buffer[480 * 8];
    cspa_managed_read_outcome_record out = {0};
    out.struct_size = (uint32_t)sizeof(out);
    out.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;

    if (cspa_manager_read_into(mgr, buffer, sizeof(buffer), 480u, 200u, NULL, &out) == CSPA_STATUS_OK) {
        printf("Read %llu frames (%llu bytes)\n",
               (unsigned long long)out.actual_frames,
               (unsigned long long)out.copy_bytes);
    }

    cspa_manager_stop(mgr, 0u);
    uint32_t cleaned = 0;
    cspa_manager_wait_cleanup(mgr, 2000u, &cleaned);
    cspa_manager_release(mgr);
    return 0;
}
```

📖 **C11 Docs:** [C11 ABI Handbook](https://s0d3s.github.io/capture-system-or-process-audio/sdks/c/) • [C11 Functions & Structs Reference](https://s0d3s.github.io/capture-system-or-process-audio/reference/c/abi-and-functions/) • [FFI SDK README](crates/capture-ffi/README.md)

<p align="right"><sub><a href="#capture-system-or-process-audio">↑ Back to SDKs</a></sub></p>

---

## Drop-In Ecosystem `Adapters` & Node.js SDK *(Planned — Next Release)*

Today's initial release already connects cleanly to `NumPy` (`block.to_numpy(copy=False)`), `PyAudio` / `wave` (`memoryview(block)` + `OutputConverter`), `faster-whisper` (`16 kHz` mono `f32`), `asyncio`, Rust, and C/C++ pipelines.

For the **next release**, we are shipping **zero-rewrite drop-in compatibility `adapters`** alongside first-class **Node.js / TypeScript / Electron** bindings so existing audio apps can swap in system, per-application, or mixed mic+app capture without rewriting their consumer loops:

| Planned Feature (Next Release) | Target Ecosystem | What It Enables |
| --- | --- | --- |
| **`PyAudio` & `PyAudioWPatch` Drop-In Adapter** | `pyaudio`, `pyaudiowpatch` | Familiar `Stream.read(num_frames, exception_on_overflow=...)` & `stream_callback` (`paInt16`, `paFloat32`, `paInt24`, `paInt32`) backed by cross-platform system, app, or mixed capture with `silence_on_idle=True` deadlock protection. |
| **`python-sounddevice` Drop-In Adapter** | `sounddevice` (`InputStream`, `RawInputStream`, `rec`) | Exact 2D `(frames, channels)` NumPy pull & `CallbackFlags` push streams over any application, speaker, or mic+app mix. |
| **`SoundCard` & `SpeechRecognition` Adapters** | `soundcard` (`_Recorder`), `speech_recognition` (`AudioSource`) | Drop any running app (`Zoom`, `Teams`, browser) directly into `Recognizer.listen(...)` or `_Recorder.record(numframes=...)` without `PyAudio` C-extension build issues. |
| **Whisper / Live STT Stream Helpers** | `faster-whisper`, `openai-whisper`, VADs | Ready-to-iterate 1D `(samples,)` `16 kHz` `float32` / `int16` sliding-window streams. |
| **Node.js / TypeScript / Electron SDK (`napi-rs`)** | Electron `UtilityProcess`, Node `20+`, Bun, `@livekit/rtc-node`, `@discordjs/voice` | Prebuilt `Node-API` binaries (Electron V8 Memory Cage safe, zero `node-gyp`), `for await (const chunk of stream)`, `.toNodeReadable()`, zero-GC `readInto(TypedArray)`, and built-in `16k`/`24k`/`48k` presets. |

👉 **Want early access to a specific adapter or the Node.js SDK?** Star / Watch this repository or open an issue with your target stack!

---

## Why This Exists — Beyond `PyAudioWPatch`

Four years ago I created **[PyAudioWPatch](https://github.com/s0d3s/PyAudioWPatch)** to bring Windows WASAPI speaker loopback to Python's `PyAudio`. Today it is a staple of the Python audio ecosystem — serving **140,000+ monthly PyPI downloads** (`~1.6M+` / year) and powering **340+ open-source repositories** across AI meeting copilots, live Whisper/STT transcription bots, voice agents, and desktop recorders.

As `PyAudioWPatch` grew over 4+ years in production, users repeatedly requested features that were **architecturally impossible to implement** inside a legacy PortAudio C wrapper:

1. **Per-application isolation & native macOS / Linux capture:** PortAudio only models hardware endpoints — you could not isolate *just* `Zoom`, `Spotify`, or a game without recording Discord pings and desktop notifications too, while macOS and Linux users were still forced to manually install virtual audio drivers (`BlackHole`, `VB-Cable`).
2. **Production loopback traps that break naive recorders:** Silent WASAPI endpoints emit zero packets (causing blocking `stream.read()` loops in `PyAudioWPatch` to hang forever); unplugging or switching headphones breaks pinned streams; mixing `[Microphone + System/App Output]` drifts apart across independent hardware clocks; and Free-Threaded Python `3.14t`/`3.15t` (`-Xgil=0`) collides with PortAudio's global C state.

Rather than piling fragile workarounds onto `PyAudioWPatch`, I spent **nearly a year developing, testing, and polishing `capture-system-or-process-audio` from scratch in safe Rust** — turning years of `PyAudioWPatch` user requests and edge-case lessons into one unified, zero-steady-state-allocation engine across **Python, Rust, C++17, and C11**:

| Capability | `capture-system-or-process-audio` | `PyAudioWPatch` | `PyAudio` / `sounddevice` / `SoundCard` | `cpal` (Rust) | `VB-Cable` / `BlackHole` |
| --- | :---: | :---: | :---: | :---: | :---: |
| **System Speaker Loopback** | ✅ Win · Linux · macOS | ✅ Windows only | ⚠️ Partial (`SoundCard`) | ⚠️ Partial | ⚠️ Manual driver install |
| **Single App / Process Tree Isolation** | ✅ Win · Linux · macOS | ❌ | ❌ | ❌ | ❌ |
| **Microphone + App/System Live Mix** | ✅ Built-in (`CspaMixer` + drift sync) | ❌ Manual | ❌ Manual | ❌ Manual | ⚠️ Manual multi-output setup |
| **Silent-Speaker Hang Protection** | ✅ (`silence_on_idle` + timeouts) | ❌ Hangs `read()` | ❌ | ❌ | N/A |
| **Auto-Follow Default Device Switches** | ✅ (`Follow` mode) | ❌ | ❌ | ❌ | ❌ Breaks hardware volume keys |
| **Built-In Resampler & Format Converter** | ✅ (`16k`/`24k`/`44.1k`/`48k`, `f32`/`i16`/`i24`/`i32`) | ❌ | ❌ | ❌ | ❌ |
| **OS Permission Preflight & Prompt API** | ✅ Honest 5-state report | ❌ | ❌ | ❌ | N/A |
| **Free-Threaded Python (`3.14t`/`3.15t`)** | ✅ Native (`Py_MOD_GIL_NOT_USED`) | ❌ Disabled | ❌ | N/A | N/A |
| **First-Class SDKs** | **Python, Rust, C++17, C11** *(+ Node.js next)* | Python (Win) | Python | Rust | OS Driver |

- **What about single-OS process-tap wrappers (`proc-tap`, `catap`, `flexaudio`) or `Electron desktopCapturer`?** Single-platform and prototype wrappers lack unified Windows + PipeWire/PulseAudio + macOS Core Audio/ScreenCaptureKit coverage, cross-clock `[Microphone + App]` drift compensation, live target hot-swap, and free-threaded `3.14t`/`3.15t` zero-allocation `read_into` — while `Electron desktopCapturer` bundles heavyweight video capture pipelines without per-process isolation on Windows/Linux.
- 📊 **Upcoming Cross-Platform Statistical Benchmarks:** A standalone, reproducible cross-platform latency, tail-jitter (`p50`/`p95`), and zero-allocation benchmark suite comparing `capture-system-or-process-audio` head-to-head against existing audio capture libraries across **Windows 11 (`WASAPI`)**, **Linux (`PipeWire` & standalone `PulseAudio`)**, and **macOS (`Core Audio Taps` & `ScreenCaptureKit`)** will be published shortly.

> 📖 **Migrating from `PyAudioWPatch`?** See the [Side-by-Side `PyAudioWPatch` Migration Guide](https://s0d3s.github.io/capture-system-or-process-audio/sdks/python/#3-migrating-from-pyaudiowpatch) in the Python SDK Handbook.

---

## Platform & Backend Matrix

Capabilities differ across operating systems. **Core invariant:** When a requested scope or capability is unsupported on the current OS or OS version, `capture-system-or-process-audio` **fails explicitly with a typed error (`CapabilityError` / `PlanError`)** — it **never** silently falls back from single-process capture to full-system audio.

| Platform | Native Backends (`Backends`) | System & Device Output | Application / Process Capture | Microphones & Inputs | Key Runtime Notes |
| --- | --- | --- | --- | --- | --- |
| **Windows 10 / 11** (`x86_64`, `ARM64`) | `wasapi` | ✅ Build `19041+` (`default_output` `Follow`/`Pin`, `device(id)`) | ✅ Build `20348+` (`process` & `application`, `scope="tree"`) | ✅ Build `19041+` (`microphone` `Follow`/`Pin`, `input_device(id)`) | `scope="exact"` rejects explicitly (WASAPI process loopback is kernel tree-scoped). |
| **Linux** (`x86_64`, `aarch64`) | `pipewire` (`0.3.65+`), `pulseaudio` (`15.0+`) | ✅ Sink monitor & default sink tracking | ✅ PipeWire virtual-sink fan-in (`tree`/`exact`) & PulseAudio per-stream mix | ✅ Physical/virtual sources (`.monitor` sinks excluded) | `libpipewire-0.3.so.0` & `libpulse.so.0` load via runtime `dlopen` (**`0` compile-time `NEEDED` links**). |
| **macOS** (`Apple Silicon arm64`, `Intel x86_64`) | `coreaudio`, `screencapturekit` | ✅ `macOS 14.2+` Core Audio Process Taps | ✅ `macOS 14.2+` Core Audio Taps (`tree`/`exact`) & `macOS 13.0+` ScreenCaptureKit (`application`, `scope="exact"`) | ✅ `macOS 13.0+` Core Audio HAL input (`AVMediaTypeAudio` TCC) | Supports cross-backend `[ScreenCaptureKit + CoreAudio]` mixing. SCK `scope="tree"` rejects explicitly (use `coreaudio` for process trees). |

---

## Recording Permissions

Modern desktop operating systems gate audio capture behind user privacy consent — including macOS TCC (`Microphone`, Core Audio `SystemAudio` taps, and `ScreenCaptureKit` `ScreenRecording`) and Windows Microphone `ConsentStore` / Group Policy.

- **Interactive auto-request by default (`PermissionPolicy::AutoRequest`):** Opening a capture session automatically prompts for required OS permissions on interactive desktop sessions.
- **Non-prompting preflight (`check_permissions` & `PermissionPolicy::CheckOnly` / `request_permissions=False`):** Query an honest 5-state `PermissionReport` (`Granted`, `NotDetermined`, `Denied`, `Restricted`, `Unknown`) before opening any stream, without triggering modal OS dialogs in background services or CI.
- **macOS `.app` bundle `SIGABRT` protection:** Detects missing `NSMicrophoneUsageDescription` / `NSAudioCaptureUsageDescription` keys in bundled `Info.plist` files before calling Apple frameworks and returns a clean `PermissionDenied` (`PermissionError` in Python) instead of aborting the process.

---

## Interactive Studio Demo

Explore live target discovery, per-source backend/gain controls, real-time `CaptureRequest::inspect()` execution plans, OS permission states, and live spectroscopes right in your terminal with **`cspa-tui-demo`** (prebuilt binaries in [GitHub Releases](https://github.com/s0d3s/capture-system-or-process-audio/releases/latest), or built from source):

```bash
cargo run --release --manifest-path tools/cspa-tui-demo/Cargo.toml
```

- **Live Picker (`A`):** Browse **Applications** (parent-tree roll-up), **Processes**, **Output Devices**, and **Microphones** with live `256`-event discovery updates.
- **Record (`R`), Live Hot-Swap (`Space`) & Permissions (`K`):** Start/stop RAM takes (`R`), toggle staged sources or gain (`G`) **mid-recording** without losing your active take, and inspect/request OS privacy permissions interactively.
- **3-Mode Spectroscope (`V`) & WAV Export (`W`):** Switch between **2048-pt Hann FFT**, **Truecolor Waterfall**, and **Zero-Crossing Braille Oscilloscope** (visual-only monitoring prevents microphone speaker feedback!), play back RAM takes (`P`), and export `[Save WAV]` on demand.

See [tools/cspa-tui-demo/README.md](tools/cspa-tui-demo/README.md) and the [Interactive Studio Demo Manual](https://s0d3s.github.io/capture-system-or-process-audio/studio-demo/) for all panels, modals, and shortcuts.

---

## FAQ & Architecture Details

<details>
<summary><b>Do I or my end users need to install VB-Cable, BlackHole, or Soundflower?</b></summary>

**No.** Every backend uses built-in OS audio APIs (`WASAPI` on Windows, `PipeWire`/`PulseAudio` on Linux, `Core Audio Process Taps` and `ScreenCaptureKit` on macOS). Normal speaker and headphone playback continues uninterrupted.
</details>

<details>
<summary><b>Why do blocking loopback reads hang in other libraries when audio pauses — and how is that solved here?</b></summary>

On Windows WASAPI loopback and per-process taps, the OS emits zero packets while no audio is playing. By default, single-stream reads wait cleanly until audio resumes (or until `timeout_ms` elapses). When feeding consumers that expect continuous wall-clock frames (such as VADs, `PyAudio`-style loops, or WebRTC/Discord streams), pass `silence_on_idle=True` (Python) or `MixerConfig { silence_on_idle: true, .. }` (Rust/C++) to synthesize metadata-flagged silent frames (`synthesized_silence = true`) while idle.
</details>

<details>
<summary><b>Is the capture pipeline real-time safe and zero-allocation?</b></summary>

Yes. Architecture follows a strict four-layer invariant: *Backends capture. Core orders and buffers. Manager plans, monitors, mixes, and reopens sessions. Adapters adapt.* Native acquisition callbacks write into preallocated bounded slot pools (`StorageBudget`) without unbounded allocation or locks, `CspaMixer` aligns independent hardware clocks within `< 10 ms` best-effort relative skew, and warmed `read_into` loops perform **0 heap allocations** in steady state across Rust, Python (including `-Xgil=0` free-threading), C++17, and C11.
</details>

<details>
<summary><b>Can I preview which backend and mixing strategy will run before opening any OS audio stream?</b></summary>

Yes. Call `cspa.request(targets).inspect()` (Python), `request(targets).inspect()` (Rust), or `req.inspect()` (C++) for a side-effect-free `CapturePlanReport` showing per-target backend resolution, session combine strategy (`PassThrough`, `SharedNativeGraphMix`, or `CspaMixer`), active stream count, and estimated PCM memory footprint.
</details>

---

## Documentation & Guides

Full user manuals, architecture deep-dives, and 100%-symbol API references are hosted at **[s0d3s.github.io/capture-system-or-process-audio](https://s0d3s.github.io/capture-system-or-process-audio/)**:

| Section | Direct Links |
| :--- | :--- |
| **Getting Started** | [Overview](https://s0d3s.github.io/capture-system-or-process-audio/getting-started/overview/) • [Installation](https://s0d3s.github.io/capture-system-or-process-audio/getting-started/installation/) • [Choosing an API (`CaptureManager` vs `AudioStream`)](https://s0d3s.github.io/capture-system-or-process-audio/getting-started/choosing-an-api/) |
| **SDK Handbooks** | [Python (`3.11`–`3.15t`)](https://s0d3s.github.io/capture-system-or-process-audio/sdks/python/) • [Rust (`#![forbid(unsafe_code)]`)](https://s0d3s.github.io/capture-system-or-process-audio/sdks/rust/) • [C++17 RAII (`cspa.hpp`)](https://s0d3s.github.io/capture-system-or-process-audio/sdks/cpp/) • [C11 ABI (`cspa_experimental.h`)](https://s0d3s.github.io/capture-system-or-process-audio/sdks/c/) • [Ecosystem & Planned Adapters](https://s0d3s.github.io/capture-system-or-process-audio/adapters/overview/) |
| **Core Concepts** | [Targets & Scopes](https://s0d3s.github.io/capture-system-or-process-audio/concepts/targets-and-scopes/) • [Live Discovery](https://s0d3s.github.io/capture-system-or-process-audio/concepts/discovery/) • [Planning & Live Mixing (`CspaMixer`)](https://s0d3s.github.io/capture-system-or-process-audio/concepts/planning-and-mixing/) • [Format Conversion (`OutputConverter`)](https://s0d3s.github.io/capture-system-or-process-audio/concepts/format-conversion/) • [Real-Time & Lifecycle](https://s0d3s.github.io/capture-system-or-process-audio/concepts/realtime-and-lifecycle/) |
| **Platforms & Privacy** | [Platform & Backend Matrix](https://s0d3s.github.io/capture-system-or-process-audio/concepts/platforms/) • [OS Recording Permissions (`TCC` / `ConsentStore`)](https://s0d3s.github.io/capture-system-or-process-audio/concepts/permissions/) • [Troubleshooting & FAQ](https://s0d3s.github.io/capture-system-or-process-audio/concepts/troubleshooting-faq/) |
| **API References & Demo** | [Multi-SDK API Reference Hub](https://s0d3s.github.io/capture-system-or-process-audio/reference/) • [Hosted Rustdoc (`/api/rust/`)](https://s0d3s.github.io/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/) • [Interactive `cspa-tui-demo` Manual](https://s0d3s.github.io/capture-system-or-process-audio/studio-demo/) |

---

## License

Dual-licensed under either of:

- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- **MIT License** ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option. Prebuilt Linux binaries dynamically load system `libpipewire-0.3.so.0` and `libpulse.so.0` at runtime without bundling or statically linking LGPL libraries; see each package's `THIRD_PARTY_NOTICES.md` for dependency attributions.
