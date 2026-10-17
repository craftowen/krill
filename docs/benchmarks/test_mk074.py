"""Run: python3 -m unittest discover -s docs/benchmarks -v"""

import importlib.util
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import mk074

SPEC = importlib.util.spec_from_file_location(
    "notices", Path(__file__).resolve().parents[1] / "licensing/generate_notices.py")
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)


class BenchmarkTests(unittest.TestCase):
    def test_descendants_exclude_other_workers(self):
        table = {1: (0, "init"), 10: (1, "app"), 11: (10, "holder"),
                 12: (11, "daemon"), 13: (12, "shell"), 20: (1, "other-worker")}
        self.assertEqual(mk074.descendants(10, table), {10, 11, 12, 13})

    def test_window_requires_matching_pid_normal_layer_and_real_size(self):
        window = {"kCGWindowOwnerPID": 10, "kCGWindowLayer": 0,
                  "kCGWindowBounds": {"Width": 1200, "Height": 760}}
        self.assertTrue(mk074.has_window(10, [window]))
        self.assertFalse(mk074.has_window(20, [window]))
        self.assertFalse(mk074.has_window(10, [dict(window, kCGWindowLayer=3)]))
        self.assertFalse(mk074.has_window(10, [dict(window, kCGWindowBounds={"Width": 1, "Height": 1})]))

    def test_cleanup_never_signals_reused_pid(self):
        class Info:
            start = 456
        with patch.object(mk074, "usage", return_value=Info()), patch.object(mk074.os, "kill") as kill:
            mk074.cleanup({123: 123})
            kill.assert_not_called()
            mk074.cleanup({123: 456})
            kill.assert_called_once_with(123, mk074.signal.SIGKILL)

    def test_child_environment_is_isolated(self):
        with patch.dict(mk074.os.environ, {"ORCA_SESSION_ID": "private", "KRILL_STATE_DIR": "private", "MK_STATE_DIR": "private"}):
            env = mk074.isolated_env(Path("/tmp/isolated"))
        self.assertNotIn("ORCA_SESSION_ID", env)
        self.assertNotIn("KRILL_STATE_DIR", env)
        self.assertNotIn("MK_STATE_DIR", env)
        self.assertEqual(env["HOME"], "/tmp/isolated")

    def test_orca_snapshot_only_reads_and_excludes_cli_and_agent(self):
        rows = ("10 1 100 /Applications/Orca.app/Contents/MacOS/Orca\n"
                "11 1 100 /Applications/Orca.app/Contents/MacOS/Orca\n"
                "12 10 100 /usr/local/bin/agent\n")
        args = "10 Orca\n11 Orca /out/cli/index.js check\n12 agent\n"
        result = SimpleNamespace(returncode=0, stdout="    phys_footprint: 1234 B\n")
        with patch.object(mk074.subprocess, "check_output", side_effect=[rows, args]), \
                patch.object(mk074.subprocess, "run", return_value=result) as run, \
                patch.object(mk074.subprocess, "Popen") as launch, \
                patch.object(mk074.os, "kill") as kill:
            snapshot = mk074.orca_snapshot()
        self.assertEqual(snapshot["phys_footprint_bytes"], 1234)
        self.assertEqual(snapshot["rss_bytes"], 102400)
        self.assertEqual([p["pid"] for p in snapshot["processes"]], [10])
        self.assertEqual(run.call_args.args[0][:3], ["/usr/bin/footprint", "-p", "10"])
        launch.assert_not_called()
        kill.assert_not_called()


    def test_orca_version_never_crashes_report(self):
        with patch.object(mk074.Path, "read_bytes", side_effect=FileNotFoundError(2, "missing")):
            self.assertIsInstance(mk074.orca_version(), str)


class NoticeTests(unittest.TestCase):
    def test_preserves_notice_copyright_and_marks_missing_archives(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            present = root / "present"
            absent = root / "absent"
            present.mkdir()
            absent.mkdir()
            (present / "LICENSE-MIT").write_text("Copyright example\nPermission is granted.\n")
            (present / "NOTICE").write_text("Attribution to upstream.\n")
            def package(name, path):
                return {"id": name, "name": name, "version": "1.0", "manifest_path": str(path / "Cargo.toml"),
                        "license": "MIT OR Apache-2.0", "repository": "https://example.com/upstream"}
            metadata = {"workspace_members": [], "packages": [package("a", present), package("b", absent)]}
            report = notices.render(metadata, {"a 1.0 registry": {}, "b 1.0 registry": {}}, "lock-hash")
            self.assertIn("Copyright example", report)
            self.assertIn("Attribution to upstream.", report)
            self.assertIn("**UPSTREAM TEXT MISSING**", report)
            self.assertIn("2 packages; 1 packages without notice texts.", report)
            self.assertIn("lock-hash", report)

    def test_ambiguous_sources_fail_closed(self):
        metadata = {"workspace_members": [], "packages": [
            {"name": "a", "version": "1"}, {"name": "a", "version": "1"}]}
        with self.assertRaises(ValueError):
            notices.render(metadata, {"a 1 registry": {}}, "hash")


if __name__ == "__main__":
    unittest.main()
