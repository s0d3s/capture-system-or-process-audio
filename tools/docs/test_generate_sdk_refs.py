#!/usr/bin/env python3
"""Unit tests for tools/docs/generate_sdk_refs.py."""

from __future__ import annotations

import pathlib
import re
import sys
import tempfile
import unittest

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "tools" / "docs"))

import generate_sdk_refs as gen  # noqa: E402


class GenerateSdkRefsTests(unittest.TestCase):
    def test_generates_all_12_expected_mdx_pages_with_valid_frontmatter(self) -> None:
        pages = gen.generate_all_ref_pages(REPO_ROOT)
        self.assertEqual(
            set(pages),
            {
                "python/functions.mdx",
                "python/targets-and-discovery.mdx",
                "python/streams-and-planning.mdx",
                "python/mixing-and-conversion.mdx",
                "python/permissions-and-enums.mdx",
                "rust/overview.mdx",
                "rust/types-and-traits.mdx",
                "cpp/classes-and-streams.mdx",
                "cpp/config-and-reports.mdx",
                "cpp/errors-and-enums.mdx",
                "c/abi-and-functions.mdx",
                "c/records-and-enums.mdx",
            },
        )
        for rel, content in pages.items():
            self.assertTrue(content.startswith("---\ntitle: "), f"Missing frontmatter in {rel}")
            self.assertNotIn("\r\n", content, f"CRLF found in {rel}")
            m_desc = re.search(r'^description:\s*"([^"]+)"', content, re.MULTILINE)
            self.assertIsNotNone(m_desc, f"Missing description in {rel}")
            assert m_desc is not None
            self.assertGreaterEqual(len(m_desc.group(1)), 40, rel)
            self.assertLessEqual(len(m_desc.group(1)), 180, rel)

    def test_python_parser_extracts_all_functions_classes_and_33_enums(self) -> None:
        funcs, classes = gen.parse_python_pyi(
            REPO_ROOT
            / "crates"
            / "capture-python"
            / "python"
            / "capture_system_or_process_audio"
            / "__init__.pyi"
        )
        self.assertEqual(len(funcs), 20)
        enum_cls = [c for c in classes.values() if c.is_enum]
        frozen_cls = [c for c in classes.values() if not c.is_enum]
        self.assertEqual(len(enum_cls), 33)
        self.assertEqual(len(frozen_cls), 26)

    def test_c11_parser_extracts_verified_record_sizes_and_excludes_test_hooks(self) -> None:
        _, _, structs, _, funcs = gen.parse_c11_header_and_layout(
            REPO_ROOT / "crates" / "capture-ffi" / "include" / "cspa_experimental.h",
            REPO_ROOT / "crates" / "capture-ffi" / "tests" / "representation-smoke.c",
        )
        struct_sizes = {s.name: s.sizeof_bytes for s in structs}
        self.assertEqual(struct_sizes["cspa_capture_target_record"], 832)
        self.assertEqual(struct_sizes["cspa_capture_request_record"], 96)
        self.assertEqual(struct_sizes["cspa_plan_report_record"], 112)
        self.assertEqual(struct_sizes["cspa_permission_report_record"], 104)
        self.assertEqual(struct_sizes["cspa_managed_read_outcome_record"], 264)
        self.assertEqual(struct_sizes["cspa_discovery_event_record"], 1120)
        for f in funcs:
            self.assertFalse(f.name.startswith("cspa_test_"))

    def test_cpp17_parser_extracts_enums_classes_and_free_functions(self) -> None:
        enums, classes, funcs = gen.parse_cpp_header(
            REPO_ROOT / "crates" / "capture-ffi" / "include" / "cspa.hpp"
        )
        self.assertEqual(len(enums), 25)
        self.assertEqual(len(classes), 20)
        self.assertIn("ManagedStream", classes)
        self.assertIn("Result", classes)
        self.assertIn("Error", classes)
        self.assertEqual({f.name for f in funcs}, {"unwrap_or_throw", "check_permissions", "request_permissions"})

    def test_rust_parser_generates_valid_rustdoc_urls(self) -> None:
        items = gen.parse_rust_facade(REPO_ROOT)
        by_name = {i.name: i for i in items}
        self.assertEqual(
            by_name["CaptureManager"].rustdoc_url,
            "/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/type.CaptureManager.html",
        )
        self.assertEqual(
            by_name["CaptureTarget"].rustdoc_url,
            "/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/struct.CaptureTarget.html",
        )
        self.assertEqual(
            by_name["CombineMode"].rustdoc_url,
            "/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/enum.CombineMode.html",
        )
        self.assertEqual(
            by_name["CaptureManagerBackend"].rustdoc_url,
            "/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/trait.CaptureManagerBackend.html",
        )
        self.assertEqual(
            by_name["capture"].rustdoc_url,
            "/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/fn.capture.html",
        )

    def test_sync_and_check_idempotence_and_drift_detection(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cspa-ref-gen-test-") as tmp_str:
            out = pathlib.Path(tmp_str)
            # Before generating, --check must detect missing files:
            missing = gen.sync_or_check_ref_pages(out, workspace_root=REPO_ROOT, check_only=True)
            self.assertEqual(len(missing), 12)

            # Generate once:
            diffs_write = gen.sync_or_check_ref_pages(out, workspace_root=REPO_ROOT, check_only=False)
            self.assertEqual(diffs_write, [])

            # Immediate --check must report 0 byte diffs:
            diffs_check = gen.sync_or_check_ref_pages(out, workspace_root=REPO_ROOT, check_only=True)
            self.assertEqual(diffs_check, [])

            # Mutating one file must fail --check:
            target_f = out / "python" / "functions.mdx"
            target_f.write_text(target_f.read_text(encoding="utf-8") + "\n<!-- drift -->\n", encoding="utf-8")
            drift = gen.sync_or_check_ref_pages(out, workspace_root=REPO_ROOT, check_only=True)
            self.assertEqual(len(drift), 1)
            self.assertIn("python/functions.mdx", drift[0])

            # Injecting a stale file must also fail --check and be cleaned up on sync:
            stale_f = out / "c" / "obsolete.mdx"
            stale_f.write_text("---\ntitle: old\ndescription: old\n---\n", encoding="utf-8")
            stale_diffs = gen.sync_or_check_ref_pages(out, workspace_root=REPO_ROOT, check_only=True)
            self.assertEqual(len(stale_diffs), 2)

            gen.sync_or_check_ref_pages(out, workspace_root=REPO_ROOT, check_only=False)
            self.assertFalse(stale_f.exists())
            self.assertEqual(gen.sync_or_check_ref_pages(out, workspace_root=REPO_ROOT, check_only=True), [])


if __name__ == "__main__":
    unittest.main()
