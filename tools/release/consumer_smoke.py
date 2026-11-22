import os
import shutil
import subprocess


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