import argparse
import pathlib
import re
import tomllib


def generate_cmake_version(manifest: pathlib.Path, template: pathlib.Path, output: pathlib.Path) -> str:
    version = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]["version"]
    match = re.fullmatch(
        r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
        r"(?P<suffix>(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)",
        version,
    )
    if not match:
        raise ValueError(f"Invalid Cargo package version: {version!r}")
    content = template.read_text(encoding="utf-8")
    for token, value in {
        "@CSPA_PACKAGE_VERSION@": version,
        "@CSPA_PACKAGE_VERSION_MAJOR@": match.group(1),
        "@CSPA_PACKAGE_VERSION_MINOR@": match.group(2),
        "@CSPA_PACKAGE_NONNUMERIC@": "TRUE" if match.group("suffix") else "FALSE",
    }.items():
        if content.count(token) != 1:
            raise ValueError(f"Expected exactly one {token} in {template}")
        content = content.replace(token, value)
    output.write_text(content, encoding="utf-8")
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=pathlib.Path)
    parser.add_argument("template", type=pathlib.Path)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    generate_cmake_version(args.manifest, args.template, args.output)