#!/usr/bin/env python3
"""Unit tests for CI test shard partitioning and command generation."""

from pathlib import Path
import tempfile
import unittest

from ci_shards import HELPER_BINS, SHARDS, build_commands, verify, workspace_packages


class ShardTests(unittest.TestCase):
    def test_repository_shards_partition_every_workspace_crate(self):
        workspace = workspace_packages()
        verified = verify()
        self.assertEqual(verified, workspace)
        self.assertEqual(set(SHARDS), {"gui", "daemon", "core"})
        self.assertGreaterEqual(len(workspace), 15)

    def test_missing_unknown_or_duplicate_crate_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/*"]\n', encoding="utf-8")
            for name in ("alpha", "beta"):
                pkg = root / "crates" / name
                pkg.mkdir(parents=True)
                (pkg / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n', encoding="utf-8")
            self.assertEqual(
                verify(root, shards={"a": ("alpha",), "b": ("beta",)}, helper_bins={"a": ("beta",)}),
                {"alpha", "beta"},
            )
            with self.assertRaisesRegex(ValueError, "missing"):
                verify(root, shards={"a": ("alpha",)}, helper_bins={})
            with self.assertRaisesRegex(ValueError, "unknown"):
                verify(root, shards={"a": ("alpha", "beta", "gamma")}, helper_bins={})
            with self.assertRaisesRegex(ValueError, "duplicate"):
                verify(root, shards={"a": ("alpha",), "b": ("alpha", "beta")}, helper_bins={})
            with self.assertRaisesRegex(ValueError, "empty"):
                verify(root, shards={"a": (), "b": ("alpha", "beta")}, helper_bins={})
            with self.assertRaisesRegex(ValueError, "helper_bins"):
                verify(root, shards={"a": ("alpha", "beta")}, helper_bins={"a": ("ghost",)})

    def test_commands_build_only_required_non_gui_helpers_in_test_profile(self):
        for shard in SHARDS:
            cmds = build_commands(shard, timings=(shard == "gui"))
            self.assertEqual(len(cmds), 2)
            build_cmd, nextest_cmd = cmds
            self.assertEqual(build_cmd[:5], ["cargo", "build", "--profile", "test", "--locked"])
            self.assertNotIn("krill-app", build_cmd)
            for helper in HELPER_BINS[shard]:
                self.assertIn(helper, build_cmd)
            self.assertIn("--cargo-profile", nextest_cmd)
            self.assertEqual(nextest_cmd[nextest_cmd.index("--cargo-profile") + 1], "test")
            if shard == "gui":
                self.assertIn("--timings", nextest_cmd)
            else:
                self.assertNotIn("--timings", nextest_cmd)
            for pkg in SHARDS[shard]:
                self.assertIn(pkg, nextest_cmd)


if __name__ == "__main__":
    unittest.main()
