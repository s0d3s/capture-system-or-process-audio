import json, os, subprocess, time, urllib.error, urllib.request

from config import PUBLISHED_CRATES

ver = os.environ["PACKAGE_VERSION"]
crates = [(f"crates/{folder}/Cargo.toml", name) for folder, name in PUBLISHED_CRATES]

def already_published(crate_name: str, version: str) -> bool:
    url = f"https://crates.io/api/v1/crates/{crate_name}/{version}"
    req = urllib.request.Request(
        url,
        headers={"User-Agent": "capture-system-or-process-audio-release-ci/1.0"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status == 200
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        raise

for idx, (manifest, crate_name) in enumerate(crates):
    if already_published(crate_name, ver):
        print(f"Skipping {crate_name} {ver}: already present on crates.io")
        continue
    print(f"Publishing [{idx + 1}/{len(crates)}] {crate_name} {ver}...")
    subprocess.run(
        ["cargo", "publish", "--locked", "--manifest-path", manifest],
        check=True,
    )
    # Brief sparse-index propagation settle before dependent crates resolve it.
    if idx + 1 < len(crates):
        time.sleep(5)
