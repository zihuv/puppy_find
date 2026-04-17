from __future__ import annotations

import argparse
import os
import shutil
import sys
import tarfile
import zipfile
from pathlib import Path


MODEL_REPO_URL = "https://huggingface.co/zihuv/chinese-clip-vit-base-patch16-onnx"


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Create portable PuppyFind release archives.")
    parser.add_argument("--binary-path", required=True)
    parser.add_argument("--platform", required=True, choices=["windows", "linux", "macos"])
    parser.add_argument("--package-id", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--flavor", required=True, choices=["nomodel", "model"])
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--config-dir", default="config")
    parser.add_argument("--model-source-dir")
    return parser.parse_args()


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8", newline="\n")


def copy_tree_contents(source: Path, destination: Path) -> None:
    for child in source.iterdir():
        if child.name == ".cache":
            continue
        target = destination / child.name
        if child.is_dir():
            shutil.copytree(child, target, dirs_exist_ok=True)
        else:
            shutil.copy2(child, target)

def create_zip(source_dir: Path, archive_path: Path) -> None:
    if archive_path.exists():
        archive_path.unlink()

    with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for file_path in sorted(source_dir.rglob("*")):
            archive.write(file_path, file_path.relative_to(source_dir))


def create_tar_gz(base_dir: Path, folder_name: str, archive_path: Path) -> None:
    if archive_path.exists():
        archive_path.unlink()

    with tarfile.open(archive_path, "w:gz") as archive:
        archive.add(base_dir / folder_name, arcname=folder_name)

def build_bundle(args: argparse.Namespace) -> Path:
    repo_root = Path(__file__).resolve().parents[1]
    binary = Path(args.binary_path).resolve()
    if not binary.is_file():
        fail(f"binary not found: {binary}")

    output_root = Path(args.output_dir).resolve()
    output_root.mkdir(parents=True, exist_ok=True)

    staging_root = repo_root / ".dist"
    staging_root.mkdir(parents=True, exist_ok=True)

    bundle_name = f"puppy_find-{args.version}-{args.package_id}-{args.flavor}"
    bundle_root = staging_root / bundle_name
    if bundle_root.exists():
        shutil.rmtree(bundle_root)
    bundle_root.mkdir(parents=True)

    binary_name = binary.name
    shutil.copy2(binary, bundle_root / binary_name)

    config_root = bundle_root / args.config_dir
    config_root.mkdir(parents=True, exist_ok=True)

    model_dir = config_root / "model"
    model_dir.mkdir()
    (config_root / "log").mkdir()
    (bundle_root / "materials").mkdir()

    if args.flavor == "model":
        if not args.model_source_dir:
            fail("ModelSourceDir is required when flavor=model")
        model_source_dir = Path(args.model_source_dir).resolve()
        if not model_source_dir.is_dir():
            fail(f"model source dir not found: {model_source_dir}")
        copy_tree_contents(model_source_dir, model_dir)
        write_text(
            model_dir / "MODEL_INFO.txt",
            "This package already includes the Hugging Face model bundle:\n"
            "zihuv/chinese-clip-vit-base-patch16-onnx\n",
        )
    else:
        write_text(
            model_dir / "PUT_MODEL_HERE.txt",
            "Download the Hugging Face repository below into this folder without adding an extra nested directory:\n"
            f"{MODEL_REPO_URL}\n\n"
            "Expected result:\n"
            "  ./config/model/model_config.json\n"
            "  ./config/model/text.onnx\n"
            "  ./config/model/visual.onnx\n"
            "  ./config/model/vocab.txt\n",
        )

    write_text(
        config_root / "README.txt",
        "Runtime configuration is generated automatically on first launch.\n"
        "Files and folders in this directory:\n"
        "  ./.env          generated local configuration\n"
        "  ./model/        local model bundle\n"
        "  ./log/          application log files\n",
    )

    if args.platform == "windows":
        archive_path = output_root / f"{bundle_name}.zip"
        create_zip(bundle_root, archive_path)
    else:
        archive_path = output_root / f"{bundle_name}.tar.gz"
        create_tar_gz(staging_root, bundle_name, archive_path)

    return archive_path


def main() -> None:
    args = parse_args()
    archive_path = build_bundle(args)
    print(os.fspath(archive_path))


if __name__ == "__main__":
    main()
