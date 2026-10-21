#!/usr/bin/env python3
"""야간 배포의 실패 차단·커밋 고정·자산 출처 검증. 실제 빌드·원격 쓰기는 하지 않는다."""

import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import nightly
from nightly_assets import import_assets

SHA = "a" * 40
BASE = "b" * 40
PUB = "c" * 64


class AssetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.assets, self.output = self.root / "assets", self.root / "out"
        self.assets.mkdir()
        for platform in ("windows", "x86_64", "aarch64"):
            (self.assets / f"source-commit-{platform}.txt").write_text(SHA + "\r\n")
        self.windows = self.assets / "Krill-0.1.2-windows-x64.zip"
        self.write_zip(PUB)
        for arch in ("x86_64", "aarch64"):
            archive = self.assets / f"krill-0.1.2-linux-{arch}.tar.gz"
            archive.write_bytes(b"native-linux")
            self.checksum(archive)

    def write_zip(self, key):
        with zipfile.ZipFile(self.windows, "w") as archive:
            archive.writestr("Krill/Krill.json", json.dumps({
                "CFBundleShortVersionString": "0.1.2", "KrillUpdatePublicKey": key}))
        self.checksum(self.windows)

    def checksum(self, archive):
        archive.with_name(archive.name + ".sha256").write_text(
            hashlib.sha256(archive.read_bytes()).hexdigest() + "  " + archive.name + "\r\n")

    def run_import(self):
        import_assets(self.assets, self.output, "0.1.2", SHA, PUB)

    def test_round_trip_preserves_provenance_for_the_publisher(self):
        self.run_import()
        final = self.root / "publish"
        import_assets(self.output, final, "0.1.2", SHA, PUB)
        self.assertEqual((final / self.windows.name).read_bytes(), self.windows.read_bytes())
        self.assertEqual(len(list(final.iterdir())), 9)

    def test_wrong_head_does_not_copy_any_assets(self):
        (self.assets / "source-commit-aarch64.txt").write_text(BASE)
        with self.assertRaisesRegex(ValueError, "source does not match"):
            self.run_import()
        self.assertFalse(self.output.exists())

    def test_corruption_and_checksum_path_injection_stop_import(self):
        self.windows.write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            self.run_import()
        self.checksum(self.windows)
        checksum = self.windows.with_name(self.windows.name + ".sha256")
        checksum.write_text(hashlib.sha256(b"corrupt").hexdigest() + "  ../../unexpected")
        with self.assertRaisesRegex(ValueError, "checksum filename"):
            self.run_import()

    def test_other_update_key_stops_import(self):
        self.write_zip("d" * 64)
        with self.assertRaisesRegex(ValueError, "update key"):
            self.run_import()

    def test_symlink_and_overwrite_are_refused(self):
        other = self.root / "real.zip"
        self.windows.rename(other)
        self.windows.symlink_to(other)
        with self.assertRaisesRegex(ValueError, "symlinks"):
            self.run_import()
        self.windows.unlink()
        other.rename(self.windows)
        self.run_import()
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.run_import()


class WorkflowTests(unittest.TestCase):
    def run_info(self, name, **values):
        return {"id": 1, "run_attempt": 1, "path": f".github/workflows/{name}",
                "head_sha": SHA, "head_branch": "release/nightly-test", "status": "completed",
                "conclusion": "success", "html_url": "https://example.invalid/run", **values}

    def test_missing_pending_wrong_sha_or_wrong_branch_cannot_pass(self):
        expected = {"ci.yml", "ci-macos.yml"}
        first = self.run_info("ci.yml")
        for second in ([], [self.run_info("ci-macos.yml", status="in_progress", conclusion=None)],
                       [self.run_info("ci-macos.yml", head_sha=BASE)],
                       [self.run_info("ci-macos.yml", head_branch="main")]):
            self.assertIsNone(nightly.workflow_results([first, *second], SHA, first["head_branch"], expected))

    def test_failure_cancellation_and_newest_attempt(self):
        good = self.run_info("ci.yml")
        for conclusion in ("failure", "cancelled", "timed_out", "skipped"):
            bad = self.run_info("ci.yml", id=2, conclusion=conclusion)
            with self.assertRaises(nightly.NightlyError):
                nightly.workflow_results([good, bad], SHA, good["head_branch"], {"ci.yml"})
        passed = self.run_info("ci.yml", id=2, run_attempt=2)
        failed = self.run_info("ci.yml", id=2, conclusion="failure")
        self.assertIsNotNone(nightly.workflow_results([passed, failed], SHA, good["head_branch"], {"ci.yml"}))

    def test_launchd_waits_until_midnight_and_does_not_start_during_install(self):
        job = nightly.launch_agent(Path("/runner"), Path("/config"), Path("/logs"))
        self.assertEqual(job["StartCalendarInterval"], {"Hour": 0, "Minute": 0})
        self.assertFalse(job.get("RunAtLoad", False))
        self.assertFalse(job.get("KeepAlive", False))
        self.assertEqual(job["ProgramArguments"][2], "dispatch")

    def test_dispatch_uses_orca_shell_and_safe_quoting_without_starting_an_agent(self):
        with tempfile.TemporaryDirectory(prefix="nightly space ") as temporary:
            root = Path(temporary)
            config = {"orca_cli": "/path with spaces/orca", "workspace": "id:repo::/a b"}
            result = {"ok": True, "result": {"terminal": {"handle": "test-terminal"}}}
            with patch.object(nightly, "command", side_effect=["{}", json.dumps(result)]) as mocked:
                nightly.dispatch(config, root / "config.json", preflight_only=True)
            args = mocked.call_args_list[1].args[0]
            self.assertEqual(args[1:3], ["terminal", "create"])
            self.assertIn("preflight", args[args.index("--command") + 1])
            self.assertNotIn("agent", args)
            self.assertEqual(json.loads((root / "dispatch.json").read_text())["terminal"], "test-terminal")

    def test_patch_version_is_newer_than_both_workspace_and_latest_release(self):
        self.assertEqual(nightly.bump_version("0.1.2", "0.1.4"), "0.1.5")
        self.assertEqual(nightly.bump_version("0.2.0", "0.1.4"), "0.2.1")
        with self.assertRaises(nightly.NightlyError):
            nightly.bump_version("0.1.0-beta", "0.1.0")


class PublicationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.config = {"source": str(root / "source"), "run_root": str(root),
                       "target_dir": "/Volumes/krill-build/targets/nightly", "key_file": "/unused", "repo": "o/r"}
        self.release = nightly.Release(self.config, root / "state")
        self.release.work.mkdir()
        self.calls = []
        self.failure = None
        self.main = BASE
        self.current = SHA
        self.dirty = ""
        self.release.prepare = lambda: (SHA, "release/nightly-test", "0.1.2", BASE)
        self.release.run = self.fake_run
        self.release.wait_ci = self.fake_ci
        preflight = patch.object(nightly, "preflight", return_value=PUB)
        preflight.start()
        self.addCleanup(preflight.stop)
        importer = patch("nightly_assets.import_assets")
        self.importer = importer.start()
        self.addCleanup(importer.stop)

    def fake_run(self, *args, **_kwargs):
        args = tuple(map(str, args))
        self.calls.append(args)
        if self.failure and self.failure in args:
            raise nightly.NightlyError("injected failure")
        if args == ("git", "rev-parse", "origin/main"):
            return self.main
        if args == ("git", "rev-parse", "HEAD"):
            return self.current
        if args == ("git", "status", "--porcelain"):
            return self.dirty
        return ""

    def fake_ci(self, _sha, branch, _workflows):
        self.calls.append(("wait-ci", branch))
        if self.failure == "ci" or self.failure == "main-ci" and branch == "main":
            raise nightly.NightlyError("injected CI failure")
        return {"release.yml": {"id": 123}}

    def promoted(self):
        return ("git", "push", "origin", "HEAD:refs/heads/main") in self.calls

    def published(self):
        return any("packaging/publish-github.sh" in call for call in self.calls)

    def test_success_promotes_validated_head_and_imports_assets_before_publication(self):
        self.release.publish()
        self.assertTrue(self.promoted())
        self.assertTrue(self.published())
        self.assertEqual(self.release.state["status"], "published")
        self.importer.assert_called_once()
        receipt = self.release.state_dir / "checks" / f"{SHA}.json"
        self.assertEqual(json.loads(receipt.read_text())["head"], SHA)
        publish = next(i for i, call in enumerate(self.calls) if "packaging/publish-github.sh" in call)
        self.assertLess(self.calls.index(("wait-ci", "main")), publish)
        downloads = [call for call in self.calls if call[:3] == ("gh", "run", "download")]
        self.assertEqual(len(downloads), 3)
        self.assertTrue(all(call.count("-n") == 1 for call in downloads))

    def test_any_local_gate_or_native_ci_failure_prevents_promotion_and_publication(self):
        for failure in ("scripts/check.sh", "scripts/check-windows.sh", "build", "pane_e2e", "survive", "ci"):
            with self.subTest(failure=failure):
                self.failure = failure
                self.calls.clear()
                with self.assertRaises(nightly.NightlyError):
                    self.release.publish()
                self.assertFalse(self.promoted())
                self.assertFalse(self.published())
                for path in (self.release.state_dir / "checks").glob("*"):
                    path.unlink()

    def test_main_race_or_candidate_mutation_prevents_promotion(self):
        for field, value in (("main", SHA), ("current", BASE), ("dirty", " M Cargo.toml")):
            with self.subTest(field=field):
                old = getattr(self, field)
                setattr(self, field, value)
                self.calls.clear()
                with self.assertRaises(nightly.NightlyError):
                    self.release.publish()
                self.assertFalse(self.promoted())
                self.assertFalse(self.published())
                setattr(self, field, old)

    def test_main_ci_failure_keeps_existing_release(self):
        self.failure = "main-ci"
        with self.assertRaises(nightly.NightlyError):
            self.release.publish()
        self.assertTrue(self.promoted())
        self.assertFalse(self.published())

    def test_no_changes_skips_all_gates_and_remote_writes(self):
        self.release.prepare = lambda: None
        self.release.publish()
        self.assertEqual(self.calls, [])


class SnapshotTests(unittest.TestCase):
    def test_prepare_integrates_committed_dev_and_main_without_touching_dirty_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, origin = root / "source", root / "origin.git"

            def git(cwd, *args):
                return subprocess.check_output(["git", *args], cwd=cwd, stderr=subprocess.DEVNULL).decode().strip()

            git(root, "init", "--bare", "--initial-branch=main", str(origin))
            git(root, "init", "--initial-branch=main", str(source))
            git(source, "config", "user.name", "nightly test")
            git(source, "config", "user.email", "nightly@example.invalid")
            (source / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.1"\n')
            (source / "Cargo.lock").write_text("lock\n")
            (source / "THIRD_PARTY_NOTICES.md").write_text("notices\n")
            git(source, "add", ".")
            git(source, "commit", "-m", "published")
            git(source, "tag", "v0.1.1")
            git(source, "remote", "add", "origin", str(origin))
            git(source, "push", "origin", "main", "--tags")
            git(source, "checkout", "-b", "dev")
            (source / "dev-change").write_text("committed dev\n")
            git(source, "add", ".")
            git(source, "commit", "-m", "dev work")
            frozen = git(source, "rev-parse", "HEAD")
            git(source, "push", "origin", "dev")
            git(source, "checkout", "main")
            (source / "main-change").write_text("separate main commit\n")
            git(source, "add", ".")
            git(source, "commit", "-m", "other release work")
            git(source, "push", "origin", "main")
            git(source, "checkout", "dev")
            (source / "dev-change").write_text("dirty tracked edit; must remain local\n")
            (source / "dirty-file").write_text("must remain local\n")
            status = git(source, "status", "--porcelain")
            config = {"source": str(source), "branch": "dev", "repo": "o/r", "run_root": str(root / "nightly"),
                      "target_dir": str(nightly.TARGETS / "nightly"), "key_file": "/unused",
                      "user.name": "nightly test", "user.email": "nightly@example.invalid"}
            release = nightly.Release(config, root / "state")
            real_command = nightly.command

            def local_command(args, **kwargs):
                args = list(map(str, args))
                if args[:4] == ["git", "remote", "add", "origin"]:
                    args[-1] = str(origin)
                if args[:3] == ["gh", "release", "view"]:
                    return json.dumps({"tagName": "v0.1.1"})
                if args[0] in ("cargo", "python3"):
                    return ""  # 고정 fixture에는 바뀌는 의존성·고지문이 없다.
                return real_command(args, **kwargs)

            with patch.object(nightly, "command", side_effect=local_command):
                sha, _branch, version, _main = release.prepare()
            self.assertEqual(version, "0.1.2")
            self.assertEqual(release.state["source_head"], frozen)
            self.assertEqual(git(source, "rev-parse", "HEAD"), frozen)
            self.assertEqual(git(source, "status", "--porcelain"), status)
            self.assertEqual((release.work / "dev-change").read_text(), "committed dev\n")
            self.assertEqual((release.work / "main-change").read_text(), "separate main commit\n")
            self.assertFalse((release.work / "dirty-file").exists())
            self.assertEqual(git(release.work, "status", "--porcelain"), "")
            self.assertNotEqual(sha, frozen)
            self.assertIn(f"Nightly-Source: {frozen}", git(release.work, "log", "-1", "--format=%B"))
            config["after_release"] = "0.1.1"
            with patch.object(nightly, "command", side_effect=local_command):
                incomplete = nightly.Release(config, root / "incomplete-state")
                self.assertIsNone(incomplete.prepare())
            self.assertIn("release assets", incomplete.state["reason"])
            config["after_release"] = "0.1.2"
            with patch.object(nightly, "command", side_effect=local_command):
                waiting = nightly.Release(config, root / "waiting-state")
                self.assertIsNone(waiting.prepare())
            self.assertEqual(waiting.state["status"], "skipped")
            self.assertIn("existing v0.1.2 release owner", waiting.state["reason"])


if __name__ == "__main__":
    unittest.main()
