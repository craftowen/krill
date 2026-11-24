#!/usr/bin/env python3
"""Prune superseded GitHub Actions Rust caches so active caches stay under the 10 GiB repo limit."""

import argparse
from collections import defaultdict
import json
import os
import re
import subprocess
import sys

VENDOR_CACHE_RE = re.compile(
    r"^v1-vendor-.+-(ci-[a-z0-9-]+-(?:Darwin|Linux|Windows_NT)-[A-Za-z0-9_]+)-[0-9a-f]{8}-[0-9a-f]{8}$"
)
RUST_CACHE_RE = re.compile(
    r"^v0-rust-([a-z0-9-]+-(?:Darwin|Linux|Windows_NT)-[A-Za-z0-9_]+)-[0-9a-f]{8}-[0-9a-f]{8}$"
)
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


def cache_family(key):
    """Return the logical cache family name (without vendor/lockfile hashes), or None if unknown."""
    if not isinstance(key, str):
        return None
    for pattern in (VENDOR_CACHE_RE, RUST_CACHE_RE):
        match = pattern.fullmatch(key)
        if match:
            return match.group(1)
    return None


def select_stale(caches, keep_per_group=1):
    """Select superseded caches per (ref, family), always preserving the newest `keep_per_group`."""
    if keep_per_group < 1:
        raise ValueError("keep_per_group must be >= 1")
    groups = defaultdict(list)
    for item in caches:
        family = cache_family(item.get("key"))
        ref = item.get("ref")
        if family is None or not ref or item.get("id") is None:
            continue
        groups[(ref, family)].append(item)
    stale = []
    for items in groups.values():
        ordered = sorted(items, key=lambda entry: (entry.get("created_at") or "", int(entry["id"])), reverse=True)
        stale.extend(ordered[keep_per_group:])
    return sorted(stale, key=lambda entry: (entry.get("created_at") or "", int(entry["id"])))


def list_caches(repo):
    result = subprocess.run(
        ["gh", "api", f"repos/{repo}/actions/caches?per_page=100", "--paginate", "--slurp"],
        capture_output=True, text=True, timeout=120, check=True,
    )
    pages = json.loads(result.stdout)
    return [cache for page in pages for cache in page.get("actions_caches", [])]


def delete_cache(repo, cache_id):
    result = subprocess.run(
        ["gh", "api", "-X", "DELETE", f"repos/{repo}/actions/caches/{int(cache_id)}"],
        capture_output=True, text=True, timeout=60,
    )
    if result.returncode == 0:
        return True
    # Another concurrent push may have already deleted the same cache entry.
    if "404" in result.stderr or "Not Found" in result.stderr:
        return False
    raise RuntimeError(f"failed to delete cache {cache_id}: {result.stderr.strip()}")


def prune(repo, keep_per_group=1, dry_run=False, reader=list_caches, deleter=delete_cache):
    if not REPO_RE.fullmatch(repo or ""):
        raise ValueError(f"invalid repository: {repo!r}")
    caches = reader(repo)
    stale = select_stale(caches, keep_per_group=keep_per_group)
    freed_bytes = 0
    deleted = 0
    for item in stale:
        size = int(item.get("size_in_bytes") or 0)
        if dry_run:
            deleted += 1
            freed_bytes += size
            continue
        if deleter(repo, item["id"]):
            deleted += 1
            freed_bytes += size
    total_bytes = sum(int(item.get("size_in_bytes") or 0) for item in caches)
    return {
        "total_caches": len(caches),
        "total_bytes": total_bytes,
        "stale_caches": len(stale),
        "deleted_caches": deleted,
        "freed_bytes": freed_bytes,
        "remaining_bytes": max(0, total_bytes - freed_bytes),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--keep", type=int, default=1)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    summary = prune(args.repo, keep_per_group=args.keep, dry_run=args.dry_run)
    print(json.dumps(summary, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, RuntimeError, subprocess.SubprocessError, OSError) as error:
        print(f"ci-cache-prune: {error}", file=sys.stderr)
        raise SystemExit(1) from error
