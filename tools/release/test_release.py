import hashlib
import json
import os
import pathlib
import runpy
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest import mock

from cmake_version import generate_cmake_version
from config import HOST_PYTHON, PUBLISHED_CRATES, PYTHON_SPECS, ROOT_LICENSE_FILES, TARGETS, WHEEL_TAGS, matrix_json

SCRIPTS = pathlib.Path(__file__).resolve().parent


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.raw = self.root / "raw-artifacts"
        self.raw.mkdir()
        self.environment = os.environ.copy()
        self.environment.update({
            "RELEASE_TAG": "dry-run-v1.2.3",
            "IS_RELEASE": "false",
            "GITHUB_STEP_SUMMARY": str(self.root / "summary.md"),
        })

    def run_script(self, name):
        return subprocess.run(
            [sys.executable, str(SCRIPTS / name)],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
        )

    def populate(self):
        names = [f"{name}-1.2.3.crate" for _, name in PUBLISHED_CRATES]
        names += [f"pkg-1.2.3-{tag}-{target['label']}.whl" for target in TARGETS for tag in WHEEL_TAGS]
        names += ["capture_system_or_process_audio-1.2.3.tar.gz"]
        names += [f"cspa-ffi-1.2.3-{target['target']}.zip" for target in TARGETS]
        names += [f"cspa-tui-demo-1.2.3-{target['target']}.zip" for target in TARGETS]
        for name in names:
            (self.raw / name).write_bytes(name.encode())
        return names

    def test_matrix_has_unique_targets(self):
        self.assertEqual(json.loads(matrix_json())["include"], list(TARGETS))
        self.assertEqual(len({target["target"] for target in TARGETS}), len(TARGETS))
        self.assertEqual(len({target["label"] for target in TARGETS}), len(TARGETS))

    def test_preflight_dry_run_and_tag_mismatch(self):
        repository = SCRIPTS.parent.parent
        version = tomllib.loads((repository / "crates/capture-core/Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
        output = self.root / "output"
        environment = {
            "EVENT_NAME": "workflow_dispatch",
            "GITHUB_REF_VALUE": "refs/heads/main",
            "INPUT_RETENTION": "21",
            "GITHUB_OUTPUT": str(output),
        }
        with mock.patch.dict(os.environ, environment), mock.patch("subprocess.run") as cargo:
            runpy.run_path(str(SCRIPTS / "preflight.py"), run_name="__main__")
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        self.assertEqual(values["release_tag"], f"dry-run-v{version}")
        self.assertEqual(values["retention_days"], "21")
        self.assertEqual(values["python_specs"].split(), list(PYTHON_SPECS))
        self.assertEqual(values["host_python"], HOST_PYTHON)
        self.assertEqual(json.loads(values["matrix"])["include"], list(TARGETS))
        self.assertEqual(cargo.call_count, len(PUBLISHED_CRATES) + 3)
        environment.update({"EVENT_NAME": "push", "GITHUB_REF_VALUE": "refs/tags/v999.0.0"})
        with mock.patch.dict(os.environ, environment), mock.patch("subprocess.run") as cargo:
            with self.assertRaisesRegex(SystemExit, "does not match manifest version"):
                runpy.run_path(str(SCRIPTS / "preflight.py"), run_name="__main__")
            cargo.assert_not_called()

    def test_bundle_checksums_and_summary(self):
        names = self.populate()
        result = self.run_script("bundle_artifacts.py")
        self.assertEqual(result.returncode, 0, result.stderr)
        checksums = (self.root / "release-assets" / "SHA256SUMS.txt").read_text().splitlines()
        self.assertEqual(len(checksums), len(names))
        for name in names:
            self.assertIn(f"{hashlib.sha256(name.encode()).hexdigest()}  {name}", checksums)
        self.assertIn(f"Verified **{len(names)}** artifacts", (self.root / "summary.md").read_text())

    def test_bundle_rejects_missing_and_duplicate_files(self):
        names = self.populate()
        (self.raw / names[0]).unlink()
        self.assertNotEqual(self.run_script("bundle_artifacts.py").returncode, 0)
        (self.raw / names[0]).write_bytes(b"first")
        duplicate = self.raw / "duplicate"
        duplicate.mkdir()
        (duplicate / names[0]).write_bytes(b"second")
        result = self.run_script("bundle_artifacts.py")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Duplicate artifact filename collision", result.stderr)

    def test_pypi_inventory_count(self):
        names = self.populate()
        dist = self.root / "pypi-dist"
        dist.mkdir()
        for name in names:
            if name.endswith(".whl") or name.startswith("capture_system_or_process_audio-"):
                (dist / name).write_bytes(b"")
        self.assertEqual(self.run_script("verify_pypi.py").returncode, 0)
        next(dist.glob("*.whl")).unlink()
        self.assertNotEqual(self.run_script("verify_pypi.py").returncode, 0)

    def test_root_licenses_present_and_staged_in_build_target(self):
        repository = SCRIPTS.parent.parent
        for lic_name in ROOT_LICENSE_FILES:
            self.assertTrue((repository / lic_name).is_file(), f"Missing {lic_name}")
        build_target_src = (SCRIPTS / "build_target.py").read_text(encoding="utf-8")
        self.assertEqual(build_target_src.count("for lic_name in ROOT_LICENSE_FILES:"), 2)

    def test_cmake_version_selection(self):
        repository = SCRIPTS.parent.parent
        template = repository / "crates/capture-ffi/cmake/CspaConfigVersion.cmake.in"
        manifest = self.root / "Cargo.toml"
        package = self.root / "package with spaces"
        package.mkdir()
        (package / "CspaConfig.cmake").write_text("set(Cspa_FOUND TRUE)\n", encoding="utf-8")
        version_file = package / "CspaConfigVersion.cmake"
        probe = self.root / "probe.cmake"
        probe.write_text(
            'find_package(Cspa ${REQUEST} ${EXACT_ARG} REQUIRED CONFIG NO_DEFAULT_PATH PATHS "${PACKAGE_DIR}")\n'
            'if(NOT Cspa_VERSION STREQUAL EXPECTED_VERSION)\n'
            '  message(FATAL_ERROR "Unexpected package version: ${Cspa_VERSION}")\n'
            'endif()\n',
            encoding="utf-8",
        )
        for version, cases in (
            ("1.3.1", (("1.3.1", True, True), ("1.3", False, True), ("1.3.0", False, True),
                       ("1.3.0", True, False), ("1.3.2", False, False), ("1.2", False, False),
                       ("1.4", False, False), ("2.0", False, False), ("0.1", False, False),
                       ("1.3.0...1.3.2", False, False))),
            ("2.5.7", (("2.5.7", True, True), ("2.5", False, True), ("2.5.8", False, False),
                       ("1.3", False, False), ("2.4", False, False))),
            ("1.3.1-rc.1", (("1.3.1", True, False), ("1.3", False, False))),
        ):
            manifest.write_text(f'[package]\nname = "example"\nversion = "{version}"\n', encoding="utf-8")
            self.assertEqual(generate_cmake_version(manifest, template, version_file), version)
            for requested, exact, accepted in cases:
                with self.subTest(version=version, requested=requested, exact=exact):
                    result = subprocess.run(
                        ["cmake", f"-DREQUEST={requested}", f"-DEXACT_ARG={'EXACT' if exact else ''}",
                         f"-DPACKAGE_DIR={package}", f"-DEXPECTED_VERSION={version}", "-P", str(probe)],
                        capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)

        unversioned = self.root / "unversioned.cmake"
        unversioned.write_text(
            'find_package(Cspa REQUIRED CONFIG NO_DEFAULT_PATH PATHS "${PACKAGE_DIR}")\n',
            encoding="utf-8",
        )
        result = subprocess.run(
            ["cmake", f"-DPACKAGE_DIR={package}", "-P", str(unversioned)],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_cmake_version_rejects_invalid_manifest_version(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_text('[package]\nversion = "not-a-version"\n', encoding="utf-8")
        template = SCRIPTS.parent.parent / "crates/capture-ffi/cmake/CspaConfigVersion.cmake.in"
        with self.assertRaisesRegex(ValueError, "Invalid Cargo package version"):
            generate_cmake_version(manifest, template, self.root / "version.cmake")


if __name__ == "__main__":
    unittest.main()