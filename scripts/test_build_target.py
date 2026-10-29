#!/usr/bin/env python3
"""Build path policy: traversal, symlinks, missing volume, and no directory creation."""

import importlib.util
import os
import json
import contextlib
import io
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("build_target", Path(__file__).with_name("check-build-target.py"))
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class TargetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="krill-target-policy-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.targets = self.root / "volume/targets"
        self.targets.mkdir(parents=True)

    def validate(self, path, mounted=True):
        with patch.object(Path, "is_mount", return_value=mounted):
            return policy.validate_target(path, self.targets)

    def test_external_child_with_spaces_is_accepted_without_creating_it(self):
        target = self.targets / "work tree/debug"
        self.assertEqual(self.validate(target), target)
        self.assertFalse(target.exists())

    def test_internal_path_prefix_sibling_parent_and_traversal_are_rejected(self):
        for target in (self.root / "target", self.root / "volume/targets-other/work",
                       self.targets, self.targets / "../internal/work"):
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, "must be below"):
                self.validate(target)

    def test_symlink_out_of_external_targets_is_rejected(self):
        (self.targets / "escape").symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "must be below"):
            self.validate(self.targets / "escape/work")

    def test_missing_mount_is_rejected_even_if_the_directory_exists(self):
        with self.assertRaisesRegex(ValueError, "not mounted"):
            self.validate(self.targets / "work", mounted=False)

    def test_targets_root_cannot_itself_redirect_to_internal_storage(self):
        self.targets.rmdir()
        self.targets.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "outside its build volume"):
            self.validate(self.targets / "work")

    def test_intermediate_build_directory_must_also_be_external(self):
        target = self.targets / "work"
        with patch.object(Path, "is_mount", return_value=True):
            with self.assertRaisesRegex(ValueError, "build_directory"):
                policy.validate_directories({"target_directory": str(target),
                                             "build_directory": str(self.root / "internal-build")}, self.targets)
            checked = policy.validate_directories({"target_directory": str(target),
                                                    "build_directory": str(self.targets / "intermediate")}, self.targets)
            self.assertEqual(checked["build_directory"], self.targets / "intermediate")

    def test_older_metadata_without_build_directory_uses_target(self):
        target = self.targets / "work"
        with patch.object(Path, "is_mount", return_value=True):
            checked = policy.validate_directories({"target_directory": str(target)}, self.targets)
        self.assertEqual(checked["target_directory"], checked["build_directory"])

    def test_explicit_target_override_cannot_skip_intermediate_directory_validation(self):
        target = self.targets / "override"
        metadata = {"target_directory": str(target), "build_directory": str(self.root / "internal-build")}
        with patch.object(sys, "argv", ["check-build-target.py", "--target-dir", str(target)]), \
             patch.object(sys, "platform", "darwin"), patch.object(policy, "TARGETS", self.targets), \
             patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), \
             patch.object(Path, "is_mount", return_value=True), \
             patch.object(policy.subprocess, "check_output", return_value=json.dumps(metadata)) as cargo, \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(policy.main(), 1)
            self.assertEqual(cargo.call_args.kwargs["env"]["CARGO_TARGET_DIR"], str(target))
        self.assertFalse(target.exists())

    def toolchain_probe_environment(self):
        tools = self.root / "tools"
        tools.mkdir()
        probe = self.root / "brew-was-called"
        # Even if the guard regresses, never reach the real toolchain/build.
        brew = tools / "brew"
        brew.write_text('#!/bin/sh\n: > "$KRILL_TARGET_POLICY_PROBE"\nexit 1\n')
        brew.chmod(0o755)
        return probe, {**os.environ, "GITHUB_ACTIONS": "false", "KRILL_TARGET_POLICY_PROBE": str(probe),
                       "PATH": str(tools) + os.pathsep + os.environ["PATH"]}

    @unittest.skipUnless(sys.platform == "darwin", "local macOS policy")
    def test_windows_script_rejects_internal_override_before_starting_cargo(self):
        target = self.root / "must-not-be-created"
        probe, env = self.toolchain_probe_environment()
        result = subprocess.run(
            ["bash", str(policy.ROOT / "scripts/check-windows.sh"), "--target-dir", str(target)],
            capture_output=True, text=True, timeout=15,
            env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("build-target:", result.stderr)
        self.assertFalse(target.exists())
        self.assertFalse(probe.exists(), "unsafe target must be refused before toolchain probes")

    @unittest.skipUnless(sys.platform == "darwin", "local macOS policy")
    def test_toolchain_only_bootstrap_does_not_require_a_cargo_target(self):
        target = self.root / "unused-target"
        probe, env = self.toolchain_probe_environment()
        result = subprocess.run(
            ["bash", str(policy.ROOT / "scripts/check-windows.sh"), "--bootstrap", "--target-dir", str(target)],
            env=env, capture_output=True, text=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)  # Fake brew stops before download/install.
        self.assertNotIn("build-target:", result.stderr)
        self.assertTrue(probe.exists())
        self.assertFalse(target.exists())


if __name__ == "__main__":
    unittest.main()
