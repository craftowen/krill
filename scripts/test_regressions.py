#!/usr/bin/env python3
"""Exercise regression routing and real child success, failure and timeout."""

import importlib.util
import contextlib
import io
import os
import signal
import shutil
import subprocess
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("regressions", Path(__file__).with_name("check-regressions.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class RegressionTests(unittest.TestCase):
    def test_mobile_inventory_is_standalone_and_complete(self):
        checks = runner.commands(runner.ROOT, ["mobile", "mobile"])
        expected = {"mobile_" + name + ".cjs" for name in runner.MOBILE}
        self.assertEqual({Path(c[1]).name for c in checks}, expected)
        self.assertEqual(len(checks), len(expected))
        for _, path in checks:
            self.assertTrue(Path(path).is_file(), path)
            self.assertNotIn("browser", path)
            self.assertNotEqual(Path(path).name, "mobile_pwa.cjs")

    def test_new_or_deleted_mobile_checks_require_an_inventory_update(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tests = root / "crates/krilld/tests"
            tests.mkdir(parents=True)
            for name in ["mobile_" + n + ".cjs" for n in runner.MOBILE] + list(runner.MOBILE_INTEGRATION):
                (tests / name).touch()
            self.assertEqual(len(runner.commands(root, ["mobile"])), len(runner.MOBILE))
            extra = tests / "mobile_new_regression.cjs"
            extra.touch()
            with self.assertRaisesRegex(ValueError, "unregistered=.*mobile_new_regression"):
                runner.commands(root, ["mobile"])
            extra.unlink()
            (tests / "mobile_install.cjs").unlink()
            with self.assertRaisesRegex(ValueError, "missing=.*mobile_install"):
                runner.commands(root, ["mobile"])

    def test_harness_and_packaging_discover_new_python_regressions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("scripts/test_new.py", "packaging/test_new.py", "packaging/macos/test_new.py",
                         "docs/licensing/test_generate_notices.py", *runner.PACKAGING_INTEGRATION):
                file = root / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.touch()
            checks = runner.commands(root, ["harness", "packaging"])
            paths = {str(Path(c[1]).relative_to(root)) for c in checks}
            self.assertIn("scripts/test_new.py", paths)
            self.assertIn("packaging/test_new.py", paths)
            self.assertIn("packaging/macos/test_new.py", paths)
            self.assertIn("docs/licensing/test_generate_notices.py", paths)
            self.assertNotIn("packaging/macos/test_pipeline.py", paths)
            self.assertNotIn("packaging/homebrew/test_release.py", paths)

    def test_empty_discovery_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            for scope in ("harness", "packaging", "telemetry", "mobile"):
                with self.subTest(scope=scope), self.assertRaises(ValueError):
                    runner.commands(Path(directory), [scope])

    def test_child_failures_and_timeouts_remain_failures(self):
        for code, expected in (("pass", 0), ("raise SystemExit(7)", 7),
                               ("import time; time.sleep(60)", 124)):
            with self.subTest(code=code), contextlib.redirect_stdout(io.StringIO()), \
                 contextlib.redirect_stderr(io.StringIO()):
                timeout = 0.2 if expected == 124 else 5
                self.assertEqual(runner.run([sys.executable, "-c", code], runner.ROOT, timeout), expected)

    @unittest.skipUnless(os.name == "posix", "process group cleanup")
    def test_denied_cleanup_is_reported_and_cannot_turn_leftovers_into_success(self):
        errors = io.StringIO()
        with patch.object(runner.os, "killpg", side_effect=[None, PermissionError("denied"),
                                                          PermissionError("denied")]), \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(errors):
            result = runner.run([sys.executable, "-c", "pass"], runner.ROOT, 5)
        self.assertEqual(result, 1)
        self.assertIn("cannot signal process group", errors.getvalue())

    @unittest.skipUnless(os.name == "posix", "process group cleanup")
    def test_denied_cleanup_cannot_wait_forever(self):
        class Child:
            pid = 123

            def wait(self, timeout=None):
                if timeout is None:
                    raise AssertionError("cleanup must not wait without a deadline")
                raise subprocess.TimeoutExpired("fixture", timeout)

        errors = io.StringIO()
        with patch.object(runner.os, "killpg", side_effect=PermissionError("denied")), \
             contextlib.redirect_stderr(errors):
            runner.stop(Child())
        self.assertIn("could not reap process", errors.getvalue())

    @unittest.skipUnless(os.name == "posix", "process group cleanup")
    def test_timeout_also_stops_a_grandchild_that_ignores_sigterm(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "leaked-child"
            ready = Path(directory) / "child-ready"
            descendant = ("import signal,time; from pathlib import Path; "
                          "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                          "Path(" + repr(str(ready)) + ").touch(); time.sleep(2); "
                          "Path(" + repr(str(marker)) + ").touch()")
            parent = ("import subprocess,sys; p=subprocess.Popen([sys.executable,'-c',"
                      + repr(descendant) + "]); p.wait()")
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                result = runner.run([sys.executable, "-c", parent], runner.ROOT, 1)
            self.assertEqual(result, 124)
            self.assertTrue(ready.exists(), "the grandchild must start before timeout")
            time.sleep(1.3)
            self.assertFalse(marker.exists(), "a descendant survived the runner timeout")

    @unittest.skipUnless(os.name == "posix", "process group cleanup")
    def test_normal_parent_exit_cannot_leave_a_running_descendant(self):
        with tempfile.TemporaryDirectory() as directory:
            markers = []
            for status in (0, 7):
                marker = Path(directory) / ("leaked-" + str(status))
                ready = Path(directory) / ("ready-" + str(status))
                markers.append(marker)
                descendant = ("import time;from pathlib import Path;Path(" + repr(str(ready))
                              + ").touch();time.sleep(2);Path(" + repr(str(marker)) + ").touch()")
                parent = ("import subprocess,sys,time;from pathlib import Path;"
                          "subprocess.Popen([sys.executable,'-c'," + repr(descendant) + "]);"
                          "\nwhile not Path(" + repr(str(ready)) + ").exists(): time.sleep(.01)"
                          "\nraise SystemExit(" + str(status) + ")")
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(runner.run([sys.executable, "-c", parent], runner.ROOT, 5), status or 1)
            time.sleep(2.2)
            self.assertTrue(all(not marker.exists() for marker in markers))

    @unittest.skipUnless(os.name == "posix", "process group cleanup")
    def test_cli_termination_signals_clean_the_active_test_group(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            shutil.copy2(runner.ROOT / "scripts/check-regressions.py", scripts)
            markers = []
            for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGALRM):
                with self.subTest(signal=sig):
                    ready = root / ("ready-" + str(sig))
                    marker = root / ("leaked-" + str(sig))
                    markers.append(marker)
                    (scripts / "test_slow.py").write_text(
                        "import time;from pathlib import Path;Path(" + repr(str(ready))
                        + ").touch();time.sleep(2);Path(" + repr(str(marker)) + ").touch()")
                    with subprocess.Popen([sys.executable, str(scripts / "check-regressions.py"),
                                           "--scope", "harness"], stdout=subprocess.DEVNULL,
                                          stderr=subprocess.DEVNULL) as child:
                        try:
                            deadline = time.monotonic() + 5
                            while not ready.exists() and time.monotonic() < deadline:
                                time.sleep(.01)
                            self.assertTrue(ready.exists(), "the CLI must start its test")
                            child.send_signal(sig)
                            self.assertEqual(child.wait(timeout=5), 128 + sig)
                        finally:
                            if child.poll() is None:
                                child.kill()
                                child.wait()
            time.sleep(2.2)
            self.assertTrue(all(not marker.exists() for marker in markers),
                            "the CLI left its active test running")

    @unittest.skipUnless(os.name == "posix", "process group cleanup")
    def test_second_termination_cannot_interrupt_child_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            shutil.copy2(runner.ROOT / "scripts/check-regressions.py", scripts)
            ready, signalled = root / "ready", root / "signalled"
            (scripts / "test_stubborn.py").write_text(
                "import os,signal,time\nfrom pathlib import Path\n"
                "signal.signal(signal.SIGTERM,lambda *_:Path(" + repr(str(signalled)) + ").touch())\n"
                "Path(" + repr(str(ready)) + ").write_text(str(os.getpid()))\n"
                "while True:time.sleep(.05)\n")
            child = subprocess.Popen([sys.executable, str(scripts / "check-regressions.py"),
                                      "--scope", "harness"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            worker_pid = None
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and (not ready.exists() or not ready.read_text().strip()):
                    time.sleep(.01)
                self.assertTrue(ready.exists(), "the fixture must start")
                worker_pid = int(ready.read_text())
                child.send_signal(signal.SIGTERM)
                deadline = time.monotonic() + 5
                while not signalled.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue(signalled.exists(), "cleanup must signal the fixture before the second interrupt")
                child.send_signal(signal.SIGTERM)
                # A surviving worker retains these pipes even after its parent exits.
                child.communicate(timeout=8)
                self.assertEqual(child.returncode, 143)
            finally:
                if worker_pid is not None:
                    try:
                        os.kill(worker_pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass
                if child.poll() is None:
                    child.kill()
                child.communicate(timeout=3)

    def test_main_propagates_failure_after_running_remaining_checks(self):
        with patch.object(sys, "argv", ["check-regressions.py"]), \
             patch.object(runner, "commands", return_value=[["first"], ["second"]]), \
             patch.object(runner, "run", side_effect=[7, 0]) as run, \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(runner.main(), 1)
            self.assertEqual(run.call_count, 2)

    def test_missing_executable_is_a_failure_and_does_not_skip_later_checks(self):
        with patch.object(sys, "argv", ["check-regressions.py"]), \
             patch.object(runner, "commands", return_value=[["missing"], ["second"]]), \
             patch.object(runner, "run", side_effect=[FileNotFoundError("fixture"), 0]) as run, \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(runner.main(), 1)
            self.assertEqual(run.call_count, 2)

    def test_listing_does_not_execute_anything(self):
        with patch.object(sys, "argv", ["check-regressions.py", "--list"]), \
             patch.object(runner, "run", side_effect=AssertionError("must not run")), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(runner.main(), 0)


if __name__ == "__main__":
    unittest.main()
