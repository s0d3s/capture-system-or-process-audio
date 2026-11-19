#!/usr/bin/env python3
"""Unit tests for tools/docs/verify_built_site.py."""

from __future__ import annotations

import json
import pathlib
import sys
import tempfile
import unittest

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "tools" / "docs"))

import verify_built_site as vbs  # noqa: E402


def _seed_minimal_valid_dist(dist: pathlib.Path) -> None:
    base = "https://s0d3s.github.io/capture-system-or-process-audio"
    (dist / "pagefind").mkdir(parents=True)
    (dist / "pagefind" / "pagefind.js").write_text("// pagefind", encoding="utf-8")
    (dist / "og-card-1280x640.png").write_bytes(b"\x89PNG\r\n\x1a\n")
    (dist / "robots.txt").write_text(f"User-agent: *\nAllow: /\nSitemap: {base}/sitemap-index.xml\n", encoding="utf-8")
    (dist / "sitemap-index.xml").write_text(
        f'<?xml version="1.0" encoding="UTF-8"?><sitemapindex><sitemap><loc>{base}/sitemap-0.xml</loc></sitemap></sitemapindex>',
        encoding="utf-8",
    )
    (dist / "sitemap-0.xml").write_text(
        f'<?xml version="1.0" encoding="UTF-8"?><urlset><url><loc>{base}/</loc></url></urlset>',
        encoding="utf-8",
    )
    for llm in ("llms.txt", "llms-full.txt", "llms-small.txt"):
        (dist / llm).write_text(
            f"# capture-system-or-process-audio\n\nOfficial documentation at {base}/ covering Python, Rust, C++17, and C11.\n",
            encoding="utf-8",
        )
    rust_dir = dist / "api" / "rust" / "capture_system_or_process_audio"
    rust_dir.mkdir(parents=True)
    (rust_dir / "index.html").write_text("<!doctype html><html><head><title>rustdoc</title></head><body id='main-content'></body></html>", encoding="utf-8")

    jsonld = json.dumps(
        {
            "@context": "https://schema.org",
            "@graph": [{"@type": "SoftwareSourceCode", "name": "capture-system-or-process-audio"}],
        }
    )
    (dist / "index.html").write_text(
        f"""<!doctype html>
<html lang="en">
<head>
  <title>CSPA Docs — Cross-Platform Audio Capture</title>
  <meta name="description" content="Cross-platform system speaker loopback, per-process, and microphone PCM audio capture library." />
  <link rel="canonical" href="{base}/" />
  <meta property="og:title" content="CSPA Docs" />
  <meta property="og:description" content="Cross-platform audio capture." />
  <meta property="og:url" content="{base}/" />
  <meta property="og:image" content="{base}/og-card-1280x640.png" />
  <meta name="twitter:card" content="summary_large_image" />
  <script type="application/ld+json">{jsonld}</script>
</head>
<body>
  <h1 id="intro">CSPA Documentation</h1>
  <a href="#intro">Jump</a>
  <a href="/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/index.html#main-content">Rustdoc</a>
</body>
</html>""",
        encoding="utf-8",
    )


class VerifyBuiltSiteTests(unittest.TestCase):
    def test_valid_minimal_dist_passes_with_zero_issues(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cspa-vbs-ok-") as tmp_s:
            dist = pathlib.Path(tmp_s)
            _seed_minimal_valid_dist(dist)
            issues, summary = vbs.verify_dist(dist)
            self.assertEqual(issues, [])
            self.assertEqual(summary["doc_pages_checked"], 1)

    def test_detects_unprefixed_link_and_broken_anchor(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cspa-vbs-badlink-") as tmp_s:
            dist = pathlib.Path(tmp_s)
            _seed_minimal_valid_dist(dist)
            idx = dist / "index.html"
            idx.write_text(
                idx.read_text(encoding="utf-8").replace(
                    '<a href="#intro">Jump</a>',
                    '<a href="/sdks/python/">Unprefixed</a><a href="#missing-id">BadHash</a>',
                ),
                encoding="utf-8",
            )
            issues, _ = vbs.verify_dist(dist)
            codes = {i.code for i in issues}
            self.assertIn("unprefixed_root_link", codes)
            self.assertIn("broken_anchor", codes)

    def test_detects_missing_rustdoc_and_raw_mdx_in_llms_txt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cspa-vbs-llms-") as tmp_s:
            dist = pathlib.Path(tmp_s)
            _seed_minimal_valid_dist(dist)
            (dist / "api" / "rust" / "capture_system_or_process_audio" / "index.html").unlink()
            (dist / "llms-full.txt").write_text(
                "# Docs\n\n" + ("x" * 90) + "\n<SdkTabs><TabItem label='Python'></TabItem></SdkTabs>\n",
                encoding="utf-8",
            )
            issues, _ = vbs.verify_dist(dist)
            codes = {i.code for i in issues}
            self.assertIn("missing_rustdoc", codes)
            self.assertIn("raw_mdx_in_llms", codes)

    def test_detects_broken_rustdoc_deep_link(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cspa-vbs-rustlink-") as tmp_s:
            dist = pathlib.Path(tmp_s)
            _seed_minimal_valid_dist(dist)
            idx = dist / "index.html"
            idx.write_text(
                idx.read_text(encoding="utf-8").replace(
                    "capture_system_or_process_audio/index.html#main-content",
                    "capture_system_or_process_audio/struct.NonExistent.html",
                ),
                encoding="utf-8",
            )
            issues, _ = vbs.verify_dist(dist)
            codes = {i.code for i in issues}
            self.assertIn("broken_internal_link", codes)


if __name__ == "__main__":
    unittest.main()
