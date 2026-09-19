import os, pathlib, shutil, subprocess, tarfile, tempfile

from config import PUBLISHED_CRATES

root = pathlib.Path(".").resolve()
ver = os.environ["EXPECTED_VERSION"]
dist_dir = root / "dist" / "crates"
dist_dir.mkdir(parents=True, exist_ok=True)

# Topological publish order: core -> OS backends -> unified facade.
crates = [(folder, name, index == 0) for index, (folder, name) in enumerate(PUBLISHED_CRATES)]

for folder, pkg_name, full_verify in crates:
    manifest = root / "crates" / folder / "Cargo.toml"
    cmd = ["cargo", "package", "--locked", "--manifest-path", str(manifest)]
    if not full_verify:
        # Dependent crates reference sibling crates before registry upload;
        # we verify all 5 extracted .crate archives together below.
        cmd.extend(["--no-verify", "--exclude-lockfile"])
    subprocess.run(cmd, check=True)
    crate_file = (
        root / "crates" / folder / "target" / "package" / f"{pkg_name}-{ver}.crate"
    )
    assert crate_file.is_file(), f"Missing packaged crate {crate_file}"
    shutil.copy2(crate_file, dist_dir / crate_file.name)

# Verify docs.rs simulation (`DOCS_RS=1`) on unified facade crate.
docs_env = os.environ.copy()
docs_env["DOCS_RS"] = "1"
subprocess.run(
    ["cargo", "doc", "--locked", "--no-deps", "--manifest-path", "crates/capture/Cargo.toml"],
    env=docs_env,
    check=True,
)

# Extract all 5 .crate tarballs outside the repository and compile a clean
# consumer solely against the extracted .crate contents.
with tempfile.TemporaryDirectory(prefix="cspa-crate-verify-") as tmp:
    tmp_path = pathlib.Path(tmp)
    unpack_dir = tmp_path / "unpacked"
    unpack_dir.mkdir()
    patch_lines = []

    for _, pkg_name, _ in crates:
        archive = dist_dir / f"{pkg_name}-{ver}.crate"
        with tarfile.open(archive, "r:gz") as tf:
            names = tf.getnames()
            prefix = f"{pkg_name}-{ver}/"
            assert f"{prefix}README.md" in names, f"Missing README.md in {archive.name}"
            assert (
                f"{prefix}THIRD_PARTY_NOTICES.md" in names
            ), f"Missing THIRD_PARTY_NOTICES.md in {archive.name}"
            tf.extractall(unpack_dir, filter="data")
        extracted = (unpack_dir / f"{pkg_name}-{ver}").as_posix()
        patch_lines.append(f'{pkg_name} = {{ path = "{extracted}" }}')

    consumer = tmp_path / "consumer"
    (consumer / "src").mkdir(parents=True)
    (consumer / "Cargo.toml").write_text(
        "[package]\n"
        'name = "crate-archive-smoke"\n'
        'version = "0.1.0"\n'
        'edition = "2024"\n'
        "publish = false\n\n"
        "[dependencies]\n"
        f'capture-system-or-process-audio = {{ version = "{ver}", default-features = false }}\n\n'
        "[patch.crates-io]\n"
        + "\n".join(patch_lines)
        + "\n",
        encoding="utf-8",
    )
    (consumer / "src" / "main.rs").write_text(
        "use capture_system_or_process_audio::{\n"
        "    CaptureRequest, CaptureTarget, DefaultDeviceMode, Discovery, PlatformOs, check_permissions,\n"
        "};\n"
        "fn main() {\n"
        "    let t = CaptureTarget::default_output(DefaultDeviceMode::Pin);\n"
        "    let _plan = CaptureRequest::single(t.clone()).inspect_for_os(PlatformOs::Linux).unwrap();\n"
        "    let perms = check_permissions(std::slice::from_ref(&t)).unwrap();\n"
        "    assert!(perms.system_audio().required);\n"
        "    let _ = Discovery::new();\n"
        "}\n",
        encoding="utf-8",
    )
    subprocess.run(
        ["cargo", "run", "--manifest-path", str(consumer / "Cargo.toml")],
        check=True,
    )
print("Verified all 5 packaged .crate archives out-of-tree.")
