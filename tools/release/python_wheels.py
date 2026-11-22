import subprocess

from config import PYTHON_WHEELS


def build_wheels(maturin_common, wheels_dir, python_executables):
    for wheel in PYTHON_WHEELS:
        command = [*maturin_common, "-i", python_executables[wheel["build_python"]]]
        if wheel["features"]:
            command.extend(["--features", ",".join(wheel["features"])])
        subprocess.run(command, check=True)

    wheels = {}
    for wheel in PYTHON_WHEELS:
        matches = list(wheels_dir.glob(f"*-{wheel['tag']}-*.whl"))
        if len(matches) != 1:
            raise ValueError(f"Expected one wheel for {wheel['tag']}, got {[path.name for path in matches]}")
        wheels[wheel["tag"]] = matches[0]
    if set(wheels.values()) != set(wheels_dir.glob("*.whl")):
        raise ValueError(f"Unexpected wheel inventory: {[path.name for path in wheels_dir.glob('*.whl')]}")
    return wheels


def smoke_cases(wheels, python_executables):
    for wheel in PYTHON_WHEELS:
        for spec, free_threaded in wheel["smoke_pythons"]:
            yield spec, python_executables[spec], wheels[wheel["tag"]], free_threaded