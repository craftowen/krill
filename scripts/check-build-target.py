#!/usr/bin/env python3
"""Refuse local macOS Cargo output outside the mounted external build volume."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
TARGETS = Path("/Volumes/krill-build/targets")


def validate_target(target, targets=TARGETS):
    # Resolve symlinks and '..': a spelling below targets/ is not sufficient.
    target = Path(target).resolve()
    allowed = targets.resolve()
    if not targets.parent.is_mount():
        raise ValueError("external build volume is not mounted: " + str(targets.parent))
    if targets.parent.resolve() not in allowed.parents:
        raise ValueError("external targets directory resolves outside its build volume")
    if target == allowed or allowed not in target.parents:
        raise ValueError("Cargo target must be below " + str(targets) + "/<name>: " + str(target))
    return target


def validate_directories(metadata, targets=TARGETS):
    checked = {}
    for key in ("target_directory", "build_directory"):
        # Older Cargo metadata has no separate intermediate-build directory.
        value = metadata.get(key, metadata["target_directory"])
        try:
            checked[key] = validate_target(value, targets)
        except (ValueError, TypeError) as error:
            raise ValueError("invalid Cargo {}: {}".format(key, error)) from error
    return checked


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", help="explicit Cargo target directory to validate")
    args = parser.parse_args()
    # GitHub-hosted runners and native Windows use their platform's build paths.
    if sys.platform != "darwin" or os.environ.get("GITHUB_ACTIONS") == "true":
        print("build-target: local macOS policy does not apply")
        return 0
    try:
        env = dict(os.environ)
        if args.target_dir is not None:
            # Refuse an unsafe override before probing Cargo or build tools.
            env["CARGO_TARGET_DIR"] = str(validate_target(args.target_dir, TARGETS))
        # Metadata resolves both final and intermediate output paths, including
        # CARGO_BUILD_BUILD_DIR/config precedence, without compiling or mkdir.
        data = subprocess.check_output(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked", "--offline"],
            cwd=ROOT, env=env, text=True, timeout=30)
        checked = validate_directories(json.loads(data), TARGETS)
        print("build-target: target={} build={}".format(checked["target_directory"], checked["build_directory"]))
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        print("build-target: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
