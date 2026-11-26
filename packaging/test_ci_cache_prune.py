#!/usr/bin/env python3
"""Unit tests for GitHub Actions Rust cache pruning."""

import unittest

from ci_cache_prune import cache_family, prune, select_stale


class CachePruneTests(unittest.TestCase):
    def test_cache_family_groups_vendor_and_rust_cache_keys_and_ignores_unknown(self):
        macos_a = (
            "v1-vendor-" + "a" * 64 + "-15.5-macos15-20261118.0550.1-" + "b" * 64
            + "-ci-macos-15-Darwin-arm64-757d1e03-d96ebd23"
        )
        macos_b = (
            "v1-vendor-" + "c" * 64 + "-15.6-macos15-20261201.0001.0-" + "d" * 64
            + "-ci-macos-15-Darwin-arm64-757d1e03-6b737a28"
        )
        self.assertEqual(cache_family(macos_a), "ci-macos-15-Darwin-arm64")
        self.assertEqual(cache_family(macos_b), "ci-macos-15-Darwin-arm64")
        self.assertEqual(
            cache_family("v0-rust-ci-windows-test-Windows_NT-x64-7a5511c2-6b737a28"),
            "ci-windows-test-Windows_NT-x64",
        )
        self.assertEqual(
            cache_family("v0-rust-ci-windows-release-fast-Windows_NT-x64-7a5511c2-6b737a28"),
            "ci-windows-release-fast-Windows_NT-x64",
        )
        self.assertEqual(
            cache_family("v0-rust-release-windows-msvc-static-Windows_NT-x64-7901fbdd-6b737a28"),
            "release-windows-msvc-static-Windows_NT-x64",
        )
        win_vendor_ci = (
            "v1-vendor-" + "a" * 64 + "-none-win25-20261118.1-" + "b" * 64
            + "-ci-windows-test-Windows_NT-x64-7a5511c2-6b737a28"
        )
        win_vendor_rel = (
            "v1-vendor-" + "a" * 64 + "-none-win25-20261118.1-" + "b" * 64
            + "-release-windows-audit-Windows_NT-x64-7901fbdd-6b737a28"
        )
        self.assertEqual(cache_family(win_vendor_ci), "ci-windows-test-Windows_NT-x64")
        self.assertEqual(cache_family(win_vendor_rel), "release-windows-audit-Windows_NT-x64")
        self.assertIsNone(cache_family("custom-unrelated-cache-key"))
        self.assertIsNone(cache_family(None))

    def test_select_stale_keeps_newest_per_ref_and_family_only(self):
        caches = [
            {"id": 1, "ref": "refs/heads/dev", "created_at": "2026-11-22T00:00:00Z", "size_in_bytes": 100,
             "key": "v0-rust-ci-windows-test-Windows_NT-x64-7a5511c2-11111111"},
            {"id": 2, "ref": "refs/heads/dev", "created_at": "2026-11-24T00:00:00Z", "size_in_bytes": 120,
             "key": "v0-rust-ci-windows-test-Windows_NT-x64-7a5511c2-22222222"},
            {"id": 3, "ref": "refs/heads/release/nightly", "created_at": "2026-11-23T00:00:00Z", "size_in_bytes": 150,
             "key": "v0-rust-ci-windows-test-Windows_NT-x64-7a5511c2-33333333"},
            {"id": 4, "ref": "refs/heads/dev", "created_at": "2026-11-20T00:00:00Z", "size_in_bytes": 500,
             "key": "unrecognized-custom-cache"},
        ]
        stale = select_stale(caches, keep_per_group=1)
        self.assertEqual([item["id"] for item in stale], [1])
        with self.assertRaises(ValueError):
            select_stale(caches, keep_per_group=0)

    def test_prune_deletes_only_superseded_caches_and_reports_freed_bytes(self):
        caches = [
            {"id": 10, "ref": "refs/heads/release/nightly", "created_at": "2026-11-21T00:00:00Z", "size_in_bytes": 1000,
             "key": "v0-rust-release-windows-msvc-static-Windows_NT-x64-7901fbdd-11111111"},
            {"id": 11, "ref": "refs/heads/release/nightly", "created_at": "2026-11-22T00:00:00Z", "size_in_bytes": 1100,
             "key": "v0-rust-release-windows-msvc-static-Windows_NT-x64-7901fbdd-22222222"},
            {"id": 12, "ref": "refs/heads/release/nightly", "created_at": "2026-11-23T00:00:00Z", "size_in_bytes": 1200,
             "key": "v0-rust-release-windows-msvc-static-Windows_NT-x64-7901fbdd-33333333"},
        ]
        deleted = []
        summary = prune(
            "owner/repo",
            keep_per_group=1,
            reader=lambda _repo: caches,
            deleter=lambda _repo, cid: deleted.append(cid) or True,
        )
        self.assertEqual(deleted, [10, 11])
        self.assertEqual(summary["deleted_caches"], 2)
        self.assertEqual(summary["freed_bytes"], 2100)
        self.assertEqual(summary["remaining_bytes"], 1200)

        with self.assertRaises(ValueError):
            prune("bad repo", reader=lambda _repo: [])


if __name__ == "__main__":
    unittest.main()
