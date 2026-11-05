#!/usr/bin/env python3
"""Cache corruption and source identity regressions."""

import os
from pathlib import Path
import tempfile
import unittest

from vendor_cache import digest_tree, stage, verify_stage


class StagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.project = self.root / "project"
        source = self.project / "vendor/dependency/src"
        source.mkdir(parents=True)
        (source / "lib.rs").write_text("pub fn value() -> u8 { 1 }\n")
        (self.project / "Cargo.toml").write_text('[workspace]\n[patch.crates-io]\ndependency = { path = "vendor/dependency" }\n')
        (self.project / "Cargo.lock").write_text("unchanged\n")

    def run_stage(self, case="producer"):
        return stage(self.project, self.root / "home", self.root / "external", self.root / "evidence", case)

    def test_external_config_preserves_source_and_copies_bytes_and_modes(self):
        before = digest_tree(self.project)
        result = self.run_stage()
        self.assertEqual(before, digest_tree(self.project))
        self.assertEqual(result["vendor_hash"], digest_tree(Path(result["vendor_directory"])))
        self.assertIn(result["vendor_directory"], (self.root / "home/config.toml").read_text())
        self.assertEqual(result, self.run_stage())

    def test_control_does_not_install_a_global_patch(self):
        self.run_stage("control")
        self.assertFalse((self.root / "home/config.toml").exists())

    def test_existing_config_and_source_inside_workspace_are_rejected(self):
        home = self.root / "home"
        home.mkdir()
        (home / "config.toml").write_text("existing configuration\n")
        with self.assertRaisesRegex(ValueError, "existing Cargo"):
            self.run_stage()
        self.assertEqual((home / "config.toml").read_text(), "existing configuration\n")
        with self.assertRaisesRegex(ValueError, "outside"):
            stage(self.project, self.project / "home", self.root / "external", self.root / "evidence", "producer")

    def test_changed_bytes_or_permissions_change_key_even_with_same_mtime(self):
        path = self.project / "vendor/dependency/src/lib.rs"
        first = digest_tree(self.project / "vendor")
        before = path.stat()
        path.write_text(path.read_text().replace("1", "2"))
        os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.assertEqual(before.st_mtime_ns, path.stat().st_mtime_ns)
        second = digest_tree(self.project / "vendor")
        self.assertNotEqual(first, second)
        path.chmod(0o700)
        self.assertNotEqual(second, digest_tree(self.project / "vendor"))

    def test_corrupt_existing_or_restored_source_is_not_used(self):
        record = self.run_stage()
        (Path(record["vendor_directory"]) / "dependency/src/lib.rs").write_text("corrupt")
        with self.assertRaisesRegex(ValueError, "differ"):
            self.run_stage()
        with self.assertRaisesRegex(ValueError, "restored vendor"):
            verify_stage(self.root / "evidence")

    def test_symlink_input_is_rejected(self):
        (self.project / "vendor/link").symlink_to(self.project / "Cargo.lock")
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.run_stage()

    def test_changed_global_configuration_is_rejected(self):
        self.run_stage()
        (self.root / "home/config.toml").write_text("different config\n")
        with self.assertRaisesRegex(ValueError, "configuration changed"):
            verify_stage(self.root / "evidence")


if __name__ == "__main__":
    unittest.main()
