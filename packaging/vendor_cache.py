#!/usr/bin/env python3
"""Stage immutable path dependencies for the macOS CI cache and verify them after restore."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys

CASES = ("control", "producer", "consumer")


def digest_tree(root):
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("vendor symlinks require an explicit cache policy")
        mode = stat.S_IMODE(path.stat().st_mode)
        name = path.relative_to(root).as_posix().encode()
        body = path.read_bytes() if path.is_file() else b""
        kind = b"f" if path.is_file() else b"d"
        digest.update(kind + mode.to_bytes(4, "big") + len(name).to_bytes(8, "big") + name)
        digest.update(len(body).to_bytes(8, "big") + body)
    return digest.hexdigest()


def patches(root):
    text = (root / "Cargo.toml").read_text()
    section = text.split("[patch.crates-io]\n", 1)[1].split("\n[", 1)[0]
    result = {}
    for line in section.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        match = re.fullmatch(r'([\w-]+)\s*=\s*\{\s*path\s*=\s*"vendor/([\w-]+)"\s*\}', line)
        if not match or match[1] in result:
            raise ValueError("unsupported or duplicate vendor patch declaration")
        result[match[1]] = match[2]
    if not result:
        raise ValueError("no vendor patches")
    return result


def stage(root, cargo_home, vendor_root, directory, case):
    root, cargo_home, vendor_root = root.resolve(), cargo_home.resolve(), vendor_root.resolve()
    for outside in (cargo_home, vendor_root):
        if outside == root or root in outside.parents:
            raise ValueError("Cargo home and staged vendor must be outside the source checkout")
    mapping = patches(root)
    vendor_hash = digest_tree(root / "vendor")
    destination = vendor_root / vendor_hash / "vendor"
    config_hash = None
    if case != "control":
        if destination.exists():
            if digest_tree(destination) != vendor_hash:
                raise ValueError("existing staged vendor content or modes differ")
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(root / "vendor", destination, copy_function=shutil.copy2)
        config = "# Generated CI-only vendor cache configuration.\n[patch.crates-io]\n"
        config += "".join(f'{name} = {{ path = {json.dumps(str(destination / relative))} }}\n'
                          for name, relative in sorted(mapping.items()))
        cargo_home.mkdir(parents=True, exist_ok=True)
        config_path = cargo_home / "config.toml"
        if (cargo_home / "config").exists() or (config_path.exists() and config_path.read_text() != config):
            raise ValueError("refusing to replace an existing Cargo home configuration")
        config_path.write_text(config)
        config_hash = hashlib.sha256(config.encode()).hexdigest()
    directory.mkdir(parents=True, exist_ok=True)
    record = {"case": case, "vendor_hash": vendor_hash, "patches": mapping,
              "vendor_directory": str(destination), "cargo_home": str(cargo_home), "config_sha256": config_hash}
    (directory / "vendor.json").write_text(json.dumps(record, indent=2) + "\n")
    return record


def verify_stage(directory):
    record = json.loads((directory / "vendor.json").read_text())
    if record["case"] != "control":
        if digest_tree(Path(record["vendor_directory"])) != record["vendor_hash"]:
            raise ValueError("restored vendor differs from tracked source content or modes")
        config = Path(record["cargo_home"]) / "config.toml"
        if hashlib.sha256(config.read_bytes()).hexdigest() != record["config_sha256"]:
            raise ValueError("external Cargo configuration changed")
    if digest_tree(Path("vendor")) != record["vendor_hash"]:
        raise ValueError("tracked vendor changed")
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("stage", "verify"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--case", choices=CASES)
    parser.add_argument("--cargo-home", type=Path)
    parser.add_argument("--vendor-root", type=Path)
    args = parser.parse_args()
    if args.action == "stage":
        if not all((args.case, args.cargo_home, args.vendor_root)):
            parser.error("stage requires case, cargo-home and vendor-root")
        record = stage(Path.cwd(), args.cargo_home, args.vendor_root, args.directory, args.case)
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"vendor-hash={record['vendor_hash']}\nvendor-directory={record['vendor_directory']}\n")
            if sys.platform == "darwin":
                sdk = subprocess.check_output(["xcrun", "--show-sdk-version"], text=True, timeout=30).strip()
                output.write(f"sdk={sdk}\nimage={os.environ.get('ImageOS', 'macos')}-{os.environ.get('ImageVersion', 'local')}\n")
    else:
        verify_stage(args.directory)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"vendor-cache: {error}", file=sys.stderr)
        raise SystemExit(1) from error
