# C11 ABI & C++17 RAII SDK (`cspa_ffi`)

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

First-class **C11 ABI** (`include/cspa_experimental.h`, `CSPA_EXPERIMENTAL_ABI_MAJOR = 0`, `CSPA_EXPERIMENTAL_SCHEMA_REVISION = 1`) and **header-only C++17 RAII wrapper** (`include/cspa.hpp`, namespace `cspa`) built directly on top of the unified cross-platform Rust facade (`capture-system-or-process-audio`) across **Windows (`x64`, `ARM64`)**, **Linux (`x86_64`, `aarch64`)**, and **Apple Silicon macOS (`arm64`)**.

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

