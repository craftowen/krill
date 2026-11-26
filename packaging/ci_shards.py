#!/usr/bin/env python3
"""Single source of truth for CI nextest package shards and completeness verification."""

import argparse
from pathlib import Path
import re
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]

# Disjoint partition of every Cargo workspace member. Splitting by crate group
# avoids compiling GPUI on daemon/core runners and avoids compiling krilld's
# opt-level=2 test suite on gui/core runners.
SHARDS = {
    "gui": (
        "krill-app",
        "krill-app-shell",
        "krill-bots-ui",
        "krill-app-core",
        "krill-ui",
    ),
    "daemon": (
        "krilld",
    ),
    "core": (
        "krill",
        "krill-holder",
        "krill-proto",
        "krill-link",
        "krill-link-core",
        "krill-hosts",
        "krill-term",
        "krill-log",
        "krill-version",
    ),
}

# Sibling binaries looked up in target/debug/ by integration tests outside the
# binary's own package. Built with `--profile test` so they reuse the exact
# `[profile.test]` dependency graph without compiling duplicate dev artifacts.
HELPER_BINS = {
    "gui": ("krill-holder", "krilld", "krill", "krill-link"),
    "daemon": ("krill-holder", "krill"),
    "core": ("krilld",),
}

NAME_RE = re.compile(r'(?ms)^\[package\]\n(?:[^\[]*\n)*?^name\s*=\s*"([^"]+)"')


def workspace_packages(root=ROOT):
    """Return the sorted set of Cargo workspace package names under `crates/*`."""
    root = Path(root)
    cargo_toml = (root / "Cargo.toml").read_text(encoding="utf-8")
    if 'members = ["crates/*"]' not in cargo_toml:
        raise ValueError("expected workspace members = [\"crates/*\"] in root Cargo.toml")
    packages = set()
    for manifest in sorted((root / "crates").glob("*/Cargo.toml")):
        match = NAME_RE.search(manifest.read_text(encoding="utf-8"))
        if not match:
            raise ValueError(f"missing [package] name in {manifest}")
        packages.add(match.group(1))
    if not packages:
        raise ValueError("no workspace packages discovered under crates/")
    return packages


def verify(root=ROOT, shards=SHARDS, helper_bins=HELPER_BINS):
    """Ensure shards form an exact, non-empty, disjoint partition of the workspace."""
    workspace = workspace_packages(root)
    assigned = set()
    duplicates = set()
    for name, packages in shards.items():
        if not packages:
            raise ValueError(f"shard {name!r} must not be empty")
        for pkg in packages:
            if pkg in assigned:
                duplicates.add(pkg)
            assigned.add(pkg)
    if duplicates:
        raise ValueError(f"duplicate packages across CI shards: {sorted(duplicates)}")
    missing = sorted(workspace - assigned)
    unknown = sorted(assigned - workspace)
    if missing or unknown:
        raise ValueError(f"CI shard mismatch: missing={missing}, unknown={unknown}")
    for name, bins in helper_bins.items():
        if name not in shards:
            raise ValueError(f"helper_bins references unknown shard: {name!r}")
        bad_bins = sorted(set(bins) - workspace)
        if bad_bins:
            raise ValueError(f"helper_bins for shard {name!r} has unknown packages: {bad_bins}")
    return workspace


def build_commands(shard, timings=False):
    """Return the helper-binary build and nextest commands for `shard`."""
    if shard not in SHARDS:
        raise ValueError(f"unknown CI shard: {shard!r}")
    commands = []
    helpers = HELPER_BINS.get(shard, ())
    if helpers:
        build = ["cargo", "build", "--profile", "test", "--locked", "--bins"]
        for pkg in helpers:
            build.extend(("-p", pkg))
        commands.append(build)
    nextest = [
        "cargo",
        "nextest",
        "run",
        "--profile",
        "ci",
        "--locked",
        "--no-tests=pass",
        "--cargo-profile",
        "test",
    ]
    if timings:
        nextest.append("--timings")
    for pkg in SHARDS[shard]:
        nextest.extend(("-p", pkg))
    commands.append(nextest)
    return commands


def run_shard(shard, timings=False, root=ROOT):
    verify(root)
    for cmd in build_commands(shard, timings=timings):
        print("$ " + shlex.join(cmd), flush=True)
        result = subprocess.run(cmd, cwd=root)
        if result.returncode != 0:
            return result.returncode
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    sub.add_parser("verify", help="Verify shard partition covers every workspace crate")
    run_parser = sub.add_parser("run", help="Build helper binaries and run nextest for a shard")
    run_parser.add_argument("shard", choices=tuple(SHARDS))
    run_parser.add_argument("--timings", action="store_true")
    args = parser.parse_args()
    if args.action == "verify":
        packages = verify()
        print(f"Verified {len(SHARDS)} shards covering {len(packages)} workspace crates.")
        return 0
    return run_shard(args.shard, timings=args.timings)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"ci-shards: {error}", file=sys.stderr)
        raise SystemExit(1) from error
