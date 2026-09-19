import hashlib, os, pathlib, shutil

from config import PUBLISHED_CRATES, TARGETS, WHEEL_TAGS

raw = pathlib.Path("raw-artifacts")
out = pathlib.Path("release-assets")
out.mkdir(parents=True, exist_ok=True)

seen = {}
for f in sorted(raw.rglob("*")):
    if not f.is_file():
        continue
    if f.name in seen:
        raise SystemExit(f"Duplicate artifact filename collision: {f.name} ({f} vs {seen[f.name]})")
    seen[f.name] = f
    shutil.copy2(f, out / f.name)

crates = sorted(out.glob("*.crate"))
wheels = sorted(out.glob("*.whl"))
sdists = sorted(out.glob("capture_system_or_process_audio-*.tar.gz"))
ffis = sorted(out.glob("cspa-ffi-*"))
demos = sorted(out.glob("cspa-tui-demo-*"))

assert len(crates) == len(PUBLISHED_CRATES), f"Expected {len(PUBLISHED_CRATES)} .crate packages, got {len(crates)}"
assert len(sdists) == 1, f"Expected 1 Python sdist, got {len(sdists)}"
assert len(wheels) == len(WHEEL_TAGS) * len(TARGETS), f"Unexpected Python wheel count: {len(wheels)}"
assert len(ffis) == len(TARGETS), f"Unexpected FFI SDK archive count: {len(ffis)}"
assert len(demos) == len(TARGETS), f"Unexpected TUI demo archive count: {len(demos)}"

checksum_lines = []
summary_rows = []
for asset in sorted(out.iterdir(), key=lambda p: p.name):
    digest = hashlib.sha256(asset.read_bytes()).hexdigest()
    checksum_lines.append(f"{digest}  {asset.name}")
    summary_rows.append(f"| `{asset.name}` | {asset.stat().st_size:,} B | `{digest}` |")

(out / "SHA256SUMS.txt").write_text("\n".join(checksum_lines) + "\n", encoding="utf-8")

mode_label = "Tag Release" if os.environ["IS_RELEASE"] == "true" else "Manual Dry-Run"
summary_md = (
    f"### {mode_label} Artifact Inventory (`{os.environ['RELEASE_TAG']}`)\n\n"
    f"Verified **{len(summary_rows)}** artifacts (`{len(crates)}` Rust `.crate`s, `{len(wheels)}` Python wheels, "
    f"`{len(sdists)}` Python `sdist`, `{len(ffis)}` C/C++ FFI SDKs, `{len(demos)}` TUI Studio Demo binaries) + `SHA256SUMS.txt`.\n\n"
    "| Artifact | Size | SHA-256 |\n"
    "| --- | ---: | --- |\n"
    + "\n".join(summary_rows)
    + "\n"
)
with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as sf:
    sf.write(summary_md)
