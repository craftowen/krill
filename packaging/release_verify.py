#!/usr/bin/env python3
"""게시 계획을 서명하고 실제 원격 태그·자산·업데이트 서명·소스 커밋을 다시 읽어 검증한다."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from ci_evidence import gh_json
from macos import ed25519_manifest as ed
from release_check import sha256


def signed_payload(plan):
    return b"krill-publication-plan-v1\n" + json.dumps(
        {key: value for key, value in plan.items() if key != "signature"}, sort_keys=True, separators=(",", ":")).encode()


def create_plan(directory, head, version, repository, key_file, all_platforms=False):
    if not re.fullmatch(r"[0-9a-f]{40}", head) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("invalid publication SHA or stable version")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid publication repository")
    mac = list(directory.glob(f"Krill-{version}-*-adhoc.zip"))
    if len(mac) != 1:
        raise ValueError("expected exactly one macOS release zip")
    files = [directory / f"Krill-{version}.dmg", mac[0], Path(str(mac[0]) + ".sha256"),
             directory / f"Krill-{version}-source.tar.gz", directory / "homebrew-krill/SHA256SUMS",
             directory / "latest.json", directory / "install.sh"]
    windows = directory / f"Krill-{version}-windows-x64.zip"
    if all_platforms or windows.exists():
        files.extend((windows, Path(str(windows) + ".sha256"), directory / "latest-windows.json", directory / "install.ps1"))
    if any(path.is_symlink() or not path.is_file() for path in files):
        raise ValueError("publication asset inventory is incomplete or contains symlinks")
    plan = {"schema": 1, "head": head, "version": version, "repository": repository,
            "created_at": datetime.now(timezone.utc).isoformat(), "all_platforms": all_platforms,
            "assets": [{"name": path.name, "sha256": sha256(path), "size": path.stat().st_size} for path in files]}
    seed = ed.load_or_create_seed(key_file, create_if_missing=False)
    plan["public_key"] = ed.public_key_from_seed(seed).hex()
    plan["signature"] = ed.ed25519_sign(seed, signed_payload(plan)).hex()
    verify_files(plan, directory, plan["public_key"])
    return plan


def verify_files(plan, directory, public_key):
    if plan.get("schema") != 1 or plan.get("public_key") != public_key or not ed.ed25519_verify(
            bytes.fromhex(public_key), signed_payload(plan), bytes.fromhex(plan.get("signature", ""))):
        raise ValueError("publication plan signature does not match the trusted update key")
    names = [asset["name"] for asset in plan["assets"]]
    if len(names) != len(set(names)) or any(Path(name).name != name or name in (".", "..") for name in names):
        raise ValueError("invalid or duplicate publication asset name")
    for asset in plan["assets"]:
        path = directory / asset["name"]
        if asset["name"] == "SHA256SUMS" and not path.exists():
            path = directory / "homebrew-krill/SHA256SUMS"
        if path.is_symlink() or not path.is_file() or sha256(path) != asset["sha256"] or path.stat().st_size != asset["size"]:
            raise ValueError(f"published asset bytes do not match: {asset['name']}")
    version = plan["version"]
    sums = directory / "SHA256SUMS"
    if not sums.exists():
        sums = directory / "homebrew-krill/SHA256SUMS"
    expected_sums = {f"Krill-{version}.dmg", f"Krill-{version}-source.tar.gz"}
    entries = [line.split() for line in sums.read_text().splitlines()]
    if any(len(parts) != 2 for parts in entries) or {parts[1] for parts in entries} != expected_sums:
        raise ValueError("published source/DMG checksum inventory does not match")
    if any(parts[0] != sha256(directory / parts[1]) for parts in entries):
        raise ValueError("published source/DMG checksum does not match")
    for name in names:
        if name.endswith(".sha256"):
            parts = (directory / name).read_text().split()
            archive = name.removesuffix(".sha256")
            if len(parts) != 2 or parts[1].lstrip("*") != archive or parts[0].lower() != sha256(directory / archive):
                raise ValueError("published archive checksum file does not match")
    for filename in ("latest.json", "latest-windows.json"):
        if filename not in names:
            continue
        manifest = json.loads((directory / filename).read_text())
        payload = ed.canonical_signing_payload(manifest["version"], manifest["url"], manifest["sha256"],
                                               manifest["min_daemon_protocol"], manifest["channel"], manifest["rollout_pct"])
        archive = manifest["url"].rsplit("/", 1)[-1]
        expected_url = f"https://github.com/{plan['repository']}/releases/download/v{version}/{archive}"
        if manifest["version"] != version or manifest["url"] != expected_url or archive not in names:
            raise ValueError("published update URL or version does not match")
        if manifest["sha256"] != sha256(directory / archive) or not ed.ed25519_verify(
                bytes.fromhex(public_key), payload, bytes.fromhex(manifest["ed25519_sig"])):
            raise ValueError("published update signature or zip digest does not match")
    with tarfile.open(directory / f"Krill-{version}-source.tar.gz", "r:gz") as archive:
        if archive.pax_headers.get("comment") != plan["head"]:
            raise ValueError("published source archive does not identify the verified commit")
    mac = next(name for name in names if name.endswith("-adhoc.zip"))
    with zipfile.ZipFile(directory / mac) as archive:
        info = plistlib.loads(archive.read("Krill.app/Contents/Info.plist"))
        if info.get("CFBundleShortVersionString") != version or info.get("KrillUpdatePublicKey") != public_key:
            raise ValueError("published macOS bundle version or update key does not match")
    windows = f"Krill-{version}-windows-x64.zip"
    if windows in names:
        with zipfile.ZipFile(directory / windows) as archive:
            info = json.loads(archive.read("Krill/Krill.json").decode("utf-8-sig"))
            if info.get("CFBundleShortVersionString") != version or info.get("KrillUpdatePublicKey") != public_key:
                raise ValueError("published Windows bundle version or update key does not match")


def verify_public(plan, head, repository, public_key, read=gh_json, download=None, temporary_root=None):
    if plan["head"] != head or plan["repository"] != repository:
        raise ValueError("publication plan does not match the requested source and repository")
    # 원격 메타데이터를 신뢰하기 전에 로컬 서명부터 확인한다.
    if plan.get("public_key") != public_key or not ed.ed25519_verify(
            bytes.fromhex(public_key), signed_payload(plan), bytes.fromhex(plan.get("signature", ""))):
        raise ValueError("publication plan signature is invalid")
    version = plan["version"]
    tag = read(f"repos/{repository}/git/ref/tags/v{version}")["object"]
    for _ in range(4):
        if tag["type"] != "tag":
            break
        tag = read(f"repos/{repository}/git/tags/{tag['sha']}")["object"]
    if tag.get("type") != "commit" or tag.get("sha") != head:
        raise ValueError("published tag does not identify the verified commit")
    release = read(f"repos/{repository}/releases/tags/v{version}")
    expected = {asset["name"] for asset in plan["assets"]}
    if release.get("draft") or release.get("prerelease") or release.get("tag_name") != f"v{version}":
        raise ValueError("release is not the expected published stable version")
    if {asset["name"] for asset in release["assets"]} != expected:
        raise ValueError("published release asset inventory does not match")
    parent = temporary_root or Path("/Volumes/krill-build/nightly/verification")
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="public-", dir=parent) as temporary:
        directory = Path(temporary)
        if download:
            download(directory)
        else:
            subprocess.run(["gh", "release", "download", f"v{version}", "--repo", repository,
                            "--dir", str(directory), "--pattern", "*"], stdout=sys.stderr, check=True, timeout=900)
        verify_files(plan, directory, public_key)
    return {"head": head, "tag": f"v{version}", "verified_assets": sorted(expected),
            "verified_at": datetime.now(timezone.utc).isoformat()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("plan", "verify"))
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--version")
    parser.add_argument("--key-file", type=Path)
    parser.add_argument("--public-key")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--all-platforms", action="store_true")
    args = parser.parse_args()
    if args.action == "plan":
        plan = create_plan(args.directory, args.head, args.version, args.repo, args.key_file, args.all_platforms)
        args.plan.write_text(json.dumps(plan, indent=2) + "\n")
    else:
        result = json.dumps(verify_public(json.loads(args.plan.read_text()), args.head, args.repo, args.public_key), indent=2) + "\n"
        if args.output:
            args.output.write_text(result)
        else:
            print(result, end="")


if __name__ == "__main__":
    main()
