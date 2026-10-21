#!/usr/bin/env python3
"""같은 후보 HEAD에서 빌드·스모크한 Windows/Linux 산출물만 게시 디렉터리로 가져온다."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import zipfile

from ci_evidence import gh_json


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def download_artifact(repository, run_id, attempt, name, directory, head, read=gh_json, fetch=None):
    pages = read(f"repos/{repository}/actions/runs/{run_id}/artifacts?per_page=100", paginate=True)
    matches = [artifact for page in pages for artifact in page["artifacts"] if artifact["name"] == name]
    if len(matches) != 1:
        raise ValueError("expected one native artifact for the verified attempt")
    artifact = matches[0]
    workflow = artifact.get("workflow_run", {})
    if (artifact.get("expired") or workflow.get("id") != run_id or workflow.get("head_sha") != head
            or not name.endswith(f"-attempt-{attempt}")
            or not re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest", ""))):
        raise ValueError("native artifact identity or digest does not match the verified attempt")
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryFile(dir=directory) as stream:
        if fetch:
            fetch(artifact["id"], stream)
        else:
            subprocess.run(["gh", "api", f"repos/{repository}/actions/artifacts/{artifact['id']}/zip"],
                           stdout=stream, check=True, timeout=900)
        stream.seek(0)
        digest = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
        if artifact["digest"] != "sha256:" + digest.hexdigest():
            raise ValueError("downloaded native artifact digest does not match GitHub evidence")
        stream.seek(0)
        with zipfile.ZipFile(stream) as archive:
            names = set()
            for member in archive.infolist():
                name = member.filename
                if (Path(name).name != name or name in (".", "..") or name in names
                        or stat.S_ISLNK(member.external_attr >> 16) or (directory / name).exists()):
                    raise ValueError("unsafe or duplicate native artifact member")
                names.add(name)
            for member in archive.infolist():
                with archive.open(member) as source, (directory / member.filename).open("xb") as destination:
                    shutil.copyfileobj(source, destination)
    return {"id": artifact["id"], "name": artifact["name"], "digest": artifact["digest"],
            "run_id": run_id, "attempt": attempt, "head": head}


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
        if parts[0].lower() != sha256(path):
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
