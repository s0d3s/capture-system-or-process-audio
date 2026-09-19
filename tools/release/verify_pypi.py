import pathlib

from config import TARGETS, WHEEL_TAGS

dist = pathlib.Path("pypi-dist")
wheels = list(dist.glob("*.whl"))
sdists = list(dist.glob("*.tar.gz"))
if len(wheels) != len(TARGETS) * len(WHEEL_TAGS) or len(sdists) != 1:
    raise SystemExit(f"Unexpected PyPI distribution counts: {len(wheels)} wheels, {len(sdists)} sdists")
print(f"Verified {len(wheels)} wheels and {len(sdists)} sdist")