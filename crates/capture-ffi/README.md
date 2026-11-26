# C11 ABI & C++17 RAII SDK (`cspa_ffi`)

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

First-class **C11 ABI** (`include/cspa_experimental.h`, `CSPA_EXPERIMENTAL_ABI_MAJOR = 0`, `CSPA_EXPERIMENTAL_SCHEMA_REVISION = 1`) and **header-only C++17 RAII wrapper** (`include/cspa.hpp`, namespace `cspa`) built directly on top of the unified cross-platform Rust facade (`capture-system-or-process-audio`) across **Windows (`x64`, `ARM64`)**, **Linux (`x86_64`, `aarch64`)**, and **macOS (`Apple Silicon arm64`, `Intel x86_64`)**.

- **Zero Per-OS `#ifdef` Glue:** Identical C11 and C++17 API across WASAPI, PipeWire/PulseAudio, and Core Audio/ScreenCaptureKit.
- **Panic-Safe & Version-Checked ABI:** Every exported C symbol enforces `catch_unwind` at the FFI boundary and validates `struct_size` + `schema_revision` (`cspa_abi_check`).
- **Exceptions Optional in C++17:** Supports both structured `cspa::Result<T>` under `-fno-exceptions` / `/EHs-c- /D_HAS_EXCEPTIONS=0` and ergonomic `*_or_throw()` helpers when exceptions are enabled (`/EHsc`, `-fexceptions`).
- **Real-Time Safe Reads:** Zero-extra-copy `cspa_block_record` / `cspa::PcmBlock` views surviving `stop()`, plus **0 steady-state heap allocations** on warmed `read_into` loops.

---

## Prebuilt SDK Layout

Download `cspa-ffi-<version>-<target>.zip` (Windows) or `.tar.gz` (Linux / macOS) from [GitHub Releases](https://github.com/s0d3s/capture-system-or-process-audio/releases/latest):

```text
cspa-ffi-<version>-<target>/
├── include/
│   ├── cspa_experimental.h          # C11 size & schema-revision-checked ABI
│   └── cspa.hpp                     # Header-only C++17 RAII wrapper
├── lib/
│   ├── cspa_ffi.dll.lib             # Windows MSVC import library (or libcspa_ffi.so / .dylib)
│   └── cmake/Cspa/
│       ├── CspaConfig.cmake         # Defines imported target Cspa::cspa_ffi
│       └── CspaConfigVersion.cmake
├── bin/
│   └── cspa_ffi.dll                 # Windows runtime DLL
├── README.md
├── LICENSE-MIT
├── LICENSE-APACHE
└── THIRD_PARTY_NOTICES.md
```

---

## CMake Integration (`Cspa::cspa_ffi`)

```cmake
cmake_minimum_required(VERSION 3.20)
project(audio_capture_app LANGUAGES C CXX)

find_package(Cspa REQUIRED CONFIG)

add_executable(cpp_example main.cpp)
target_compile_features(cpp_example PRIVATE cxx_std_17)
target_link_libraries(cpp_example PRIVATE Cspa::cspa_ffi)

# On Windows, copy cspa_ffi.dll next to the built executable automatically:
if(WIN32)
    add_custom_command(TARGET cpp_example POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
            $<TARGET_FILE:Cspa::cspa_ffi>
            $<TARGET_FILE_DIR:cpp_example>
    )
endif()
```

Configure and build pointing `-DCspa_DIR` at the extracted `lib/cmake/Cspa` directory:

For numeric package versions, CMake accepts an exact match or an older requested
version with the same major and minor number. Version ranges are not supported.
For prerelease or build-metadata versions, use unversioned `find_package(Cspa REQUIRED CONFIG)`.
Package version matching does not guarantee stability of the experimental C ABI.

```bash
# Windows (x64 or ARM64):
cmake -S . -B build -A x64 -DCspa_DIR="C:/path/to/cspa-ffi/lib/cmake/Cspa"
cmake --build build --config Release

# Linux / macOS:
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DCspa_DIR="/path/to/cspa-ffi/lib/cmake/Cspa"
cmake --build build
```

---

## C++17 Quick Start (`include/cspa.hpp`)

### 1. Exception-Free `cspa::Result<T>` (`-fno-exceptions` & `/EHsc` Compatible)

```cpp
#include <cspa.hpp>
#include <cstdio>
#include <vector>

int main() {
    auto speaker_res = cspa::Target::default_output(cspa::DefaultDeviceMode::Follow);
    auto mic_res     = cspa::Target::microphone(cspa::DefaultDeviceMode::Follow);
    if (!speaker_res.is_ok() || !mic_res.is_ok()) return 1;

    cspa::MixerConfig mixer;
    mixer.set_silence_on_idle(true).set_clamp_f32(true);

    cspa::CaptureRequest req({speaker_res.take_value(), mic_res.take_value()});
    req.set_mixer_config(mixer)
       .set_output_converter(cspa::OutputConverter(16000, 1, cspa::SampleFormat::I16));

    // Optional side-effect-free execution plan & OS permission check before opening streams:
    auto plan_res = req.inspect();
    auto perm_res = req.check_permissions();
    if (!plan_res.is_ok() || !perm_res.is_ok()) return 1;

    auto stream_res = cspa::ManagedStream::open(req);
    if (!stream_res.is_ok()) return 1;
    cspa::ManagedStream stream = stream_res.take_value();

    // Zero-allocation steady-state read_into (100 ms = 1600 frames @ 16 kHz mono i16):
    std::vector<uint8_t> pcm(1600 * sizeof(int16_t));
    auto rd = stream.read_into(pcm.data(), pcm.size(), 1600, 250);
    if (rd.is_ok()) {
        std::printf("Captured %llu mixed frames (%llu bytes)\n",
                    static_cast<unsigned long long>(rd.value().frames),
                    static_cast<unsigned long long>(rd.value().copy_bytes));
    }

    stream.stop(false);
    stream.wait_cleanup(2000);
    return 0;
}
```

### 2. List, Filter & Capture `cspa::Target`s Deterministically (`cspa::Discovery`)

Enumerating `cspa::Discovery::query` lets you inspect every available application, process, output device, and microphone — filtering deterministically on `kind()`, `raw().active_audio == CSPA_ACTIVE_AUDIO_ACTIVE`, `pid()`, `app_id()`, or `raw().executable_path` rather than relying on ambiguous display-name matching:

```cpp
#include <cspa.hpp>
#include <cstdio>
#include <vector>

int main() {
    auto disc_res = cspa::Discovery::create();
    if (!disc_res.is_ok()) return 1;
    cspa::Discovery disc = disc_res.take_value();

    cspa::DiscoveryQuery query;
    auto catalog_res = disc.query(query);
    if (!catalog_res.is_ok()) return 1;

    std::vector<cspa::Target> selected;
    for (const cspa::Target &t : catalog_res.value()) {
        std::printf("Target kind=%u name='%s' pid=%u active=%u\n",
                    static_cast<unsigned>(t.kind()),
                    t.name(),
                    t.pid(),
                    t.raw().active_audio);
        if (t.kind() == cspa::TargetKind::Application &&
            t.raw().active_audio == CSPA_ACTIVE_AUDIO_ACTIVE &&
            selected.size() < 2) {
            auto scaled = t.with_gain(0.85f);
            if (scaled.is_ok()) selected.push_back(scaled.take_value());
        }
    }
    if (selected.empty()) {
        selected.push_back(cspa::Target::default_output(cspa::DefaultDeviceMode::Follow).take_value());
    }

    cspa::MixerConfig mixer;
    mixer.set_silence_on_idle(true);
    cspa::CaptureRequest req(std::move(selected));
    req.set_mixer_config(mixer);

    auto stream_res = cspa::ManagedStream::open(req);
    if (!stream_res.is_ok()) return 1;
    cspa::ManagedStream stream = stream_res.take_value();

    std::vector<uint8_t> pcm(480 * 8);
    (void)stream.read_into(pcm.data(), pcm.size(), 480, 200);
    stream.stop(false);
    stream.wait_cleanup(2000);
    return 0;
}
```

### 3. Exception-Enabled `*_or_throw()` Convenience Style

```cpp
#include <cspa.hpp>
#include <cstdio>

int main() {
#if CSPA_HAS_EXCEPTIONS
    try {
        cspa::Target out = cspa::Target::default_output_or_throw(cspa::DefaultDeviceMode::Follow);
        cspa::CaptureRequest req({out});
        cspa::ManagedStream stream = cspa::ManagedStream::open_or_throw(req);

        auto outcome = cspa::unwrap_or_throw(stream.read(480, 200));
        if (outcome.block.valid()) {
            std::printf("Zero-copy block: %llu frames (%llu bytes)\n",
                        static_cast<unsigned long long>(outcome.block.frames()),
                        static_cast<unsigned long long>(outcome.block.byte_len()));
        }
        stream.stop(false);
        stream.wait_cleanup(2000);
    } catch (const cspa::Error &err) {
        std::fprintf(stderr, "Capture error (status=%u): %s\n",
                     static_cast<unsigned>(err.status()), err.what());
        return 1;
    }
#endif
    return 0;
}
```

---

## C11 Quick Start (`include/cspa_experimental.h`)

```c
#include <cspa_experimental.h>
#include <stdint.h>
#include <stdio.h>

int main(void) {
    if (cspa_abi_check(CSPA_EXPERIMENTAL_ABI_MAJOR, CSPA_EXPERIMENTAL_SCHEMA_REVISION) != CSPA_STATUS_OK) {
        return 1;
    }

    cspa_permission_report_record perms;
    if (cspa_permission_report_init(&perms) != CSPA_STATUS_OK ||
        cspa_permissions_check(NULL, 0u, NULL, &perms) != CSPA_STATUS_OK) {
        return 1;
    }

    cspa_capture_target_record target = {0};
    target.struct_size = (uint32_t)sizeof(target);
    target.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    if (cspa_target_default_output(CSPA_DEFAULT_DEVICE_FOLLOW, 1.0f, &target) != CSPA_STATUS_OK) {
        return 1;
    }

    cspa_capture_request_record req;
    if (cspa_capture_request_init(&req, &target, 1u) != CSPA_STATUS_OK) {
        return 1;
    }

    cspa_manager *manager = NULL;
    if (cspa_manager_open(&req, &manager) != CSPA_STATUS_OK) {
        return 1;
    }

    uint8_t buffer[480 * 8];
    cspa_managed_read_outcome_record out = {0};
    out.struct_size = (uint32_t)sizeof(out);
    out.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;

    if (cspa_manager_read_into(manager, buffer, sizeof(buffer), 480u, 200u, NULL, &out) == CSPA_STATUS_OK) {
        printf("Read %llu frames (%llu bytes)\n",
               (unsigned long long)out.actual_frames,
               (unsigned long long)out.copy_bytes);
    }

    /* Note: When using cspa_manager_read() instead of read_into(), call
       cspa_block_release(&out.block) once finished with the zero-copy view. */
    cspa_manager_stop(manager, 0u);
    uint32_t cleaned = 0;
    cspa_manager_wait_cleanup(manager, 2000u, &cleaned);
    cspa_manager_release(manager);
    return 0;
}
```

### List & Filter Available `cspa_capture_target_record`s (`cspa_discover_query`)

Use the two-call `cspa_discover_query` pattern (`NULL, 0, &total` or a bounded buffer) to inspect and deterministically select active applications, processes, or specific endpoints before opening `cspa_manager`:

```c
#include <cspa_experimental.h>
#include <stdlib.h>
#include <stdio.h>

int main(void) {
    if (cspa_abi_check(CSPA_EXPERIMENTAL_ABI_MAJOR, CSPA_EXPERIMENTAL_SCHEMA_REVISION) != CSPA_STATUS_OK) {
        return 1;
    }

    cspa_discovery_query_record query = {0};
    if (cspa_discovery_query_init(&query) != CSPA_STATUS_OK) return 1;

    size_t count = 0;
    if (cspa_discover_query(&query, NULL, 0u, &count) != CSPA_STATUS_OK) return 1;

    cspa_capture_target_record *items = NULL;
    size_t cap = count + 8u;
    items = (cspa_capture_target_record *)calloc(cap, sizeof(*items));
    if (items == NULL) return 1;

    size_t filled = 0;
    if (cspa_discover_query(&query, items, cap, &filled) == CSPA_STATUS_OK) {
        size_t n = filled < cap ? filled : cap;
        for (size_t i = 0; i < n; ++i) {
            printf("kind=%u name='%s' pid=%u active=%u\n",
                   items[i].kind, items[i].name, items[i].pid, items[i].active_audio);
            if (items[i].kind == CSPA_TARGET_KIND_APPLICATION &&
                items[i].active_audio == CSPA_ACTIVE_AUDIO_ACTIVE) {
                cspa_capture_target_record scaled = {0};
                if (cspa_target_with_gain(&items[i], 0.85f, &scaled) == CSPA_STATUS_OK) {
                    cspa_capture_request_record req = {0};
                    cspa_capture_request_init(&req, &scaled, 1u);
                    cspa_manager *mgr = NULL;
                    if (cspa_manager_open(&req, &mgr) == CSPA_STATUS_OK) {
                        cspa_manager_stop(mgr, 1u);
                        cspa_manager_release(mgr);
                    }
                }
                break;
            }
        }
    }

    free(items);
    return 0;
}
```

---

## Runtime & Platform Notes

- **Windows (`x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`):** Ensure `bin/cspa_ffi.dll` is placed in the executable directory or on `PATH` (handled automatically by the `POST_BUILD` CMake rule above).
- **Linux (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`):** `libcspa_ffi.so` has **`0` compile-time `NEEDED` links** to `libpipewire` or `libpulse`; system `libpipewire-0.3.so.0` (`>= 0.3.65`) and `libpulse.so.0` (`>= 15.0`) are loaded dynamically via `dlopen` at runtime.
- **macOS (`aarch64-apple-darwin`, `x86_64-apple-darwin`):** When embedding inside a `.app` bundle, include `NSMicrophoneUsageDescription` (for microphone inputs) and `NSAudioCaptureUsageDescription` (for Core Audio taps on macOS `14.2+`) in your app's `Info.plist`. Missing usage keys are safely detected and reported as `CSPA_STATUS_PERMISSION_DENIED` without triggering TCC `SIGABRT`.

---

## Links & License

- **C++17 & C11 Handbooks + Full API Reference:** <https://s0d3s.github.io/capture-system-or-process-audio/sdks/cpp/> · <https://s0d3s.github.io/capture-system-or-process-audio/sdks/c/>
- **Repository & Other SDKs (Python, Rust, Live TUI Demo):** <https://github.com/s0d3s/capture-system-or-process-audio>
- **License:** Dual-licensed under [MIT](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/s0d3s/capture-system-or-process-audio/blob/main/LICENSE-APACHE), at your option.
