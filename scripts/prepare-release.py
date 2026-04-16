from __future__ import annotations

import os
import re
import sys
from pathlib import Path


VERSION_PATTERN = re.compile(r"^\d+\.\d+\.\d+$")
PACKAGE_NAME = "puppy_find"


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def normalize_version(value: str) -> str:
    version = value.strip()
    if version.startswith("v") and VERSION_PATTERN.fullmatch(version[1:]):
        return version[1:]
    if VERSION_PATTERN.fullmatch(version):
        return version
    fail(f"invalid release version: {value!r}")


def read_cargo_version(cargo_toml: Path) -> str:
    text = cargo_toml.read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
    if not match:
        fail(f"failed to read version from {cargo_toml}")
    return match.group(1)


def read_lock_version(cargo_lock: Path, package_name: str) -> str:
    text = cargo_lock.read_text(encoding="utf-8")
    match = re.search(
        rf'(?ms)^\[\[package\]\]\nname = "{re.escape(package_name)}"\nversion = "([^"]+)"',
        text,
    )
    if not match:
        fail(f"failed to read {package_name} version from {cargo_lock}")
    return match.group(1)


def render_body(template_path: Path, version: str) -> str:
    return template_path.read_text(encoding="utf-8").replace("VERSION", version)


def write_output(name: str, value: str) -> None:
    output_path = os.environ.get("GITHUB_OUTPUT")
    if not output_path:
        print(f"{name}={value}")
        return

    with open(output_path, "a", encoding="utf-8", newline="\n") as handle:
        if "\n" in value:
            handle.write(f"{name}<<__EOF__\n{value}\n__EOF__\n")
        else:
            handle.write(f"{name}={value}\n")


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: python scripts/prepare-release.py <version>")

    repo_root = Path(__file__).resolve().parents[1]
    version = normalize_version(sys.argv[1])
    cargo_version = read_cargo_version(repo_root / "Cargo.toml")
    lock_version = read_lock_version(repo_root / "Cargo.lock", PACKAGE_NAME)

    if cargo_version != version:
        fail(
            f"tag version {version} does not match Cargo.toml version {cargo_version}"
        )
    if lock_version != version:
        fail(
            f"tag version {version} does not match Cargo.lock version {lock_version}"
        )

    body = render_body(repo_root / ".github" / "release_template.md", version)
    write_output("version", version)
    write_output("body", body)


if __name__ == "__main__":
    main()
