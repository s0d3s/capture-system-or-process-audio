import os, pathlib, subprocess, tomllib

from config import DEFAULT_RETENTION_DAYS, HOST_PYTHON, MATURIN_VERSION, PUBLISHED_CRATES, PYTHON_SPECS, RELEASE_CARGO_MANIFESTS, ROOT_LICENSE_FILES, matrix_json

root = pathlib.Path(".").resolve()
for lic_name in ROOT_LICENSE_FILES:
    if not (root / lic_name).is_file():
        raise SystemExit(f"Missing required root license file: {lic_name}")

cargo_manifests = RELEASE_CARGO_MANIFESTS

versions = {}
for rel in cargo_manifests:
    data = tomllib.loads((root / rel).read_text(encoding="utf-8"))
    versions[rel] = data["package"]["version"]

pyproject = tomllib.loads(
    (root / "crates/capture-python/pyproject.toml").read_text(encoding="utf-8")
)
versions["crates/capture-python/pyproject.toml"] = pyproject["project"]["version"]

unique_versions = sorted(set(versions.values()))
if len(unique_versions) != 1:
    raise SystemExit(f"Version mismatch across manifests: {versions}")
version = unique_versions[0]

# Verify internal crate dependency version pins match package version.
for folder, _ in PUBLISHED_CRATES:
    rel = f"crates/{folder}/Cargo.toml"
    data = tomllib.loads((root / rel).read_text(encoding="utf-8"))
    tables = [data.get("dependencies", {})]
    for tgt in data.get("target", {}).values():
        tables.append(tgt.get("dependencies", {}))
    for tbl in tables:
        for dep_name, spec in tbl.items():
            if dep_name.startswith("capture-system-or-process-audio"):
                dep_ver = spec.get("version") if isinstance(spec, dict) else None
                if dep_ver != version:
                    raise SystemExit(
                        f"{rel} dependency {dep_name} version={dep_ver!r} != {version!r}"
                    )

event_name = os.environ["EVENT_NAME"]
ref_val = os.environ["GITHUB_REF_VALUE"]
is_release = event_name == "push" and ref_val.startswith("refs/tags/v")
if is_release:
    release_tag = ref_val.removeprefix("refs/tags/")
    tag_version = release_tag.removeprefix("v")
    if tag_version != version:
        raise SystemExit(
            f"Git tag {release_tag!r} (version {tag_version!r}) does not match manifest version {version!r}"
        )
    is_prerelease = "-" in tag_version
else:
    release_tag = f"dry-run-v{version}"
    is_prerelease = False

retention = os.environ.get("INPUT_RETENTION", "").strip() or str(DEFAULT_RETENTION_DAYS)

for rel in cargo_manifests:
    subprocess.run(
        ["cargo", "fmt", "--manifest-path", rel, "--", "--check"],
        check=True,
    )

out_path = pathlib.Path(os.environ["GITHUB_OUTPUT"])
with out_path.open("a", encoding="utf-8") as fh:
    fh.write(f"is_release={'true' if is_release else 'false'}\n")
    fh.write(f"release_tag={release_tag}\n")
    fh.write(f"version={version}\n")
    fh.write(f"is_prerelease={'true' if is_prerelease else 'false'}\n")
    fh.write(f"retention_days={retention}\n")
    fh.write(f"matrix={matrix_json()}\n")
    fh.write(f"maturin_version={MATURIN_VERSION}\n")
    fh.write(f"python_specs={' '.join(PYTHON_SPECS)}\n")
    fh.write(f"host_python={HOST_PYTHON}\n")

print(
    f"Preflight OK: mode={'release' if is_release else 'dry-run'} "
    f"tag={release_tag} version={version} prerelease={is_prerelease}"
)
