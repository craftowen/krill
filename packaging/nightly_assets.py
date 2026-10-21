#!/usr/bin/env python3
"""같은 후보 HEAD에서 빌드·스모크한 Windows/Linux 산출물만 게시 디렉터리로 가져온다."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import zipfile


def import_assets(directory, output, version, commit, public_key):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("invalid release version")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("invalid source commit")
    if not re.fullmatch(r"[0-9a-f]{64}", public_key):
        raise ValueError("invalid update public key")
    archives = [f"Krill-{version}-windows-x64.zip"] + [
        f"krill-{version}-linux-{arch}.tar.gz" for arch in ("x86_64", "aarch64")]
    sources = [directory / f"source-commit-{platform}.txt" for platform in ("windows", "x86_64", "aarch64")]
    for source in sources:
        if source.is_symlink() or source.read_text().strip() != commit:
            raise ValueError(f"{source.name} artifact source does not match {commit}")
    # 모두 검증한 뒤 복사한다. 체크섬 파일의 이름을 입력 경로로 쓰지 않는다.
    files = list(sources)
    for archive in archives:
        path = directory / archive
        checksum = directory / f"{archive}.sha256"
        if path.is_symlink() or checksum.is_symlink():
            raise ValueError("artifact symlinks are forbidden")
        parts = checksum.read_text().split()
        if len(parts) != 2 or parts[1].lstrip("*") != archive:
            raise ValueError(f"invalid checksum filename: {archive}")
        if parts[0].lower() != hashlib.sha256(path.read_bytes()).hexdigest():
            raise ValueError(f"artifact checksum mismatch: {archive}")
        files.extend((path, checksum))
    with zipfile.ZipFile(directory / archives[0]) as archive:
        info = json.loads(archive.read("Krill/Krill.json").decode("utf-8-sig"))
        if info.get("CFBundleShortVersionString") != version:
            raise ValueError("Windows version does not match")
        if info.get("KrillUpdatePublicKey") != public_key:
            raise ValueError("Windows update key does not match the macOS signing key")
    output.mkdir(parents=True, exist_ok=True)
    for path in files:
        if (output / path.name).exists():
            raise ValueError(f"artifact already exists: {path.name}")
    for path in files:
        shutil.copyfile(path, output / path.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--public-key", required=True)
    args = parser.parse_args()
    import_assets(args.directory, args.output, args.version, args.commit, args.public_key)


if __name__ == "__main__":
    main()
