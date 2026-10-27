#!/usr/bin/env python3
"""사용자 지시에 따른 원격 작업 허용과 공개 표면 차단을 로컬 저장소에서 검증한다."""

import json
import os
from pathlib import Path
import shutil
import secrets
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


class HookTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="krill-hooks-test-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.work = self.root / "work"
        self.origin = self.root / "origin.git"
        self.env = dict(os.environ)
        for name in ("KRILL_ALLOW_PROTECTED_PUSH", "KRILL_ALLOW_REMOTE", "KRILL_HARNESS_APPROVED"):
            self.env.pop(name, None)
        self.git(self.root, "init", "-q", "--bare", "-b", "main", str(self.origin))
        self.git(self.root, "init", "-q", "-b", "main", str(self.work))
        self.git(self.work, "config", "user.name", "Hook test")
        self.git(self.work, "config", "user.email", "test@example.invalid")
        (self.work / "scripts/hooks").mkdir(parents=True)
        for rel in ("scripts/hooks/pre-push", "scripts/public-surface.sh", ".gitleaks.toml"):
            shutil.copy2(ROOT / rel, self.work / rel)
        self.git(self.work, "config", "core.hooksPath", "scripts/hooks")
        self.git(self.work, "remote", "add", "origin", str(self.origin))
        self.commit()

    def git(self, cwd, *args, **kwargs):
        return subprocess.run(
            ["git", *args], cwd=cwd, env=self.env, capture_output=True,
            text=True, check=True, timeout=30, **kwargs)

    def commit(self):
        self.git(self.work, "add", ".")
        self.git(self.work, "commit", "-qm", "fixture")

    def push(self, ref, reason=None):
        env = dict(self.env)
        if reason is not None:
            env["KRILL_ALLOW_PROTECTED_PUSH"] = reason
        return subprocess.run(
            ["git", "push", "origin", f"HEAD:{ref}"], cwd=self.work,
            env=env, capture_output=True, text=True, timeout=30)

    def test_protected_refs_require_reason(self):
        for ref in ("refs/heads/main", "refs/tags/v0.1.0"):
            for reason in (None, "", " \t "):
                with self.subTest(ref=ref, reason=reason):
                    result = self.push(ref, reason)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("승인 사유가 필요", result.stderr)
        self.assertEqual(self.git(self.root, "--git-dir", str(self.origin),
                                  "for-each-ref", "--format=%(refname)").stdout, "")

    def test_authorized_main_and_release_tag_push(self):
        for ref in ("refs/heads/main", "refs/tags/v0.1.0"):
            with self.subTest(ref=ref):
                result = self.push(ref, "사용자 배포 지시: 검증 후보 게시")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("사용자 배포 지시", result.stderr)
                self.assertIn("public-surface: ok", result.stdout + result.stderr)
                remote = self.git(self.root, "--git-dir", str(self.origin), "rev-parse", ref)
                self.assertEqual(remote.stdout, self.git(self.work, "rev-parse", "HEAD").stdout)

    def test_authorization_preserves_public_surface_guard(self):
        (self.work / "docs").mkdir()
        (self.work / "docs/unapproved.md").write_text("internal fixture\n")
        self.commit()
        result = self.push("refs/heads/main", "사용자 배포 지시")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("공개 표면 점검 실패", result.stderr)

    def test_annotated_tags_check_the_peeled_commit(self):
        env = dict(self.env, KRILL_ALLOW_PROTECTED_PUSH="isolated annotated-tag fixture")
        self.git(self.work, "-c", "tag.gpgsign=false", "tag", "-a", "v0.1.0", "-m", "fixture")
        allowed = subprocess.run(
            ["git", "push", "origin", "refs/tags/v0.1.0"], cwd=self.work,
            env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(allowed.returncode, 0, allowed.stderr)
        self.assertIn("public-surface: ok", allowed.stdout + allowed.stderr)

        (self.work / "docs").mkdir()
        (self.work / "docs/unapproved.md").write_text("internal fixture\n")
        self.commit()
        self.git(self.work, "-c", "tag.gpgsign=false", "tag", "-a", "v0.1.1", "-m", "fixture")
        blocked = subprocess.run(
            ["git", "push", "origin", "refs/tags/v0.1.1"], cwd=self.work,
            env=env, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(blocked.returncode, 0)
        self.assertIn("공개 표면 점검 실패", blocked.stderr)
        refs = self.git(self.root, "--git-dir", str(self.origin), "for-each-ref",
                        "--format=%(refname)").stdout.splitlines()
        self.assertEqual(refs, ["refs/tags/v0.1.0"])

    def test_feature_branch_needs_no_protected_ref_reason(self):
        result = self.push("refs/heads/feature")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ci_refuses_missing_gitleaks(self):
        empty_path = self.root / "empty-path"
        empty_path.mkdir()
        result = subprocess.run(
            ["/bin/bash", str(ROOT / "scripts/public-surface.sh"), str(self.work)],
            env={**self.env, "CI": "true", "PATH": str(empty_path)},
            capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CI requires gitleaks", result.stderr)

    def test_other_token_in_websocket_fixture_is_not_allowlisted(self):
        source = self.work / "crates/krilld/src/mobile/ws.rs"
        source.parent.mkdir(parents=True)
        source.write_text(f'api_key = "{secrets.token_hex(32)}"\n')
        self.commit()
        result = self.push("refs/heads/feature")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("공개 표면 점검 실패", result.stderr)

    def guard(self, command=None, path=None, **env):
        payload = {"cwd": str(self.work), "tool_name": "Bash" if command is not None else "Edit",
                   "tool_input": {"command": command} if command is not None else {"file_path": path}}
        return subprocess.run(
            ["bash", str(ROOT / ".claude/hooks/guard.sh")], cwd=self.work,
            env={**self.env, **env}, input=json.dumps(payload),
            capture_output=True, text=True, timeout=30)

    def test_claude_remote_commands_require_user_instruction_record(self):
        for command in ("git push origin HEAD:main", "gh release create v0.1.0",
                        "bash packaging/publish-github.sh", "gh api repos/o/r -X POST"):
            with self.subTest(command=command):
                self.assertEqual(self.guard(command).returncode, 2)
                allowed = self.guard("# KRILL_ALLOW_REMOTE: 사용자 배포 지시\n" + command)
                self.assertEqual(allowed.returncode, 0, allowed.stderr)
                self.assertIn("사용자 배포 지시", allowed.stderr)

    def test_claude_empty_or_misplaced_record_is_rejected(self):
        for prefix in ("# KRILL_ALLOW_REMOTE:\n", "# KRILL_ALLOW_REMOTE: \t \n",
                       "true\n# KRILL_ALLOW_REMOTE: user requested deployment\n"):
            with self.subTest(prefix=prefix):
                self.assertEqual(self.guard(prefix + "git push origin main").returncode, 2)

    def test_claude_local_and_existing_session_authorization(self):
        self.assertEqual(self.guard("git status --short").returncode, 0)
        self.assertEqual(self.guard("git push origin main", KRILL_ALLOW_REMOTE="1").returncode, 0)

    def test_remote_authorization_does_not_authorize_harness_edits(self):
        path = str(self.work / "AGENTS.md")
        self.assertEqual(self.guard(path=path, KRILL_ALLOW_REMOTE="1").returncode, 2)
        self.assertEqual(self.guard(path=path, KRILL_HARNESS_APPROVED="1").returncode, 0)


if __name__ == "__main__":
    unittest.main()
