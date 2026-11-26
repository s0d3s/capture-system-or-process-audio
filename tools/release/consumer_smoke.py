import os
import shutil
import subprocess


def cmake_configure_args(consumer_src, cmake_build, cspa_dir, platform, cmake_arch=""):
    args = [
        "cmake",
        "-S",
        str(consumer_src),
        "-B",
        str(cmake_build),
        f"-DCspa_DIR={cspa_dir.as_posix()}",
    ]
    if platform == "windows" and cmake_arch:
        args.extend(["-A", cmake_arch])
    else:
        args.append("-DCMAKE_BUILD_TYPE=Release")
        if platform == "macos" and cmake_arch:
            args.append(f"-DCMAKE_OSX_ARCHITECTURES={cmake_arch}")
    return args


def run_packaged_consumers(platform, build_dir, package_dir):
    environment = os.environ.copy()
    if platform == "windows":
        bin_dir = build_dir / "Release"
        shutil.copy2(package_dir / "bin" / "cspa_ffi.dll", bin_dir / "cspa_ffi.dll")
        names = ("c11_clean_consumer.exe", "cpp17_clean_consumer.exe")
    else:
        bin_dir = build_dir
        names = ("c11_clean_consumer", "cpp17_clean_consumer")
        variable = "DYLD_LIBRARY_PATH" if platform == "macos" else "LD_LIBRARY_PATH"
        library_dir = str(package_dir / "lib")
        environment[variable] = os.pathsep.join(filter(None, (library_dir, environment.get(variable))))
    for name in names:
        subprocess.run([str(bin_dir / name), "--headless"], env=environment, check=True)