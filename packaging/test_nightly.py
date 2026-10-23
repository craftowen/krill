#!/usr/bin/env python3
"""야간 배포의 실패 차단·커밋 고정·자산 출처 검증. 실제 빌드·원격 쓰기는 하지 않는다."""

import hashlib
import io
import os
import signal
import sys
import threading
import time
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

# 임시 저장소가 사용자 전역 git 설정(예: tag.gpgSign=true면 경량 태그가 실패)을 따르지 않게 한다.
os.environ["GIT_CONFIG_GLOBAL"] = os.devnull

import nightly
from nightly_assets import download_artifact, import_assets

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
        for platform in ("windows",):
            (self.assets / f"source-commit-{platform}.txt").write_text(SHA + "\r\n")
        self.windows = self.assets / "Krill-0.1.2-windows-x64.zip"
        self.write_zip(PUB)

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
        self.assertEqual(len(list(final.iterdir())), 3)
        self.assertFalse(any("linux" in path.name for path in final.iterdir()))

    def test_wrong_head_does_not_copy_any_assets(self):
        (self.assets / "source-commit-windows.txt").write_text(BASE)
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
        downloader = patch("nightly_assets.download_artifact", return_value={"id": 1})
        self.downloader = downloader.start()
        self.addCleanup(downloader.stop)

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
        if "packaging/publish-github.sh" in args:
            nightly.write_json(self.release.root / "publications" / SHA / "public-verification.json",
                {"head": SHA, "tag": "v0.1.2", "verified_assets": [str(n) for n in range(11)]})
        return ""

    def fake_ci(self, _sha, branch, _workflows):
        self.calls.append(("wait-ci", branch))
        if self.failure == "ci" or self.failure == "main-ci" and branch == "main":
            raise nightly.NightlyError("injected CI failure")
        return {"release.yml": {"run_id": 123, "attempt": 2}}

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
        self.assertIn("validation_receipt", self.release.state)
        publish = next(i for i, call in enumerate(self.calls) if "packaging/publish-github.sh" in call)
        self.assertLess(self.calls.index(("wait-ci", "main")), publish)
        dispatch = next(i for i, call in enumerate(self.calls) if call[:3] == ("gh", "workflow", "run"))
        validate = next(i for i, call in enumerate(self.calls) if "validate" in call)
        self.assertLess(dispatch, validate)
        self.assertLess(validate, self.calls.index(("wait-ci", "release/nightly-test")))
        self.assertTrue(any(any(arg.startswith("--force-with-lease=") for arg in call) for call in self.calls if call[:1] == ("git",) and len(call) > 1))
        self.assertEqual(self.downloader.call_count, 1)
        self.assertTrue(all(call.args[3].endswith("-attempt-2") for call in self.downloader.call_args_list))

    def test_resumed_failed_ci_is_restarted_once_without_failed_job_filter(self):
        original = self.release.git
        self.release.git = lambda *args: (SHA + "\trefs/heads/release/nightly-test" if args[:1] == ("ls-remote",) else original(*args))
        failed = nightly.FailedRun({"id": 12, "conclusion": "failure"})
        with patch.object(nightly, "Evidence") as evidence:
            evidence.return_value.proof.side_effect = [failed, nightly.Pending("running"), {"run_id": 123, "attempt": 2}]
            self.release.publish()
        reruns = [call for call in self.calls if call[:3] == ("gh", "run", "rerun")]
        self.assertEqual(len(reruns), 1)
        self.assertNotIn("--failed", reruns[0])
        self.assertFalse(any(call[:3] == ("gh", "workflow", "run") for call in self.calls))

    def test_readback_failure_cannot_mark_published(self):
        original = self.release.run
        def missing(*args, **kwargs):
            result = original(*args, **kwargs)
            if "packaging/publish-github.sh" in args:
                (self.release.root / "publications" / SHA / "public-verification.json").unlink()
            return result
        self.release.run = missing
        with self.assertRaises(FileNotFoundError):
            self.release.publish()
        self.assertNotEqual(self.release.state["status"], "published")

    def test_any_local_gate_or_native_ci_failure_prevents_promotion_and_publication(self):
        for failure in ("packaging/test_nightly.py", "packaging/release_check.py", "ci"):
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
        for field, value in (("main", "d" * 40), ("current", BASE), ("dirty", " M Cargo.toml")):
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
            with patch.object(nightly, "command", side_effect=local_command):
                resumed = nightly.Release(config, root / "state")
                self.assertEqual(resumed.prepare()[0], sha)
                self.assertTrue(resumed.state["resumed"])
                pending = root / "state/pending.json"
                data = json.loads(pending.read_text())
                data["created_at"] = "2000-01-01T00:00:00+00:00"
                nightly.write_json(pending, data)
                renewed = nightly.Release(config, root / "state")
                self.assertNotEqual(renewed.prepare()[0], sha)
                self.assertFalse(renewed.state.get("resumed", False))

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


class ArtifactTests(unittest.TestCase):
    def test_artifact_is_bound_to_attempt_id_head_and_github_digest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            buffer = io.BytesIO()
            with zipfile.ZipFile(buffer, 'w') as archive:
                archive.writestr('source-commit-windows.txt', SHA)
            raw = buffer.getvalue()
            artifact = {'id': 8, 'name': 'windows-attempt-2', 'expired': False,
                        'digest': 'sha256:' + hashlib.sha256(raw).hexdigest(),
                        'workflow_run': {'id': 123, 'head_sha': SHA}}
            def read(*_args, **_kwargs): return [{'artifacts': [artifact]}]
            def fetch(identifier, stream):
                self.assertEqual(identifier, 8)
                stream.write(raw)
            proof = download_artifact('o/r', 123, 2, artifact['name'], root / 'valid', SHA, read, fetch)
            self.assertEqual(proof['attempt'], 2)
            self.assertEqual((root / 'valid/source-commit-windows.txt').read_text(), SHA)
            for field, value in (('expired', True), ('digest', 'sha256:' + '0' * 64),
                                 ('workflow_run', {'id': 123, 'head_sha': BASE})):
                old = artifact[field]
                artifact[field] = value
                with self.subTest(field=field), self.assertRaises(ValueError):
                    download_artifact('o/r', 123, 2, artifact['name'], root / field, SHA, read, fetch)
                artifact[field] = old
            with self.assertRaises(ValueError):
                download_artifact('o/r', 123, 3, artifact['name'], root / 'attempt', SHA, read, fetch)
            buffer = io.BytesIO()
            with zipfile.ZipFile(buffer, 'w') as archive: archive.writestr('../outside', b'unsafe')
            raw = buffer.getvalue()
            artifact['digest'] = 'sha256:' + hashlib.sha256(raw).hexdigest()
            with self.assertRaisesRegex(ValueError, 'unsafe'):
                download_artifact('o/r', 123, 2, artifact['name'], root / 'unsafe', SHA, read, fetch)
            self.assertFalse((root / 'outside').exists())


class RecoveryTests(unittest.TestCase):
    def test_remote_publication_readback_resumes_from_preserved_plan_and_never_skips_partial_release(self):
        for valid in (True, False):
            with self.subTest(valid=valid), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                config = {'repo': 'o/r', 'branch': 'dev', 'run_root': str(root), 'target_dir': '/unused', 'key_file': '/unused'}
                release = nightly.Release(config, root / 'state')
                release.work.mkdir()
                release.state['public_key'] = PUB
                pending = release.state_dir / 'pending.json'
                nightly.write_json(pending, {'candidate_head': SHA, 'previous_release': 'v0.1.1'})
                audit = root / 'publications' / SHA
                nightly.write_json(audit / 'publication.json', {'signed': 'retained outside clone'})
                def git(*args):
                    if args[:2] == ('config', '--get'): return 'true'
                    if args[:1] == ('rev-parse',): return SHA
                    return ''
                release.git = git
                release.ancestor = lambda *_args: True
                def run(*args, **_kwargs):
                    if args[:3] == ('gh', 'release', 'view'):
                        return json.dumps({'tagName': 'v0.1.2', 'assets': []})
                    if 'packaging/release_verify.py' in args:
                        if not valid: raise nightly.NightlyError('partial remote publication')
                        nightly.write_json(audit / 'public-verification.json', {'head': SHA, 'tag': 'v0.1.2',
                                           'verified_assets': list(range(11))})
                    return ''
                release.run = run
                if valid:
                    self.assertIsNone(release.prepare())
                    self.assertEqual(release.state['status'], 'published')
                    self.assertFalse(pending.exists())
                else:
                    with self.assertRaisesRegex(nightly.NightlyError, 'partial'):
                        release.prepare()
                    self.assertNotEqual(release.state['status'], 'published')
                    self.assertTrue(pending.exists())
                self.assertTrue((audit / 'publication.json').exists())

    @unittest.skipUnless(sys.platform == 'darwin', 'BSD lockf interoperation')
    def test_cancellation_reaps_owned_child_before_releasing_shared_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = root / 'config.json'
            config.write_text('{}')
            lock = root / 'publish.lock'
            marker = root / 'child.pid'
            previous_handler = signal.getsignal(signal.SIGTERM)
            def cancel():
                for _ in range(100):
                    if marker.exists():
                        os.kill(os.getpid(), signal.SIGTERM)
                        return
                    time.sleep(0.02)
            def work(*_args):
                blocked = subprocess.run(['/usr/bin/lockf', '-k', '-t', '0', str(lock), '/usr/bin/true'], capture_output=True)
                self.assertEqual(blocked.returncode, 75)
                nightly.command([sys.executable, '-c',
                    'import os,time; from pathlib import Path; Path(' + repr(str(marker)) + ').write_text(str(os.getpid())); time.sleep(15)'], timeout=5)
            timer = threading.Thread(target=cancel)
            timer.start()
            try:
                with patch.object(nightly, 'PUBLISH_LOCK', lock), patch.object(nightly, 'run_release', side_effect=work), \
                        patch.object(sys, 'argv', ['nightly.py', 'run', '--config', str(config)]):
                    with self.assertRaises(SystemExit): nightly.main()
                pid = int(marker.read_text())
                with self.assertRaises(ProcessLookupError): os.kill(pid, 0)
                unlocked = subprocess.run(['/usr/bin/lockf', '-k', '-t', '0', str(lock), '/usr/bin/true'], capture_output=True)
                self.assertEqual(unlocked.returncode, 0)
            finally:
                signal.signal(signal.SIGTERM, previous_handler)
                timer.join()


if __name__ == "__main__":
    unittest.main()
