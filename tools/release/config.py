import json

MATURIN_VERSION = "1.16.0"
DEFAULT_RETENTION_DAYS = 14
ROOT_LICENSE_FILES = ("LICENSE-MIT", "LICENSE-APACHE")
PYTHON_WHEELS = (
    {
        "tag": "cp311-abi3",
        "build_python": "cpython-3.11",
        "features": ("abi3-py311",),
        "smoke_pythons": (("cpython-3.11", False), ("cpython-3.14", False)),
    },
    {
        "tag": "cp314-cp314t",
        "build_python": "cpython-3.14+freethreaded",
        "features": (),
        "smoke_pythons": (("cpython-3.14+freethreaded", True),),
    },
    {
        "tag": "cp315-abi3.abi3t",
        "build_python": "cpython-3.15+freethreaded",
        "features": ("abi3t-py315",),
        "smoke_pythons": (("cpython-3.15", False), ("cpython-3.15+freethreaded", True)),
    },
)
PYTHON_SPECS = tuple(dict.fromkeys(
    [spec for wheel in PYTHON_WHEELS for spec, _ in wheel["smoke_pythons"]]
    + [wheel["build_python"] for wheel in PYTHON_WHEELS]
))
HOST_PYTHON = next(
    spec for wheel in reversed(PYTHON_WHEELS)
    for spec, free_threaded in reversed(wheel["smoke_pythons"])
    if not free_threaded
)
PUBLISHED_CRATES = (
    ("capture-core", "capture-system-or-process-audio-core"),
    ("capture-windows", "capture-system-or-process-audio-windows"),
    ("capture-linux", "capture-system-or-process-audio-linux"),
    ("capture-macos", "capture-system-or-process-audio-macos"),
    ("capture", "capture-system-or-process-audio"),
)
RELEASE_CARGO_MANIFESTS = tuple(
    f"crates/{folder}/Cargo.toml" for folder, _ in PUBLISHED_CRATES
) + (
    "crates/capture-ffi/Cargo.toml",
    "crates/capture-python/Cargo.toml",
    "tools/cspa-tui-demo/Cargo.toml",
)
TARGETS = (
    {"label": "linux-x86_64", "runner": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu", "platform": "linux", "backend_crate": "crates/capture-linux/Cargo.toml", "cmake_arch": ""},
    {"label": "linux-aarch64", "runner": "ubuntu-24.04-arm", "target": "aarch64-unknown-linux-gnu", "platform": "linux", "backend_crate": "crates/capture-linux/Cargo.toml", "cmake_arch": ""},
    {"label": "macos-aarch64", "runner": "macos-15", "target": "aarch64-apple-darwin", "platform": "macos", "backend_crate": "crates/capture-macos/Cargo.toml", "cmake_arch": "arm64"},
    {"label": "macos-x86_64", "runner": "macos-15-intel", "target": "x86_64-apple-darwin", "platform": "macos", "backend_crate": "crates/capture-macos/Cargo.toml", "cmake_arch": "x86_64"},
    {"label": "windows-x86_64", "runner": "windows-2025", "target": "x86_64-pc-windows-msvc", "platform": "windows", "backend_crate": "crates/capture-windows/Cargo.toml", "cmake_arch": "x64"},
    {"label": "windows-aarch64", "runner": "windows-11-arm", "target": "aarch64-pc-windows-msvc", "platform": "windows", "backend_crate": "crates/capture-windows/Cargo.toml", "cmake_arch": "ARM64"},
)
WHEEL_TAGS = tuple(wheel["tag"] for wheel in PYTHON_WHEELS)


def matrix_json():
    return json.dumps({"include": TARGETS}, separators=(",", ":"))