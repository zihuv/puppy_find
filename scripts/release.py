from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


VERSION_PATTERN = re.compile(r"^\d+\.\d+\.\d+$")


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
        description="Update Cargo.toml, create a git tag, and push the release."
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
    print(f"- update Cargo.toml to {version}")
    print("- git add Cargo.toml")
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
    current_version = read_cargo_version(cargo_toml)

    if current_version == version:
        fail(f"version is already {version}")

    ensure_clean_worktree(repo_root)
    ensure_tag_does_not_exist(repo_root, version)
    if not args.no_push:
        ensure_upstream_exists(repo_root)

    if args.dry_run:
        print_plan(version, args.no_push)
        return

    write_cargo_version(cargo_toml, version)

    git(repo_root, "add", "Cargo.toml")
    git(repo_root, "commit", "-m", version)
    git(repo_root, "tag", "-a", version, "-m", version)

    if not args.no_push:
        git(repo_root, "push", "--follow-tags")


if __name__ == "__main__":
    main()
