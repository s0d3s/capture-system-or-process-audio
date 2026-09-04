import json

MATURIN_VERSION = "1.16.0"
DEFAULT_RETENTION_DAYS = 14
ROOT_LICENSE_FILES = ("LICENSE-MIT", "LICENSE-APACHE")
PYTHON_SPECS = ("cpython-3.11", "cpython-3.14", "cpython-3.14+freethreaded", "cpython-3.15", "cpython-3.15+freethreaded")
HOST_PYTHON = PYTHON_SPECS[3]
PUBLISHED_CRATES = (
    ("capture-core", "capture-system-or-process-audio-core"),
    ("capture-windows", "capture-system-or-process-audio-windows"),
    ("capture-linux", "capture-system-or-process-audio-linux"),
    ("capture-macos", "capture-system-or-process-audio-macos"),
    ("capture", "capture-system-or-process-audio"),
)
TARGETS = (
    {"label": "linux-x86_64", "runner": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu", "platform": "linux", "backend_crate": "crates/capture-linux/Cargo.toml", "cmake_arch": ""},
    {"label": "linux-aarch64", "runner": "ubuntu-24.04-arm", "target": "aarch64-unknown-linux-gnu", "platform": "linux", "backend_crate": "crates/capture-linux/Cargo.toml", "cmake_arch": ""},
    {"label": "macos-aarch64", "runner": "macos-15", "target": "aarch64-apple-darwin", "platform": "macos", "backend_crate": "crates/capture-macos/Cargo.toml", "cmake_arch": ""},
    {"label": "windows-x86_64", "runner": "windows-2025", "target": "x86_64-pc-windows-msvc", "platform": "windows", "backend_crate": "crates/capture-windows/Cargo.toml", "cmake_arch": "x64"},
    {"label": "windows-aarch64", "runner": "windows-11-arm", "target": "aarch64-pc-windows-msvc", "platform": "windows", "backend_crate": "crates/capture-windows/Cargo.toml", "cmake_arch": "ARM64"},
)
WHEEL_TAGS = ("cp311-abi3", "cp314-cp314t", "cp315-abi3.abi3t")


def matrix_json():
    return json.dumps({"include": TARGETS}, separators=(",", ":"))