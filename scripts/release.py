from __future__ import annotations

import argparse
import re
import subprocess
import sys
from datetime import date
from pathlib import Path


VERSION_PATTERN = re.compile(r"^\d+\.\d+\.\d+$")
PACKAGE_NAME = "puppy_find"
CHANGELOG_CATEGORIES = (
    "Added",
    "Changed",
    "Deprecated",
    "Removed",
    "Fixed",
    "Security",
)


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def run(command: list[str], cwd: Path) -> str:
    completed = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        output = "\n".join(
            part.strip() for part in (completed.stdout, completed.stderr) if part.strip()
        )
        fail(output or f"command failed: {' '.join(command)}")
    return completed.stdout.strip()


def git(repo_root: Path, *args: str) -> str:
    return run(["git", *args], cwd=repo_root)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Update Cargo.toml, Cargo.lock, docs/CHANGELOG.md, create a git tag, and push the release."
    )
    parser.add_argument("version", help="release version, for example 0.1.1")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the plan without changing files or running git write operations",
    )
    parser.add_argument(
        "--no-push",
        action="store_true",
        help="create the commit and tag locally without pushing",
    )
    return parser.parse_args()


def ensure_valid_version(version: str) -> None:
    if not VERSION_PATTERN.fullmatch(version):
        fail(f"invalid version {version!r}, expected x.y.z")


def read_cargo_version(cargo_toml: Path) -> str:
    text = cargo_toml.read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
    if not match:
        fail(f"failed to read version from {cargo_toml}")
    return match.group(1)


def write_cargo_version(cargo_toml: Path, version: str) -> None:
    text = cargo_toml.read_text(encoding="utf-8")
    updated, count = re.subn(
        r'(?m)^version\s*=\s*"([^"]+)"',
        f'version = "{version}"',
        text,
        count=1,
    )
    if count != 1:
        fail(f"failed to update version in {cargo_toml}")
    cargo_toml.write_text(updated, encoding="utf-8", newline="\n")


def read_lock_version(cargo_lock: Path, package_name: str) -> str:
    text = cargo_lock.read_text(encoding="utf-8")
    match = re.search(
        rf'(?ms)^\[\[package\]\]\nname = "{re.escape(package_name)}"\nversion = "([^"]+)"',
        text,
    )
    if not match:
        fail(f"failed to read {package_name} version from {cargo_lock}")
    return match.group(1)


def write_lock_version(cargo_lock: Path, package_name: str, version: str) -> None:
    text = cargo_lock.read_text(encoding="utf-8")
    updated, count = re.subn(
        rf'(?ms)^(\[\[package\]\]\nname = "{re.escape(package_name)}"\nversion = )"[^"]+"',
        rf'\1"{version}"',
        text,
        count=1,
    )
    if count != 1:
        fail(f"failed to update {package_name} version in {cargo_lock}")
    cargo_lock.write_text(updated, encoding="utf-8", newline="\n")


def build_empty_unreleased_section() -> str:
    lines = ["## [Unreleased]", ""]
    for category in CHANGELOG_CATEGORIES:
        lines.append(f"### {category}")
        lines.append("")
    return "\n".join(lines).rstrip()


def extract_unreleased_section(text: str) -> tuple[str, str, str]:
    match = re.search(
        r"(?ms)^## \[Unreleased\]\n(?P<body>.*?)(?=^## \[|\Z)",
        text,
    )
    if not match:
        fail("failed to find ## [Unreleased] section in docs/CHANGELOG.md")
    return text[: match.start()], match.group("body"), text[match.end() :]


def ensure_release_notes_exist(unreleased_body: str) -> None:
    if not re.search(r"(?m)^- ", unreleased_body):
        fail("docs/CHANGELOG.md has no unreleased entries to publish")


def ensure_changelog_version_does_not_exist(changelog_text: str, version: str) -> None:
    if re.search(rf"(?m)^## \[{re.escape(version)}\](?:\s+-\s+\d{{4}}-\d{{2}}-\d{{2}})?$", changelog_text):
        fail(f"docs/CHANGELOG.md already contains version {version}")


def write_changelog_version(changelog_path: Path, version: str, release_date: date) -> None:
    text = changelog_path.read_text(encoding="utf-8")
    ensure_changelog_version_does_not_exist(text, version)
    before, unreleased_body, after = extract_unreleased_section(text)
    ensure_release_notes_exist(unreleased_body)

    release_notes = unreleased_body.strip()
    if not release_notes:
        fail("docs/CHANGELOG.md has no unreleased entries to publish")

    updated = (
        before.rstrip()
        + "\n\n"
        + build_empty_unreleased_section()
        + "\n\n"
        + f"## [{version}] - {release_date.isoformat()}\n\n"
        + release_notes
    )

    trailing = after.lstrip("\n")
    if trailing:
        updated += "\n\n" + trailing
    else:
        updated += "\n"

    changelog_path.write_text(updated, encoding="utf-8", newline="\n")


def ensure_clean_worktree(repo_root: Path) -> None:
    status = git(repo_root, "status", "--short")
    if status:
        fail(f"working tree is not clean:\n{status}")


def ensure_tag_does_not_exist(repo_root: Path, version: str) -> None:
    existing = git(repo_root, "tag", "--list", version)
    if existing:
        fail(f"git tag already exists: {version}")


def ensure_upstream_exists(repo_root: Path) -> None:
    try:
        git(repo_root, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}")
    except SystemExit:
        fail("current branch has no upstream branch; use --no-push or set upstream first")


def print_plan(version: str, no_push: bool) -> None:
    print("Release plan:")
    print(f"- update Cargo.toml, Cargo.lock, and docs/CHANGELOG.md to {version}")
    print("- move docs/CHANGELOG.md unreleased notes into a versioned release section")
    print("- git add Cargo.toml Cargo.lock docs/CHANGELOG.md")
    print(f'- git commit -m "{version}"')
    print(f'- git tag -a {version} -m "{version}"')
    if not no_push:
        print("- git push --follow-tags")


def main() -> None:
    args = parse_args()
    version = args.version.strip()
    ensure_valid_version(version)

    repo_root = Path(__file__).resolve().parents[1]
    cargo_toml = repo_root / "Cargo.toml"
    cargo_lock = repo_root / "Cargo.lock"
    changelog = repo_root / "docs" / "CHANGELOG.md"
    current_version = read_cargo_version(cargo_toml)
    lock_version = read_lock_version(cargo_lock, PACKAGE_NAME)

    if current_version == version:
        fail(f"version is already {version}")
    if current_version != lock_version:
        fail(
            f"Cargo.toml version {current_version} does not match Cargo.lock version {lock_version}"
        )

    ensure_clean_worktree(repo_root)
    ensure_tag_does_not_exist(repo_root, version)
    if not args.no_push:
        ensure_upstream_exists(repo_root)

    if args.dry_run:
        print_plan(version, args.no_push)
        return

    write_cargo_version(cargo_toml, version)
    write_lock_version(cargo_lock, PACKAGE_NAME, version)
    write_changelog_version(changelog, version, date.today())

    git(repo_root, "add", "Cargo.toml", "Cargo.lock", "docs/CHANGELOG.md")
    git(repo_root, "commit", "-m", version)
    git(repo_root, "tag", "-a", version, "-m", version)

    if not args.no_push:
        git(repo_root, "push", "--follow-tags")


if __name__ == "__main__":
    main()
