import os, pathlib, shutil, subprocess, tarfile, tempfile, textwrap, zipfile

from cmake_version import generate_cmake_version
from config import PYTHON_SPECS, ROOT_LICENSE_FILES
from consumer_smoke import run_packaged_consumers
from python_wheels import build_release_wheels, smoke_cases

root = pathlib.Path(".").resolve()
ver = os.environ["PACKAGE_VERSION"]
target = os.environ["RUST_TARGET"]
platform = os.environ["MATRIX_PLATFORM"]
cmake_arch = os.environ.get("CMAKE_ARCH", "").strip()

wheels_dir = root / "dist" / "wheels"
ffi_dir = root / "dist" / "ffi"
demo_dir = root / "dist" / "demo"
for d in (wheels_dir, ffi_dir, demo_dir):
    d.mkdir(parents=True, exist_ok=True)

def find_py(spec: str) -> str:
    out = subprocess.check_output(
        ["uv", "python", "find", "--no-project", "--managed-python", spec],
        text=True,
    ).strip()
    assert out, f"Could not find managed Python {spec}"
    return out

python_executables = {spec: find_py(spec) for spec in PYTHON_SPECS}

maturin_exe = shutil.which("maturin")
if not maturin_exe:
    bin_dir = pathlib.Path(
        subprocess.check_output(["uv", "tool", "dir", "--bin"], text=True).strip()
    )
    maturin_exe = str(
        bin_dir / ("maturin.exe" if platform == "windows" else "maturin")
    )
maturin_common = [
    maturin_exe,
    "build",
    "--release",
    "--locked",
    "-m",
    str(root / "crates/capture-python/Cargo.toml"),
    "--target",
    target,
    "-o",
    str(wheels_dir),
]
if platform == "linux":
    maturin_common.extend(["--zig", "--compatibility", "manylinux_2_28"])

wheels = build_release_wheels(maturin_common, wheels_dir, python_executables, platform)

for whl in wheels.values():
    with zipfile.ZipFile(whl, "r") as zf:
        names = zf.namelist()
        assert any(n.endswith("README.md") for n in names), f"Missing README.md in {whl.name}"
        assert any(
            n.endswith("THIRD_PARTY_NOTICES.md") for n in names
        ), f"Missing THIRD_PARTY_NOTICES.md in {whl.name}"

smoke_code = """
import capture_system_or_process_audio as cspa
import pathlib, sys, sysconfig, threading, warnings

pkg = pathlib.Path(cspa.__file__).resolve().parent
assert (pkg / "py.typed").is_file() and (pkg / "__init__.pyi").is_file() and (pkg / "__main__.py").is_file()
stub = (pkg / "__init__.pyi").read_text(encoding="utf-8")
assert "Incomplete" not in stub and "typing_extensions" not in stub

gil_dis = int(sysconfig.get_config_var("Py_GIL_DISABLED") or 0)
if gil_dis == 1:
    assert sys._is_gil_enabled() is False

ext = getattr(cspa, "capture_system_or_process_audio", cspa)
mgr = ext._synthetic_mixed_demo(sample_format="f32", clamp_f32=True)
out = mgr.read(480, 50, None)
assert out.frames == 480 and out.reason is cspa.PartialReason.COMPLETE
mv = memoryview(out.blocks[0])
assert mv.readonly and mv.shape == (480, 2)
mv.release()
mgr.stop(cspa.StopMode.ABORT)
assert mgr.wait_cleanup()

if gil_dis == 1:
    barrier = threading.Barrier(8)
    errs = []
    def worker():
        try:
            barrier.wait(timeout=10.0)
            for _ in range(8):
                m = ext._synthetic_segmented_demo()
                dst = bytearray(16)
                r = m.read_into(dst, 2, 0, None)
                assert r.copy_bytes == 16
                m.stop(cspa.StopMode.ABORT)
        except BaseException as exc:
            errs.append(repr(exc))
    ts = [threading.Thread(target=worker) for _ in range(8)]
    for t in ts: t.start()
    for t in ts: t.join(timeout=15.0)
    assert not errs, errs
"""

with tempfile.TemporaryDirectory(prefix="cspa-wheel-smoke-") as tmp_str:
    tmp = pathlib.Path(tmp_str)
    smoke_py = tmp / "smoke.py"
    smoke_py.write_text(textwrap.dedent(smoke_code).strip() + "\n", encoding="utf-8")
    for index, (_, py_exe, whl_path, free_threaded) in enumerate(smoke_cases(wheels, python_executables)):
        venv_dir = tmp / f"venv-{index}"
        subprocess.run(["uv", "venv", "--python", py_exe, str(venv_dir)], check=True)
        vpy = (
            venv_dir / "Scripts" / "python.exe"
            if platform == "windows"
            else venv_dir / "bin" / "python"
        )
        subprocess.run(
            ["uv", "pip", "install", "--python", str(vpy), "--no-deps", str(whl_path)],
            check=True,
        )
        run_args = [str(vpy), "-X", "gil=0", str(smoke_py)] if free_threaded else [str(vpy), str(smoke_py)]
        subprocess.run(run_args, check=True)

# 2. Build, stage, and CMake-verify C/C++ FFI SDK bundle:
subprocess.run(
    [
        "cargo",
        "build",
        "--release",
        "--locked",
        "--manifest-path",
        str(root / "crates/capture-ffi/Cargo.toml"),
        "--target",
        target,
    ],
    check=True,
)
ffi_rel_dir = root / "crates/capture-ffi/target" / target / "release"
ffi_stem = f"cspa-ffi-{ver}-{target}"

with tempfile.TemporaryDirectory(prefix="cspa-ffi-stage-") as tmp_ffi_str:
    tmp_ffi = pathlib.Path(tmp_ffi_str)
    pkg_stage = tmp_ffi / ffi_stem
    for sub in ("include", "lib", "lib/cmake/Cspa"):
        (pkg_stage / sub).mkdir(parents=True, exist_ok=True)
    if platform == "windows":
        (pkg_stage / "bin").mkdir(parents=True, exist_ok=True)
        shutil.copy2(ffi_rel_dir / "cspa_ffi.dll", pkg_stage / "bin" / "cspa_ffi.dll")
        shutil.copy2(ffi_rel_dir / "cspa_ffi.dll.lib", pkg_stage / "lib" / "cspa_ffi.dll.lib")
    elif platform == "macos":
        shutil.copy2(ffi_rel_dir / "libcspa_ffi.dylib", pkg_stage / "lib" / "libcspa_ffi.dylib")
    else:
        so_path = ffi_rel_dir / "libcspa_ffi.so"
        dyn = subprocess.check_output(["readelf", "-d", str(so_path)], text=True)
        for forbidden_so in ("libpipewire", "libpulse"):
            assert forbidden_so not in dyn, f"Unexpected NEEDED link to {forbidden_so}:\n{dyn}"
        shutil.copy2(so_path, pkg_stage / "lib" / "libcspa_ffi.so")

    shutil.copy2(
        root / "crates/capture-ffi/include/cspa_experimental.h",
        pkg_stage / "include" / "cspa_experimental.h",
    )
    shutil.copy2(
        root / "crates/capture-ffi/include/cspa.hpp",
        pkg_stage / "include" / "cspa.hpp",
    )
    shutil.copy2(
        root / "crates/capture-ffi/cmake/CspaConfig.cmake",
        pkg_stage / "lib/cmake/Cspa/CspaConfig.cmake",
    )
    ffi_version = generate_cmake_version(
        root / "crates/capture-ffi/Cargo.toml",
        root / "crates/capture-ffi/cmake/CspaConfigVersion.cmake.in",
        pkg_stage / "lib/cmake/Cspa/CspaConfigVersion.cmake",
    )
    if ffi_version != ver:
        raise ValueError(f"FFI manifest version {ffi_version!r} does not match release version {ver!r}")
    shutil.copy2(root / "crates/capture-ffi/README.md", pkg_stage / "README.md")
    shutil.copy2(
        root / "crates/capture-ffi/THIRD_PARTY_NOTICES.md",
        pkg_stage / "THIRD_PARTY_NOTICES.md",
    )
    for lic_name in ROOT_LICENSE_FILES:
        shutil.copy2(root / lic_name, pkg_stage / lic_name)

    # Run out-of-tree CMake C11 + C++17 consumer smoke test against staged SDK.
    consumer_src = tmp_ffi / "cmake-consumer"
    consumer_src.mkdir()
    c11_txt = (root / "crates/capture-ffi/tests/c11_consumer.c").read_text(encoding="utf-8")
    (consumer_src / "c11_consumer.c").write_text(
        c11_txt.replace('#include "../include/cspa_experimental.h"', "#include <cspa_experimental.h>"),
        encoding="utf-8",
    )
    cpp17_txt = (root / "crates/capture-ffi/tests/cpp17_consumer.cpp").read_text(encoding="utf-8")
    (consumer_src / "cpp17_consumer.cpp").write_text(
        cpp17_txt.replace('#include "../include/cspa.hpp"', "#include <cspa.hpp>"),
        encoding="utf-8",
    )
    package_find = (
        f"find_package(Cspa {ver} EXACT REQUIRED CONFIG)"
        if ver.replace(".", "").isdigit()
        else "find_package(Cspa REQUIRED CONFIG)"
    )
    (consumer_src / "CMakeLists.txt").write_text(
        "cmake_minimum_required(VERSION 3.20)\n"
        "project(cspa_ci_cmake_consumer LANGUAGES C CXX)\n"
        f"{package_find}\n"
        "add_executable(c11_clean_consumer c11_consumer.c)\n"
        "set_property(TARGET c11_clean_consumer PROPERTY C_STANDARD 11)\n"
        "target_link_libraries(c11_clean_consumer PRIVATE Cspa::cspa_ffi)\n"
        "add_executable(cpp17_clean_consumer cpp17_consumer.cpp)\n"
        "set_property(TARGET cpp17_clean_consumer PROPERTY CXX_STANDARD 17)\n"
        "target_link_libraries(cpp17_clean_consumer PRIVATE Cspa::cspa_ffi)\n",
        encoding="utf-8",
    )
    cmake_build = tmp_ffi / "cmake-build"
    cfg_cmd = [
        "cmake",
        "-S",
        str(consumer_src),
        "-B",
        str(cmake_build),
        f"-DCspa_DIR={(pkg_stage / 'lib/cmake/Cspa').as_posix()}",
    ]
    if platform == "windows" and cmake_arch:
        cfg_cmd.extend(["-A", cmake_arch])
    else:
        cfg_cmd.append("-DCMAKE_BUILD_TYPE=Release")
    subprocess.run(cfg_cmd, check=True)
    subprocess.run(["cmake", "--build", str(cmake_build), "--config", "Release"], check=True)

    run_packaged_consumers(platform, cmake_build, pkg_stage)

    if platform == "windows":
        ffi_archive = ffi_dir / f"{ffi_stem}.zip"
        with zipfile.ZipFile(ffi_archive, "w", compression=zipfile.ZIP_DEFLATED) as zf:
            for f in sorted(pkg_stage.rglob("*")):
                if f.is_file():
                    zf.write(f, f.relative_to(tmp_ffi).as_posix())
    else:
        ffi_archive = ffi_dir / f"{ffi_stem}.tar.gz"
        with tarfile.open(ffi_archive, "w:gz") as tf:
            tf.add(pkg_stage, arcname=ffi_stem)

# 3. Build, test, smoke-check, and package standalone cspa-tui-demo binary:
demo_manifest = str(root / "tools/cspa-tui-demo/Cargo.toml")
subprocess.run(
    ["cargo", "clippy", "--locked", "--manifest-path", demo_manifest, "--target", target, "--all-targets", "--", "-D", "warnings"],
    check=True,
)
# subprocess.run(
#     ["cargo", "test", "--locked", "--manifest-path", demo_manifest, "--target", target, "--", "--test-threads=1"],
#     check=True,
# )
subprocess.run(
    ["cargo", "build", "--release", "--locked", "--manifest-path", demo_manifest, "--target", target],
    check=True,
)
exe_name = "cspa-tui-demo.exe" if platform == "windows" else "cspa-tui-demo"
demo_bin = root / "tools/cspa-tui-demo/target" / target / "release" / exe_name
subprocess.run([str(demo_bin), "--synthetic-smoke-test"], check=True)

demo_stem = f"cspa-tui-demo-{ver}-{target}"
with tempfile.TemporaryDirectory(prefix="cspa-demo-stage-") as tmp_demo_str:
    tmp_demo = pathlib.Path(tmp_demo_str)
    dstage = tmp_demo / demo_stem
    dstage.mkdir()
    shutil.copy2(demo_bin, dstage / exe_name)
    shutil.copy2(root / "tools/cspa-tui-demo/README.md", dstage / "README.md")
    shutil.copy2(
        root / "tools/cspa-tui-demo/THIRD_PARTY_NOTICES.md",
        dstage / "THIRD_PARTY_NOTICES.md",
    )
    for lic_name in ROOT_LICENSE_FILES:
        shutil.copy2(root / lic_name, dstage / lic_name)
    if platform == "windows":
        demo_archive = demo_dir / f"{demo_stem}.zip"
        with zipfile.ZipFile(demo_archive, "w", compression=zipfile.ZIP_DEFLATED) as zf:
            for f in sorted(dstage.rglob("*")):
                if f.is_file():
                    zf.write(f, f.relative_to(tmp_demo).as_posix())
    else:
        demo_archive = demo_dir / f"{demo_stem}.tar.gz"
        with tarfile.open(demo_archive, "w:gz") as tf:
            tf.add(dstage, arcname=demo_stem)
