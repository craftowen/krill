#!/usr/bin/env python3
"""publish-github.sh 게시 가드 회귀 테스트. 임시 git 저장소·로컬 bare origin·가짜 gh만 쓰고 네트워크·빌드는 없다."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent

FAKE_GH = """#!/bin/bash
# FAKE_RELEASE=1이면 릴리스가 있다. FAKE_CHECKS는 "conclusion<TAB>name" 줄들.
case "$1 $2" in
  "auth status") exit 0 ;;
  "release view") [[ "${FAKE_RELEASE:-0}" == 1 ]] && exit 0; echo "release not found" >&2; exit 1 ;;
  "api "*) printf '%b\\n' "${FAKE_CHECKS-success\\tci}"; exit 0 ;;
esac
echo "fake gh: unexpected $*" >&2; exit 99
"""


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


class PublishGuardTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="krill-publish-test-")
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        origin, self.work, bin_dir = root / "origin.git", root / "work", root / "bin"
        git(root, "init", "-q", "--bare", "-b", "main", str(origin))
        git(root, "init", "-q", "-b", "main", str(self.work))
        (self.work / "packaging").mkdir()
        shutil.copy2(HERE / "publish-github.sh", self.work / "packaging/publish-github.sh")
        (self.work / "packaging/bundle.sh").write_text("#!/bin/bash\nexit 1\n")
        (self.work / "docs/licensing").mkdir(parents=True)
        (self.work / "docs/licensing/generate_notices.py").write_text(
            "import os, sys\nsys.exit('stale' if os.environ.get('FAKE_STALE') else 0)\n")
        (self.work / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
        self.commit("init")
        git(self.work, "remote", "add", "origin", str(origin))
        git(self.work, "push", "-q", "origin", "main")
        bin_dir.mkdir()
        (bin_dir / "gh").write_text(FAKE_GH)
        (bin_dir / "gh").chmod(0o755)
        # 실제 게시 락(/tmp/krill-publish.lock)은 건드리지 않는다.
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", KRILL_PUBLISH_LOCKED="1")
        self.env.pop("KRILL_PUBLISH_FORCE", None)

    def commit(self, message):
        git(self.work, "add", "-A")
        git(self.work, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
            "commit", "-q", "-m", message)

    def publish(self, *args, **env):
        return subprocess.run(
            ["bash", "packaging/publish-github.sh", "--repo", "o/r", *args],
            cwd=self.work, env={**self.env, **env}, stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=60)

    def assert_blocked(self, result, message):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(message, result.stderr)

    def test_all_guards_pass(self):
        result = self.publish("--preflight")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("All publish guards passed", result.stdout)

    def test_remote_tag_blocks(self):
        git(self.work, "tag", "v0.1.0")
        git(self.work, "push", "-q", "origin", "v0.1.0")
        self.assert_blocked(self.publish("--preflight"), "tag v0.1.0 already exists on origin")

    def test_existing_release_blocks(self):
        self.assert_blocked(self.publish("--preflight", FAKE_RELEASE="1"),
                            "GitHub release v0.1.0 already exists")

    def test_head_not_origin_main_blocks(self):
        (self.work / "extra").write_text("x\n")
        self.commit("unpushed")
        self.assert_blocked(self.publish("--preflight"), "is not origin/main")

    def test_red_or_missing_ci_blocks(self):
        self.assert_blocked(self.publish("--preflight", FAKE_CHECKS=r"success\tfmt\nfailure\tclippy"),
                            "failure\tclippy")
        self.assert_blocked(self.publish("--preflight", FAKE_CHECKS=""), "no CI check-runs")

    def test_local_tag_elsewhere_blocks(self):
        git(self.work, "tag", "v0.1.0", "HEAD")
        (self.work / "extra").write_text("x\n")
        self.commit("next")
        git(self.work, "push", "-q", "origin", "main")
        self.assert_blocked(self.publish("--preflight"), "local tag v0.1.0 points at")

    def test_stale_notices_block(self):
        self.assert_blocked(self.publish("--preflight", FAKE_STALE="1"), "THIRD_PARTY_NOTICES.md is stale")

    def test_internal_docs_block(self):
        (self.work / "docs/boost-2026-01-01-x.md").write_text("internal\n")
        self.commit("leak")
        git(self.work, "push", "-q", "origin", "main")
        self.assert_blocked(self.publish("--preflight"), "docs/boost-2026-01-01-x.md")

    def test_bin_dir_is_dry_run_only(self):
        result = self.publish("--bin-dir", "/tmp")
        self.assertEqual(result.returncode, 2)
        self.assertIn("--bin-dir is dry-run only", result.stderr)

    def test_real_publish_stops_and_force_needs_a_terminal(self):
        red = {"FAKE_CHECKS": r"failure\tclippy"}
        self.assert_blocked(self.publish(**red), "KRILL_PUBLISH_FORCE='<reason>'")
        self.assert_blocked(self.publish(**red, KRILL_PUBLISH_FORCE="hotfix"), "needs an interactive terminal")
        # 이미 게시된 버전은 사유가 있어도 넘길 수 없다.
        self.assert_blocked(self.publish(FAKE_RELEASE="1", KRILL_PUBLISH_FORCE="hotfix"),
                            "cannot be overridden")

    def test_dry_run_only_adds_available_signed_windows_assets(self):
        w = self.work / "packaging"
        fakes = {
            "bundle.sh": 'o=$2; mkdir -p "$o"; echo zip > "$o/Krill-0.1.0-arm64-adhoc.zip"; '
                         'echo "$(printf a%.0s {1..64})  x" > "$o/Krill-0.1.0-arm64-adhoc.zip.sha256"',
            "release.sh": 'o=$6; mkdir -p "$o/homebrew-krill"; touch "$o/Krill-0.1.0.dmg" '
                          '"$o/Krill-0.1.0-source.tar.gz" "$o/homebrew-krill/SHA256SUMS"',
            "install.sh": "", "install-linux.sh": "", "windows/install.ps1": "",
            # VERSION PUBLIC_KEY UPDATE_URL OUT_DIR
            "windows/build-vm.sh": 'echo "$2 $3" > "$4/vm-args"; echo zip > "$4/Krill-$1-windows-x64.zip"; '
                                   'echo "$(printf b%.0s {1..64})  x" > "$4/Krill-$1-windows-x64.zip.sha256"',
        }
        for name, body in fakes.items():
            (w / name).parent.mkdir(parents=True, exist_ok=True)
            (w / name).write_text(f"#!/bin/bash\nset -e\n{body}\n")
        (w / "macos").mkdir()
        shutil.copy2(HERE / "macos/ed25519_manifest.py", w / "macos/ed25519_manifest.py")
        (self.work / "crates/krill-proto/src").mkdir(parents=True)
        (self.work / "crates/krill-proto/src/ipc.rs").write_text("pub const PROTO_MIN: u16 = 3;\n")
        key = self.work.parent / "update.key"
        key.write_text(os.urandom(32).hex() + "\n")
        result = self.publish("--dry-run", KRILL_WIN_VM_DIR="", KRILL_UPDATE_KEY_FILE=str(key))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("no Windows zip", result.stderr)
        for name in ("Krill-0.1.0-windows-x64.zip", "latest-windows.json", "install.ps1"):
            self.assertNotIn(f"packaging/dist/{name}\n", result.stdout)
        result = self.publish("--dry-run", KRILL_WIN_VM_DIR="/unused", KRILL_UPDATE_KEY_FILE=str(key))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        dist = self.work / "packaging/dist"
        for asset in ("Krill-0.1.0-windows-x64.zip", "Krill-0.1.0-windows-x64.zip.sha256",
                      "latest-windows.json", "install.ps1"):
            self.assertIn(f"packaging/dist/{asset}\n", result.stdout)
        pub, url = (dist / "vm-args").read_text().split()
        self.assertEqual(pub, (key.parent / "update.key.pub").read_text().strip())
        self.assertEqual(url, "https://github.com/o/r/releases/latest/download/latest-windows.json")
        manifest = json.loads((dist / "latest-windows.json").read_text())
        self.assertEqual(manifest["url"],
                         "https://github.com/o/r/releases/download/v0.1.0/Krill-0.1.0-windows-x64.zip")
        self.assertEqual((manifest["sha256"], manifest["min_daemon_protocol"]), ("b" * 64, 3))
        self.assertEqual(json.loads((dist / "latest.json").read_text())["sha256"], "a" * 64)

    def test_script_never_uploads_into_existing_release(self):
        script = (HERE / "publish-github.sh").read_text()
        self.assertNotIn("gh release upload", script)


if __name__ == "__main__":
    unittest.main()
