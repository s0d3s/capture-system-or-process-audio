#!/usr/bin/env python3
"""CI static site verifier for the Astro Starlight + rustdoc GitHub Pages bundle.

Validates SEO metadata, canonical URLs, OpenGraph/Twitter tags, Schema.org
JSON-LD, internal & `/api/rust/` link resolution, `#anchor` targets,
`sitemap-index.xml`, `robots.txt`, `llms*.txt`, and Pagefind search assets.
"""

from __future__ import annotations

import argparse
import dataclasses
import html.parser
import json
import pathlib
import sys
import urllib.parse
import xml.etree.ElementTree as ET

DEFAULT_SITE_ORIGIN = "https://s0d3s.github.io"
DEFAULT_BASE_PATH = "/capture-system-or-process-audio"


@dataclasses.dataclass(frozen=True)
class SiteIssue:
    code: str
    file: str
    message: str
    line: int | None = None

    def format(self) -> str:
        loc = f"{self.file}:{self.line}" if self.line else self.file
        return f"[{self.code}] {loc}: {self.message}"


@dataclasses.dataclass
class HtmlPageInfo:
    rel_path: str
    title: str = ""
    meta_description: str = ""
    canonical: str = ""
    og_tags: dict[str, str] = dataclasses.field(default_factory=dict)
    twitter_card: str = ""
    h1_count: int = 0
    has_meta_refresh: bool = False
    ids: set[str] = dataclasses.field(default_factory=set)
    links: list[tuple[str, str, int]] = dataclasses.field(default_factory=list)
    missing_alt_imgs: list[tuple[str, int]] = dataclasses.field(default_factory=list)
    jsonld_blocks: list[tuple[str, int]] = dataclasses.field(default_factory=list)


class _HtmlScanner(html.parser.HTMLParser):
    def __init__(self, rel_path: str) -> None:
        super().__init__(convert_charrefs=True)
        self.info = HtmlPageInfo(rel_path=rel_path)
        self._in_title = False
        self._title_parts: list[str] = []
        self._in_jsonld = False
        self._jsonld_line = 1
        self._jsonld_parts: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr_map = {k.lower(): (v or "") for k, v in attrs}
        line_no = self.getpos()[0]

        el_id = attr_map.get("id", "").strip()
        if el_id:
            self.info.ids.add(el_id)
        if tag == "a":
            name_anchor = attr_map.get("name", "").strip()
            if name_anchor:
                self.info.ids.add(name_anchor)

        if tag == "title":
            self._in_title = True
            self._title_parts = []
        elif tag == "h1":
            self.info.h1_count += 1
        elif tag == "meta":
            http_equiv = attr_map.get("http-equiv", "").strip().lower()
            name = attr_map.get("name", "").strip().lower()
            prop = attr_map.get("property", "").strip().lower()
            content = attr_map.get("content", "").strip()
            if http_equiv == "refresh":
                self.info.has_meta_refresh = True
            if name == "description" and not self.info.meta_description:
                self.info.meta_description = content
            elif name == "twitter:card" and not self.info.twitter_card:
                self.info.twitter_card = content
            if prop.startswith("og:"):
                self.info.og_tags[prop] = content
        elif tag == "link":
            rels = {r.lower() for r in attr_map.get("rel", "").split()}
            href = attr_map.get("href", "").strip()
            if "canonical" in rels and not self.info.canonical:
                self.info.canonical = href
            if href and ("stylesheet" in rels or "icon" in rels):
                self.info.links.append(("link", href, line_no))
        elif tag == "a":
            href = attr_map.get("href", "").strip()
            if href:
                self.info.links.append(("a", href, line_no))
        elif tag == "img":
            src = attr_map.get("src", "").strip()
            if src:
                self.info.links.append(("img", src, line_no))
            is_decorative = attr_map.get("aria-hidden", "").lower() == "true" or attr_map.get("role", "").lower() == "presentation"
            if not is_decorative and not attr_map.get("alt", "").strip():
                self.info.missing_alt_imgs.append((src or "<img-without-src>", line_no))
        elif tag == "script":
            stype = attr_map.get("type", "").strip().lower()
            src = attr_map.get("src", "").strip()
            if stype == "application/ld+json":
                self._in_jsonld = True
                self._jsonld_line = line_no
                self._jsonld_parts = []
            elif src:
                self.info.links.append(("script", src, line_no))

    def handle_endtag(self, tag: str) -> None:
        if tag == "title" and self._in_title:
            self._in_title = False
            self.info.title = " ".join("".join(self._title_parts).split())
        elif tag == "script" and self._in_jsonld:
            self._in_jsonld = False
            self.info.jsonld_blocks.append(("".join(self._jsonld_parts).strip(), self._jsonld_line))

    def handle_data(self, data: str) -> None:
        if self._in_title:
            self._title_parts.append(data)
        if self._in_jsonld:
            self._jsonld_parts.append(data)


def _normalize_base_path(base_path: str) -> str:
    clean = "/" + base_path.strip("/")
    return "" if clean == "/" else clean


def _expected_canonical(rel_html: str, site_origin: str, base_path: str) -> str:
    prefix = f"{site_origin.rstrip('/')}{base_path}"
    if rel_html == "index.html":
        return f"{prefix}/"
    if rel_html.endswith("/index.html"):
        sub = rel_html[: -len("/index.html")]
        return f"{prefix}/{sub}/"
    return f"{prefix}/{rel_html}"


def _resolve_target(
    dist_dir: pathlib.Path,
    source_rel_html: str,
    raw_href: str,
    base_path: str,
) -> tuple[pathlib.Path | None, str, str | None]:
    parsed = urllib.parse.urlsplit(raw_href)
    path_part = urllib.parse.unquote(parsed.path)
    fragment = urllib.parse.unquote(parsed.fragment)

    if not path_part:
        return dist_dir / source_rel_html, fragment, None

    if path_part.startswith("/"):
        if base_path and path_part != base_path and not path_part.startswith(base_path + "/"):
            return None, fragment, "unprefixed_root_link"
        rel_str = path_part[len(base_path) :].lstrip("/") if base_path else path_part.lstrip("/")
        candidate_base = dist_dir / rel_str if rel_str else dist_dir
    else:
        parent_url_dir = pathlib.PurePosixPath(source_rel_html).parent
        joined = pathlib.PurePosixPath(parent_url_dir / path_part)
        parts: list[str] = []
        for part in joined.parts:
            if part in ("", "."):
                continue
            if part == "..":
                if not parts:
                    return None, fragment, "escaped_dist_root"
                parts.pop()
            else:
                parts.append(part)
        candidate_base = dist_dir.joinpath(*parts) if parts else dist_dir

    if candidate_base.is_file():
        return candidate_base, fragment, None
    if candidate_base.is_dir():
        idx = candidate_base / "index.html"
        if idx.is_file():
            return idx, fragment, None
    html_alt = dist_dir / f"{candidate_base.relative_to(dist_dir).as_posix()}.html" if candidate_base != dist_dir else None
    if html_alt and html_alt.is_file():
        return html_alt, fragment, None

    return None, fragment, "broken_internal_link"


def _validate_jsonld(data: object) -> str | None:
    allowed = {"SoftwareSourceCode", "SoftwareApplication", "TechArticle", "FAQPage", "WebSite", "WebPage", "BreadcrumbList"}
    items = data if isinstance(data, list) else [data]
    if not items:
        return "Empty JSON-LD array."
    saw_valid = False
    for obj in items:
        if not isinstance(obj, dict):
            return "JSON-LD entry must be an object."
        ctx = obj.get("@context", "")
        if not isinstance(ctx, str) or "schema.org" not in ctx:
            return "JSON-LD missing 'https://schema.org' @context."
        nodes = obj.get("@graph") if isinstance(obj.get("@graph"), list) else [obj]
        for node in nodes:
            if not isinstance(node, dict):
                continue
            raw_t = node.get("@type")
            types = {raw_t} if isinstance(raw_t, str) else (set(raw_t) if isinstance(raw_t, list) else set())
            if types & allowed:
                saw_valid = True
    return None if saw_valid else "No recognized Schema.org @type in JSON-LD."


def verify_dist(
    dist_dir: pathlib.Path,
    *,
    site_origin: str = DEFAULT_SITE_ORIGIN,
    base_path: str = DEFAULT_BASE_PATH,
    require_rustdoc: bool = True,
) -> tuple[list[SiteIssue], dict[str, int]]:
    issues: list[SiteIssue] = []
    if not dist_dir.is_dir():
        return [SiteIssue("missing_dist", str(dist_dir), "Build output directory does not exist.")], {}

    bp = _normalize_base_path(base_path)
    canonical_prefix = f"{site_origin.rstrip('/')}{bp}/"

    robots = dist_dir / "robots.txt"
    if not robots.is_file():
        issues.append(SiteIssue("missing_robots_txt", "robots.txt", "Missing robots.txt."))
    elif f"{canonical_prefix}sitemap" not in robots.read_text(encoding="utf-8", errors="replace"):
        issues.append(SiteIssue("invalid_robots_sitemap", "robots.txt", f"robots.txt missing Sitemap under {canonical_prefix}."))

    sitemap_index = dist_dir / "sitemap-index.xml"
    sitemap_urls: list[str] = []
    if not sitemap_index.is_file():
        issues.append(SiteIssue("missing_sitemap", "sitemap-index.xml", "Missing sitemap-index.xml."))
    else:
        root_el = ET.fromstring(sitemap_index.read_text(encoding="utf-8"))
        locs = [el.text.strip() for el in root_el.iter() if el.tag.endswith("loc") and el.text]
        for loc in locs:
            if not loc.startswith(canonical_prefix):
                issues.append(SiteIssue("invalid_sitemap_loc", "sitemap-index.xml", f"Invalid <loc>: {loc}"))
                continue
            sub_rel = loc[len(canonical_prefix) :]
            if sub_rel.endswith(".xml"):
                sub_xml = dist_dir / sub_rel
                if not sub_xml.is_file():
                    issues.append(SiteIssue("missing_child_sitemap", sub_rel, "Missing child sitemap XML."))
                else:
                    sub_root = ET.fromstring(sub_xml.read_text(encoding="utf-8"))
                    sitemap_urls.extend(u.text.strip() for u in sub_root.iter() if u.tag.endswith("loc") and u.text)
            else:
                sitemap_urls.append(loc)

    for su in sitemap_urls:
        if not su.startswith(canonical_prefix):
            issues.append(SiteIssue("invalid_sitemap_url", "sitemap-index.xml", f"Invalid URL: {su}"))
            continue
        rel_u = su[len(canonical_prefix) :]
        target_p = (dist_dir / rel_u / "index.html") if (not rel_u or rel_u.endswith("/")) else (dist_dir / rel_u)
        if not target_p.is_file():
            issues.append(SiteIssue("dead_sitemap_url", "sitemap-index.xml", f"Sitemap URL missing HTML file: {su}"))

    for llm_name in ("llms.txt", "llms-full.txt", "llms-small.txt"):
        lp = dist_dir / llm_name
        if not lp.is_file():
            issues.append(SiteIssue("missing_llms_txt", llm_name, f"Missing {llm_name}."))
        else:
            txt = lp.read_text(encoding="utf-8", errors="replace").strip()
            if len(txt) < 80:
                issues.append(SiteIssue("short_llms_txt", llm_name, f"{llm_name} is unexpectedly short."))
            if "<SdkTabs" in txt or "<TabItem" in txt:
                issues.append(SiteIssue("raw_mdx_in_llms", llm_name, f"{llm_name} contains uncompiled MDX component tags."))

    pf_dir = dist_dir / "pagefind"
    if not pf_dir.is_dir() or not any(pf_dir.glob("pagefind*.js")):
        issues.append(SiteIssue("missing_pagefind", "pagefind", "Missing Pagefind search bundle."))

    if require_rustdoc:
        rd = dist_dir / "api" / "rust" / "capture_system_or_process_audio" / "index.html"
        if not rd.is_file():
            issues.append(SiteIssue("missing_rustdoc", "api/rust/capture_system_or_process_audio/index.html", "Mounted cargo doc output missing."))

    pages: dict[str, HtmlPageInfo] = {}
    for html_file in sorted(dist_dir.rglob("*.html")):
        rel_html = html_file.relative_to(dist_dir).as_posix()
        scanner = _HtmlScanner(rel_html)
        scanner.feed(html_file.read_text(encoding="utf-8", errors="replace"))
        pages[rel_html] = scanner.info

    if "index.html" not in pages:
        issues.append(SiteIssue("missing_root_index", "index.html", "Missing root index.html."))

    seen_titles: dict[str, str] = {}
    doc_pages = 0

    for rel_html, info in pages.items():
        is_rustdoc = rel_html.startswith("api/rust/")
        is_google_verification = (
            "/" not in rel_html
            and rel_html.startswith("google")
            and rel_html.endswith(".html")
            and (dist_dir / rel_html).read_text(encoding="utf-8", errors="replace").strip()
            == f"google-site-verification: {rel_html}"
        )
        is_aux = is_rustdoc or is_google_verification or rel_html == "404.html" or rel_html.endswith("/404.html") or info.has_meta_refresh

        if not is_aux:
            doc_pages += 1
            if not info.title or len(info.title) > 80:
                issues.append(SiteIssue("invalid_title", rel_html, f"Invalid or overlong <title>: '{info.title}'"))
            elif info.title in seen_titles:
                issues.append(SiteIssue("duplicate_title", rel_html, f"Duplicate <title> with {seen_titles[info.title]}."))
            else:
                seen_titles[info.title] = rel_html

            desc_len = len(info.meta_description.strip())
            if desc_len < 40 or desc_len > 180:
                issues.append(SiteIssue("invalid_meta_description", rel_html, f"<meta name='description'> length {desc_len} outside 40..180."))

            exp_can = _expected_canonical(rel_html, site_origin, bp)
            if info.canonical != exp_can:
                issues.append(SiteIssue("invalid_canonical", rel_html, f"Expected canonical '{exp_can}', got '{info.canonical}'."))

            for og_key in ("og:title", "og:description", "og:url", "og:image"):
                if not info.og_tags.get(og_key, "").strip():
                    issues.append(SiteIssue("missing_og_tag", rel_html, f"Missing {og_key}."))
            og_img = info.og_tags.get("og:image", "")
            if og_img.startswith(canonical_prefix):
                og_rel = og_img[len(canonical_prefix) :]
                if not (dist_dir / og_rel).is_file():
                    issues.append(SiteIssue("missing_og_image", rel_html, f"Missing og:image file {og_rel}."))

            if not info.twitter_card:
                issues.append(SiteIssue("missing_twitter_card", rel_html, "Missing twitter:card meta tag."))
            if info.h1_count != 1:
                issues.append(SiteIssue("invalid_h1_count", rel_html, f"Expected 1 <h1>, found {info.h1_count}."))
            for img_src, ln in info.missing_alt_imgs:
                issues.append(SiteIssue("missing_img_alt", rel_html, f"Image '{img_src}' missing alt text.", line=ln))

            if not info.jsonld_blocks:
                issues.append(SiteIssue("missing_jsonld", rel_html, "Missing JSON-LD script block."))
            for raw_ld, ln in info.jsonld_blocks:
                try:
                    ld_err = _validate_jsonld(json.loads(raw_ld))
                    if ld_err:
                        issues.append(SiteIssue("invalid_jsonld", rel_html, ld_err, line=ln))
                except json.JSONDecodeError as exc:
                    issues.append(SiteIssue("malformed_jsonld", rel_html, exc.msg, line=ln))

        if not is_rustdoc:
            for kind, href, ln in info.links:
                if href.startswith(("mailto:", "tel:", "data:", "javascript:", "//")):
                    continue
                if href.startswith(("http://", "https://")):
                    if href.startswith(canonical_prefix):
                        href = f"{bp}/{href[len(canonical_prefix):]}"
                    else:
                        continue
                if href.startswith("#"):
                    frag = urllib.parse.unquote(href[1:])
                    if frag and frag not in ("top", "_top") and frag not in info.ids:
                        issues.append(SiteIssue("broken_anchor", rel_html, f"Missing anchor '#{frag}'.", line=ln))
                    continue

                resolved_f, frag, err = _resolve_target(dist_dir, rel_html, href, bp)
                if err is not None or resolved_f is None:
                    issues.append(SiteIssue(err or "broken_link", rel_html, f"Unresolved {kind} '{href}'.", line=ln))
                    continue
                if frag and resolved_f.suffix.lower() == ".html":
                    target_rel = resolved_f.relative_to(dist_dir).as_posix()
                    t_info = pages.get(target_rel)
                    if t_info and frag not in ("top", "_top") and frag not in t_info.ids:
                        issues.append(SiteIssue("broken_cross_anchor", rel_html, f"Missing '#{frag}' in {target_rel}.", line=ln))

    summary = {
        "total_html_files": len(pages),
        "doc_pages_checked": doc_pages,
        "sitemap_urls_checked": len(sitemap_urls),
    }
    return issues, summary


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Verify built Astro Starlight + rustdoc GitHub Pages bundle.")
    parser.add_argument("--dist", type=pathlib.Path, required=True, help="Path to website/dist directory.")
    parser.add_argument("--site-origin", default=DEFAULT_SITE_ORIGIN, help="Expected canonical site origin.")
    parser.add_argument("--base", default=DEFAULT_BASE_PATH, help="Expected GitHub Pages base path.")
    parser.add_argument("--no-require-rustdoc", action="store_true", help="Skip /api/rust/ requirement.")
    args = parser.parse_args(argv)

    issues, summary = verify_dist(
        args.dist,
        site_origin=args.site_origin,
        base_path=args.base,
        require_rustdoc=not args.no_require_rustdoc,
    )
    if issues:
        for item in issues[:50]:
            print(item.format(), file=sys.stderr)
        print(f"FAILED: {len(issues)} issue(s) found in {args.dist}", file=sys.stderr)
        return 1

    print(f"OK: verify_built_site passed ({json.dumps(summary)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
