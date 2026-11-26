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
from config import DEFAULT_RETENTION_DAYS, HOST_PYTHON, PUBLISHED_CRATES, PYTHON_SPECS, PYTHON_WHEELS, RELEASE_CARGO_MANIFESTS, ROOT_LICENSE_FILES, TARGETS, WHEEL_TAGS, matrix_json
from consumer_smoke import cmake_configure_args, run_packaged_consumers
from python_wheels import build_release_wheels, build_wheels, smoke_cases

SCRIPTS = pathlib.Path(__file__).resolve().parent


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.raw = self.root / "raw-artifacts"
        self.raw.mkdir()
        manifest = SCRIPTS.parent.parent / "crates/capture-core/Cargo.toml"
        self.version = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]["version"]
        self.environment = os.environ.copy()
        self.environment.update({
            "RELEASE_TAG": f"dry-run-v{self.version}",
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
        names = [f"{name}-{self.version}.crate" for _, name in PUBLISHED_CRATES]
        names += [f"pkg-{self.version}-{tag}-{target['label']}.whl" for target in TARGETS for tag in WHEEL_TAGS]
        names += [f"capture_system_or_process_audio-{self.version}.tar.gz"]
        names += [f"cspa-ffi-{self.version}-{target['target']}.zip" for target in TARGETS]
        names += [f"cspa-tui-demo-{self.version}-{target['target']}.zip" for target in TARGETS]
        for name in names:
            (self.raw / name).write_bytes(name.encode())
        return names

    def test_matrix_has_unique_targets(self):
        self.assertEqual(json.loads(matrix_json())["include"], list(TARGETS))
        self.assertEqual(len({target["target"] for target in TARGETS}), len(TARGETS))
        self.assertEqual(len({target["label"] for target in TARGETS}), len(TARGETS))

    def test_macos_targets_and_docs_rs_parity(self):
        repository = SCRIPTS.parent.parent
        by_label = {target["label"]: target for target in TARGETS}
        self.assertEqual(
            by_label["macos-aarch64"],
            {
                "label": "macos-aarch64",
                "runner": "macos-15",
                "target": "aarch64-apple-darwin",
                "platform": "macos",
                "backend_crate": "crates/capture-macos/Cargo.toml",
                "cmake_arch": "arm64",
            },
        )
        self.assertEqual(
            by_label["macos-x86_64"],
            {
                "label": "macos-x86_64",
                "runner": "macos-15-intel",
                "target": "x86_64-apple-darwin",
                "platform": "macos",
                "backend_crate": "crates/capture-macos/Cargo.toml",
                "cmake_arch": "x86_64",
            },
        )

        facade_docs = tomllib.loads(
            (repository / "crates/capture/Cargo.toml").read_text(encoding="utf-8")
        )["package"]["metadata"]["docs"]["rs"]["targets"]
        self.assertEqual(set(facade_docs), {target["target"] for target in TARGETS})

        for target in TARGETS:
            backend_docs = tomllib.loads(
                (repository / target["backend_crate"]).read_text(encoding="utf-8")
            )["package"]["metadata"]["docs"]["rs"]["targets"]
            self.assertIn(target["target"], backend_docs)

        build_rs = (repository / "crates/capture-macos/build.rs").read_text(encoding="utf-8")
        self.assertIn('"aarch64" => "arm64"', build_rs)
        self.assertIn('"x86_64" => "x86_64"', build_rs)
        self.assertIn('"-arch"', build_rs)
        self.assertIn("-mmacosx-version-min=", build_rs)
        self.assertIn("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET", build_rs)

        workflow = (repository / ".github/workflows/publish-release.yml").read_text(encoding="utf-8")
        self.assertEqual(workflow.count('MACOSX_DEPLOYMENT_TARGET: "14.0"'), 2)
        self.assertEqual(workflow.count('CSPA_DISABLE_TCC_PROMPT: "1"'), 2)

    def test_cmake_configure_args_selects_platform_architecture(self):
        src = self.root / "src"
        bld = self.root / "build"
        cspa_dir = self.root / "pkg/lib/cmake/Cspa"
        self.assertEqual(
            cmake_configure_args(src, bld, cspa_dir, "windows", "ARM64"),
            ["cmake", "-S", str(src), "-B", str(bld), f"-DCspa_DIR={cspa_dir.as_posix()}", "-A", "ARM64"],
        )
        self.assertEqual(
            cmake_configure_args(src, bld, cspa_dir, "macos", "x86_64"),
            [
                "cmake",
                "-S",
                str(src),
                "-B",
                str(bld),
                f"-DCspa_DIR={cspa_dir.as_posix()}",
                "-DCMAKE_BUILD_TYPE=Release",
                "-DCMAKE_OSX_ARCHITECTURES=x86_64",
            ],
        )
        self.assertEqual(
            cmake_configure_args(src, bld, cspa_dir, "linux", ""),
            ["cmake", "-S", str(src), "-B", str(bld), f"-DCspa_DIR={cspa_dir.as_posix()}", "-DCMAKE_BUILD_TYPE=Release"],
        )

    def test_python_wheel_configuration(self):
        self.assertEqual(len(WHEEL_TAGS), len(set(WHEEL_TAGS)))
        self.assertEqual(len(PYTHON_SPECS), len(set(PYTHON_SPECS)))
        self.assertIn(HOST_PYTHON, PYTHON_SPECS)
        features = tomllib.loads(
            (SCRIPTS.parent.parent / "crates/capture-python/Cargo.toml").read_text(encoding="utf-8")
        )["features"]
        for wheel in PYTHON_WHEELS:
            self.assertIn(wheel["build_python"], PYTHON_SPECS)
            self.assertTrue(wheel["smoke_pythons"])
            for feature in wheel["features"]:
                self.assertIn(feature, features)
            for spec, free_threaded in wheel["smoke_pythons"]:
                self.assertEqual("+freethreaded" in spec, free_threaded)

    def test_windows_wheel_build_uses_temporary_target_directory(self):
        observed = []

        def fake_build(command, wheels_dir, executables):
            target_dir = pathlib.Path(command[command.index("--target-dir") + 1])
            self.assertTrue(target_dir.is_dir())
            self.assertEqual(target_dir.parent, self.root)
            observed.append(target_dir)
            return {"result": wheels_dir}

        with mock.patch.dict(os.environ, {"RUNNER_TEMP": str(self.root)}), \
             mock.patch("python_wheels.build_wheels", side_effect=fake_build):
            self.assertEqual(build_release_wheels(["maturin"], self.root, {}, "windows"), {"result": self.root})
        self.assertFalse(observed[0].exists())
        with mock.patch.dict(os.environ, {"RUNNER_TEMP": str(self.root)}), \
             mock.patch("python_wheels.build_wheels", side_effect=RuntimeError("failure")) as run:
            with self.assertRaisesRegex(RuntimeError, "failure"):
                build_release_wheels(["maturin"], self.root, {}, "windows")
            failed_target = pathlib.Path(run.call_args.args[0][-1])
        self.assertFalse(failed_target.exists())
        with mock.patch("python_wheels.build_wheels", return_value={}) as run:
            build_release_wheels(["maturin"], self.root, {}, "linux")
        run.assert_called_once_with(["maturin"], self.root, {})

    def test_linux_release_exposes_zig_wrapper(self):
        workflow = (SCRIPTS.parent.parent / ".github/workflows/publish-release.yml").read_text(encoding="utf-8")
        self.assertIn('uv tool install --with-executables-from ziglang "maturin[zig]==$MATURIN_VERSION"', workflow)
        self.assertIn('ZIG_WRAPPER="$(uv tool dir --bin)/python-zig"', workflow)
        self.assertIn('"$ZIG_WRAPPER" version', workflow)
        self.assertIn('CARGO_ZIGBUILD_ZIG_PATH=%s', workflow)

    def test_preflight_dry_run_and_tag_mismatch(self):
        version = self.version
        output = self.root / "output"
        environment = {
            "EVENT_NAME": "workflow_dispatch",
            "GITHUB_REF_VALUE": "refs/heads/main",
            "INPUT_RETENTION": str(DEFAULT_RETENTION_DAYS + 1),
            "GITHUB_OUTPUT": str(output),
        }
        with mock.patch.dict(os.environ, environment), mock.patch("subprocess.run") as cargo:
            runpy.run_path(str(SCRIPTS / "preflight.py"), run_name="__main__")
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        self.assertEqual(values["release_tag"], f"dry-run-v{version}")
        self.assertEqual(values["retention_days"], environment["INPUT_RETENTION"])
        self.assertEqual(values["python_specs"].split(), list(PYTHON_SPECS))
        self.assertEqual(values["host_python"], HOST_PYTHON)
        self.assertEqual(json.loads(values["matrix"])["include"], list(TARGETS))
        self.assertEqual(
            cargo.call_args_list,
            [mock.call(["cargo", "fmt", "--manifest-path", manifest, "--", "--check"], check=True)
             for manifest in RELEASE_CARGO_MANIFESTS],
        )
        mismatch_major = int(version.split(".", 1)[0]) + 1
        environment.update({"EVENT_NAME": "push", "GITHUB_REF_VALUE": f"refs/tags/v{mismatch_major}.0.0"})
        with mock.patch.dict(os.environ, environment), mock.patch("subprocess.run") as cargo:
            with self.assertRaisesRegex(SystemExit, "does not match manifest version"):
                runpy.run_path(str(SCRIPTS / "preflight.py"), run_name="__main__")
            cargo.assert_not_called()

    def test_preflight_supports_changed_manifest_versions(self):
        parse_toml = tomllib.loads
        major = int(self.version.split(".", 1)[0]) + 1
        for index, version in enumerate((f"{major}.0.0", f"{major}.0.1-rc.1")):
            with self.subTest(version=version):
                def with_version(source):
                    data = parse_toml(source)
                    if "package" in data:
                        data["package"]["version"] = version
                        tables = [data.get("dependencies", {})]
                        tables.extend(target.get("dependencies", {}) for target in data.get("target", {}).values())
                        for table in tables:
                            for name, spec in table.items():
                                if name.startswith("capture-system-or-process-audio") and isinstance(spec, dict):
                                    spec["version"] = version
                    if "project" in data:
                        data["project"]["version"] = version
                    return data

                output = self.root / f"version-output-{index}"
                environment = {
                    "EVENT_NAME": "push",
                    "GITHUB_REF_VALUE": f"refs/tags/v{version}",
                    "INPUT_RETENTION": "",
                    "GITHUB_OUTPUT": str(output),
                }
                with mock.patch.dict(os.environ, environment), mock.patch("tomllib.loads", side_effect=with_version), mock.patch("subprocess.run"):
                    runpy.run_path(str(SCRIPTS / "preflight.py"), run_name="__main__")
                values = dict(line.split("=", 1) for line in output.read_text().splitlines())
                self.assertEqual(values["version"], version)
                self.assertEqual(values["release_tag"], f"v{version}")
                self.assertEqual(values["is_release"], "true")
                self.assertEqual(values["is_prerelease"], "true" if "-" in version else "false")
                self.assertEqual(values["retention_days"], str(DEFAULT_RETENTION_DAYS))

    def test_python_wheel_build_and_smoke_plans(self):
        wheels_dir = self.root / "wheels"
        wheels_dir.mkdir()
        executables = {spec: f"python-for-{spec}" for spec in PYTHON_SPECS}
        commands = []

        def fake_build(command, check):
            self.assertTrue(check)
            wheel = PYTHON_WHEELS[len(commands)]
            commands.append(command)
            (wheels_dir / f"package-{self.version}-{wheel['tag']}-platform.whl").touch()

        with mock.patch("python_wheels.subprocess.run", side_effect=fake_build):
            wheels = build_wheels(["maturin", "build"], wheels_dir, executables)
        self.assertEqual(len(commands), len(PYTHON_WHEELS))
        for wheel, command in zip(PYTHON_WHEELS, commands):
            expected = ["maturin", "build", "-i", executables[wheel["build_python"]]]
            if wheel["features"]:
                expected += ["--features", ",".join(wheel["features"])]
            self.assertEqual(command, expected)
        expected_cases = [
            (spec, executables[spec], wheels[wheel["tag"]], free_threaded)
            for wheel in PYTHON_WHEELS for spec, free_threaded in wheel["smoke_pythons"]
        ]
        self.assertEqual(list(smoke_cases(wheels, executables)), expected_cases)
        (wheels_dir / "unexpected.whl").touch()
        with mock.patch("python_wheels.subprocess.run"), self.assertRaisesRegex(ValueError, "Unexpected wheel inventory"):
            build_wheels(["maturin", "build"], wheels_dir, executables)
        (wheels_dir / "unexpected.whl").unlink()
        next(iter(wheels.values())).unlink()
        with mock.patch("python_wheels.subprocess.run"), self.assertRaisesRegex(ValueError, "Expected one wheel"):
            build_wheels(["maturin", "build"], wheels_dir, executables)

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

    def test_hosted_facade_excludes_named_live_test(self):
        repository = SCRIPTS.parent.parent
        name = "unified_facade_discovery_inspect_stream_manager_zero_alloc_hot_swap_and_drop"
        test_source = (repository / "crates/capture/tests/cross_platform_api.rs").read_text(encoding="utf-8")
        workflow = (repository / ".github/workflows/publish-release.yml").read_text(encoding="utf-8")
        self.assertEqual(test_source.count(f"fn {name}()"), 1)
        self.assertEqual(workflow.count(f"--skip {name}"), 1)

    def test_hosted_windows_excludes_device_dependent_backend_tests(self):
        repository = SCRIPTS.parent.parent
        names = (
            "platform_discovery_one_shot_leaves_no_watcher_and_supports_pre_audio_process",
            "sta_thread_compatibility_and_default_endpoint_observer_benchmark",
        )
        test_source = (repository / "crates/capture-windows/src/discovery.rs").read_text(encoding="utf-8")
        workflow = (repository / ".github/workflows/publish-release.yml").read_text(encoding="utf-8")
        backend_step = workflow.split('cargo clippy --locked --manifest-path "$BACKEND_MANIFEST"', 1)[1]
        windows_branch, other_branch = backend_step.split('if [ "$MATRIX_PLATFORM" = "windows" ]; then', 1)[1].split("else", 1)
        other_branch = other_branch.split("fi", 1)[0]
        for name in names:
            self.assertEqual(test_source.count(f"fn {name}()"), 1)
            self.assertEqual(windows_branch.count(f"--skip discovery::tests::{name}"), 1)
        self.assertIn('cargo test --locked --manifest-path "$BACKEND_MANIFEST"', other_branch)
        self.assertNotIn("--skip", other_branch)

    def test_packaged_consumer_smoke_is_headless_and_fail_closed(self):
        build_dir = self.root / "build"
        package_dir = self.root / "package"
        for platform in ("windows", "linux", "macos"):
            with self.subTest(platform=platform):
                variable = "DYLD_LIBRARY_PATH" if platform == "macos" else "LD_LIBRARY_PATH"
                with mock.patch.dict(os.environ, {variable: "existing"}), \
                     mock.patch("consumer_smoke.shutil.copy2") as copy, \
                     mock.patch("consumer_smoke.subprocess.run") as run:
                    run_packaged_consumers(platform, build_dir, package_dir)
                self.assertEqual(len(run.call_args_list), 2)
                for call, name in zip(run.call_args_list, ("c11_clean_consumer", "cpp17_clean_consumer")):
                    expected = build_dir / "Release" / (name + ".exe") if platform == "windows" else build_dir / name
                    self.assertEqual(call.args[0], [str(expected), "--headless"])
                    self.assertTrue(call.kwargs["check"])
                    if platform != "windows":
                        self.assertEqual(call.kwargs["env"][variable], os.pathsep.join((str(package_dir / "lib"), "existing")))
                self.assertEqual(copy.call_count, int(platform == "windows"))
        with mock.patch("consumer_smoke.subprocess.run", side_effect=subprocess.CalledProcessError(1, "consumer")):
            with self.assertRaises(subprocess.CalledProcessError):
                run_packaged_consumers("linux", build_dir, package_dir)

    @unittest.skipIf(os.name == "nt", "The sdist release job runs on Linux")
    def test_sdist_uses_selected_python_library_path(self):
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        interpreter = bin_dir / "managed-python"
        lib_dir = self.root / "python libraries"
        scripts = {
            "uv": 'case "$1 $2" in "python install") exit 0;; "python find") printf "%s\\n" "$FAKE_PYTHON";; *) exit 91;; esac\n',
            "managed-python": 'printf "%s\\n" "$FAKE_LIBDIR"\n',
            "cargo": 'if [ "$1" = build ]; then exit 0; fi\n'
                     'test "$1" = test && test "$PYO3_PYTHON" = "$FAKE_PYTHON" && '
                     'test "$LD_LIBRARY_PATH" = "$FAKE_LIBDIR:$ORIGINAL_LIBDIR" || exit 92\n'
                     'printf "%s\\n" loader-path-ok\nexit 23\n',
        }
        for name, body in scripts.items():
            script = bin_dir / name
            script.write_text("#!/usr/bin/env bash\nset -eu\n" + body, encoding="utf-8")
            script.chmod(0o755)
        environment = os.environ.copy()
        environment.update({
            "PATH": f"{bin_dir}{os.pathsep}{environment['PATH']}",
            "HOST_PYTHON": HOST_PYTHON,
            "MATURIN_VERSION": "unused",
            "FAKE_PYTHON": str(interpreter),
            "FAKE_LIBDIR": str(lib_dir),
            "ORIGINAL_LIBDIR": str(self.root / "existing libraries"),
            "LD_LIBRARY_PATH": str(self.root / "existing libraries"),
        })
        result = subprocess.run(
            ["bash", str(SCRIPTS / "build_sdist.sh")], cwd=self.root,
            env=environment, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertIn("loader-path-ok", result.stdout)

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
        unversioned = self.root / "unversioned.cmake"
        unversioned.write_text(
            'find_package(Cspa REQUIRED CONFIG NO_DEFAULT_PATH PATHS "${PACKAGE_DIR}")\n',
            encoding="utf-8",
        )
        numeric_version = self.version.split("-", 1)[0].split("+", 1)[0]
        current_major, current_minor, current_patch = map(int, numeric_version.split("."))
        components = {
            (current_major, current_minor, current_patch),
            (0, 0, 0),
            (current_major + 1, current_minor + 1, current_patch + 1),
        }
        versions_and_cases = []
        for major, minor, patch in sorted(components):
            version = f"{major}.{minor}.{patch}"
            short = f"{major}.{minor}"
            lower = f"{short}.0"
            newer = f"{short}.{patch + 1}"
            cases = [
                (version, True, True), (short, False, True), (lower, False, True),
                (lower, True, patch == 0), (newer, False, False),
                (f"{major}.{minor + 1}", False, False), (f"{major + 1}.0", False, False),
                (f"{lower}...{newer}", False, False),
            ]
            if minor:
                cases.append((f"{major}.{minor - 1}", False, False))
            if major:
                cases.append((f"{major - 1}.{minor}", False, False))
            versions_and_cases.append((version, cases))
            versions_and_cases.extend(
                (version + suffix, [(version, True, False), (short, False, False)])
                for suffix in ("-rc.1", "+build.1")
            )
        for version, cases in versions_and_cases:
            manifest.write_text(f'[package]\nname = "example"\nversion = "{version}"\n', encoding="utf-8")
            self.assertEqual(generate_cmake_version(manifest, template, version_file), version)
            for requested, exact, accepted in cases:
                with self.subTest(version=version, requested=requested, exact=exact):
                    result = subprocess.run(
                        ["cmake", "-Werror=dev", f"-DREQUEST={requested}", f"-DEXACT_ARG={'EXACT' if exact else ''}",
                         f"-DPACKAGE_DIR={package}", f"-DEXPECTED_VERSION={version}", "-P", str(probe)],
                        capture_output=True, text=True,
                    )
                    self.assertNotIn("CMake Error (dev)", result.stderr)
                    self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
            with self.subTest(version=version, requested=None):
                result = subprocess.run(
                    ["cmake", "-Werror=dev", f"-DPACKAGE_DIR={package}", "-P", str(unversioned)],
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