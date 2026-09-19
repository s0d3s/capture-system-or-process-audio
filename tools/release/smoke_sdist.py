import pathlib

import capture_system_or_process_audio as cspa

pkg = pathlib.Path(cspa.__file__).resolve().parent
assert (pkg / "py.typed").is_file() and (pkg / "__init__.pyi").is_file() and (pkg / "__main__.py").is_file()
plan = cspa.request([cspa.default_output("pin")]).inspect_for_os("linux")
assert plan.combine_resolution is cspa.CombineResolutionKind.PASS_THROUGH
print("sdist rebuild smoke OK:", cspa.__version__)