"""Packaging contracts with fake Cargo/signature tools; no build or keychain access."""

import json
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
PACKAGING = HERE.parent
NAMES = ("krill", "krill-holder", "krilld")


def executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


class SourceInstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="krill-source-contract ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "checkout with spaces"
        (self.source / "crates/krill").mkdir(parents=True)
        (self.source / "Cargo.toml").write_text("[workspace]\n")
        self.target = self.root / "external target with spaces/release"
        self.target.mkdir(parents=True)
        self.prefix = self.root / "installation"
        self.fake = self.root / "tools"
        self.fake.mkdir()
        executable(self.fake / "uname", '#!/bin/sh\nif [ "$1" = -s ]; then echo Darwin; else echo arm64; fi\n')
        for name in ("curl", "gh"):
            executable(self.fake / name, "#!" + sys.executable + "\n"
                       "import os,pathlib,sys\n"
                       "pathlib.Path(os.environ['FAKE_NETWORK_CALL']).write_text(sys.argv[0])\n"
                       "sys.exit(85)\n")
        self.artifacts = []
        for name in NAMES:
            artifact = self.target / name
            executable(artifact, f"#!/bin/sh\n# {name} fixture\n")
            self.artifacts.append({"reason": "compiler-artifact", "target": {"name": name},
                                   "executable": str(artifact), "fresh": True})
        executable(self.fake / "cargo", "#!" + sys.executable + "\n" + r"""
import json, os, pathlib, sys
assert '--message-format=json' in sys.argv
assert '--locked' in sys.argv and '--release' in sys.argv
assert pathlib.Path.cwd() == pathlib.Path(os.environ['FAKE_SOURCE'])
pathlib.Path(os.environ['FAKE_CALL']).write_text(json.dumps(sys.argv))
print(os.environ['FAKE_ARTIFACTS'])
print('fake compiler diagnostic', file=sys.stderr)
sys.exit(int(os.environ.get('FAKE_STATUS', '0')))
""")
        self.env = dict(os.environ, PATH=str(self.fake) + os.pathsep + os.environ['PATH'],
                        FAKE_SOURCE=str(self.source), FAKE_CALL=str(self.root / "cargo-call"),
                        FAKE_NETWORK_CALL=str(self.root / "network-call"),
                        CARGO_TARGET_DIR=str(self.target.parent))

    def install(self, **env):
        return subprocess.run(["bash", str(PACKAGING / "install.sh"), "--source-dir", self.source.name,
                               "--prefix", str(self.prefix), "--no-open"], cwd=self.root,
                              env=dict(self.env, FAKE_ARTIFACTS="\n".join(map(json.dumps, self.artifacts)), **env),
                              capture_output=True, text=True, timeout=20)

    def test_external_target_with_spaces_and_relative_source(self):
        self.artifacts.insert(0, {"reason": "build-script-executed"})
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in NAMES:
            installed = self.prefix / "bin" / name
            self.assertEqual(installed.read_bytes(), (self.target / name).read_bytes())
            self.assertEqual(installed.stat().st_mode & 0o777, 0o755)
        self.assertFalse((self.source / "target").exists())
        self.assertIn("CLI-only", result.stdout)
        self.assertIn("fake compiler diagnostic", result.stderr)

    def test_reported_path_without_target_env(self):
        self.env.pop("CARGO_TARGET_DIR")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.prefix / "bin/krill").is_file())

    def test_failed_build_never_installs_artifacts(self):
        result = self.install(FAKE_STATUS="17")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / "cargo-call").exists(), "must reach the failing compiler fixture")
        self.assertFalse(self.prefix.exists())

    def test_missing_reported_artifact_never_partially_installs(self):
        self.artifacts.pop()
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cargo did not report an executable for krilld", result.stderr)
        self.assertFalse(self.prefix.exists())

    def test_reported_nonexistent_artifact_is_rejected(self):
        (self.target / "krilld").unlink()
        self.assertNotEqual(self.install().returncode, 0)
        self.assertFalse(self.prefix.exists())

    def test_nonexecutable_artifact_is_rejected(self):
        (self.target / "krilld").chmod(0o644)
        self.assertNotEqual(self.install().returncode, 0)
        self.assertFalse(self.prefix.exists())

    def test_last_destination_conflict_preserves_everything(self):
        (self.prefix / "bin").mkdir(parents=True)
        conflict = self.prefix / "bin/krilld"
        conflict.write_text("keep me")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to overwrite", result.stderr)
        self.assertEqual(conflict.read_text(), "keep me")
        self.assertFalse((self.prefix / "bin/krill").exists())

    def test_dangling_destination_symlink_is_preserved(self):
        (self.prefix / "bin").mkdir(parents=True)
        conflict = self.prefix / "bin/krilld"
        conflict.symlink_to(self.root / "missing")
        self.assertNotEqual(self.install().returncode, 0)
        self.assertTrue(conflict.is_symlink())
        self.assertFalse((self.prefix / "bin/krill").exists())

    def test_dirty_checkout_and_worktree_are_rejected_before_build(self):
        executable(self.fake / "git", "#!/bin/sh\necho ' M Cargo.toml'\n")
        for marker in ("directory", "file"):
            with self.subTest(marker=marker):
                git = self.source / ".git"
                if marker == "directory":
                    git.mkdir()
                else:
                    git.write_text("gitdir: /fake/worktree")
                try:
                    result = self.install()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("dirty checkout, refusing", result.stderr)
                    self.assertFalse((self.root / "cargo-call").exists())
                finally:
                    if git.is_dir():
                        git.rmdir()
                    else:
                        git.unlink()

    def test_git_status_failure_stops_before_build(self):
        (self.source / ".git").mkdir()
        call = self.root / "git-call"
        executable(self.fake / "git", '#!/bin/sh\nprintf "%s\\n" "$@" > "$FAKE_GIT_CALL"\nexit 4\n')
        result = self.install(FAKE_GIT_CALL=str(call))
        self.assertEqual(result.returncode, 2)
        self.assertEqual(call.read_text().splitlines(), ["-C", str(self.source), "status", "--porcelain"])
        self.assertFalse((self.root / "cargo-call").exists())

    def test_intel_release_is_unsupported_before_network_or_install(self):
        executable(self.fake / "uname", '#!/bin/sh\nif [ "$1" = -s ]; then echo Darwin; else echo x86_64; fi\n')
        result = subprocess.run(["bash", str(PACKAGING / "install.sh"), "--release", "--no-open",
                                 "--prefix", str(self.prefix), "--app-dir", str(self.root / "apps")],
                                env=self.env, capture_output=True, text=True, timeout=20)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Krill supports Apple Silicon Macs only", result.stderr)
        self.assertFalse((self.root / "cargo-call").exists())
        self.assertFalse((self.root / "network-call").exists())
        self.assertFalse(self.prefix.exists())
        self.assertFalse((self.root / "apps").exists())


class VerificationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="krill-verify-contract ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.log = self.root / "calls.jsonl"
        self.stub = self.root / "tool"
        executable(self.stub, "#!" + sys.executable + "\n" + r"""
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ['TOOL_LOG'], 'a') as log:
    log.write(json.dumps([name, *args]) + '\n')
if name == 'plutil' and '-extract' in args:
    print('dev.krill.app' if 'CFBundleIdentifier' in args else 'krill-app')
if name == 'codesign' and '-d' in args:
    filename = pathlib.Path(args[-1]).name
    suffix = {'krill-holder':'holder', 'krilld':'krilld', 'krill-link':'link', 'krill':'cli'}.get(filename)
    print('Identifier=dev.krill.app' + ('.' + suffix if suffix else ''))
    if os.environ.get('MODE') != 'local':
        print('Authority=Developer ID Application: Example (ABCDE12345)')
        if os.environ.get('MODE') != 'no-runtime':
            print('CodeDirectory v=20500 size=123 flags=0x10000(runtime) hashes=10')
        if os.environ.get('MODE') != 'no-timestamp':
            print('Timestamp=Oct 23, 2026 at 12:00:00')
        print('TeamIdentifier=' + ('ZZZZZ12345' if os.environ.get('MODE') == 'wrong-team' and filename == 'krill-app' else 'ABCDE12345'))
if name == 'codesign' and any(a.startswith('-R=') for a in args) and os.environ.get('MODE') == 'bad-anchor':
    sys.exit(1)
if name == os.environ.get('FAIL_TOOL'):
    sys.exit(1)
""")
        text = (HERE / "verify.sh").read_text()
        for tool, original in (("codesign", "/usr/bin/codesign"), ("plutil", "/usr/bin/plutil"),
                               ("spctl", "/usr/sbin/spctl"), ("xcrun", "/usr/bin/xcrun")):
            link = self.root / tool
            link.symlink_to(self.stub)
            text = text.replace(original, shlex.quote(str(link)))
        self.script = self.root / "verify.sh"
        self.script.write_text(text)

    def verify(self, *args, **env):
        return subprocess.run(["bash", str(self.script), *args, str(self.root / "Krill.app")],
                              env=dict(os.environ, TOOL_LOG=str(self.log), **env),
                              capture_output=True, text=True, timeout=20)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_default_verification_does_not_assess_gatekeeper(self):
        result = self.verify(MODE="local")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(call[0] in ("spctl", "xcrun") for call in self.calls()))

    def test_notarized_requires_apple_chain_runtime_timestamp_and_ticket(self):
        result = self.verify("--notarized")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertEqual(sum('-R=anchor apple generic' in call for call in calls), 5)
        self.assertIn(["spctl", "--assess", "--type", "execute", "--verbose=2", str(self.root / "Krill.app")], calls)
        self.assertIn(["xcrun", "stapler", "validate", str(self.root / "Krill.app")], calls)

    def test_notarized_rejects_missing_protections_and_team_mismatch(self):
        for mode in ("local", "no-runtime", "no-timestamp", "wrong-team", "bad-anchor"):
            with self.subTest(mode=mode):
                self.assertNotEqual(self.verify("--notarized", MODE=mode).returncode, 0)

    def test_notarized_propagates_gatekeeper_and_ticket_failure(self):
        for tool in ("spctl", "xcrun"):
            with self.subTest(tool=tool):
                self.assertNotEqual(self.verify("--notarized", FAIL_TOOL=tool).returncode, 0)


@unittest.skipUnless(sys.platform == "darwin", "plutil/ditto are required")
class LocalizedBundleTests(unittest.TestCase):
    def test_localizations_are_sealed_in_mock_signed_bundle_and_zip(self):
        with tempfile.TemporaryDirectory(prefix="krill-bundle-contract ") as temporary:
            root = Path(temporary)
            packaging = root / "packaging"
            macos = packaging / "macos"
            shutil.copytree(HERE, macos, ignore=shutil.ignore_patterns("__pycache__", "test_*.py"))
            (root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
            bins = root / "binaries"
            bins.mkdir()
            for name in (*NAMES, "krill-app", "krill-link"):
                executable(bins / name, "#!/bin/sh\nexit 0\n")
            fake = root / "tools"
            fake.mkdir()
            executable(fake / "file", "#!/bin/sh\necho 'Mach-O executable'\n")
            executable(fake / "lipo", "#!/bin/sh\necho arm64\n")
            executable(fake / "security", "#!/bin/sh\ncase \"$1\" in\nfind-identity) echo '\"Krill Local Code Signing\"';;\nfind-certificate) echo 'SHA-1 hash: " + "a" * 40 + "';;\nunlock-keychain) ;;\n*) exit 90;;\nesac\n")
            executable(fake / "codesign", "#!" + sys.executable + "\n" + r"""
from pathlib import Path
import sys
if '-d' in sys.argv:
    name = Path(sys.argv[-1]).name
    suffix = {'krill-holder':'holder', 'krilld':'krilld', 'krill-link':'link', 'krill':'cli'}.get(name)
    print('Identifier=dev.krill.app' + ('.' + suffix if suffix else ''))
if '--force' in sys.argv:
    # All permission resources must already be present when signing begins.
    app = next(p for p in [Path(sys.argv[-1]), *Path(sys.argv[-1]).parents] if p.name == 'Krill.app')
    assert (app / 'Contents/Resources/en.lproj/InfoPlist.strings').is_file()
    assert (app / 'Contents/Resources/ko.lproj/InfoPlist.strings').is_file()
""")
            # No keys are generated and no security operation reaches the real keychain.
            executable(macos / "ed25519_manifest.py", "#!" + sys.executable + "\nprint('" + "b" * 64 + "')\n")
            keychain = root / "fake.keychain-db"
            keychain.touch()
            Path(str(keychain) + ".pass").write_text("fixture")
            replacements = {"/usr/bin/" + name: shlex.quote(str(fake / name))
                            for name in ("file", "lipo", "security", "codesign")}
            for source, target in ((PACKAGING / "bundle.sh", packaging / "bundle.sh"),
                                   (HERE / "verify.sh", macos / "verify.sh")):
                text = source.read_text()
                for original, replacement in replacements.items():
                    text = text.replace(original, replacement)
                executable(target, text)
            output = root / "output"
            result = subprocess.run(["bash", str(packaging / "bundle.sh"), "--bin-dir", str(bins),
                                     "--repo", "example/krill", "--output", str(output)],
                                    env=dict(os.environ, KRILL_CODESIGN_KEYCHAIN=str(keychain)),
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            app = output / "Krill.app"
            plist = plistlib.loads((app / "Contents/Info.plist").read_bytes())
            self.assertEqual(plist["CFBundleDevelopmentRegion"], "ko")
            self.assertEqual(plist["CFBundleLocalizations"], ["en", "ko"])
            self.assertEqual(plist["KrillUpdatePublicKey"], "b" * 64)
            self.assertEqual(plist["KrillUpdateDesignatedRequirement"],
                             'identifier "dev.krill.app" and certificate root = H"' + "a" * 40 + '"')
            archive, = output.glob("*.zip")
            unpacked = root / "unpacked"
            subprocess.run(["/usr/bin/ditto", "-x", "-k", str(archive), str(unpacked)], check=True)
            for language in ("en", "ko"):
                relative = Path("Contents/Resources") / (language + ".lproj") / "InfoPlist.strings"
                self.assertEqual((app / relative).read_bytes(), (unpacked / "Krill.app" / relative).read_bytes())
                contents = subprocess.check_output(["/usr/bin/plutil", "-convert", "xml1", "-o", "-", str(app / relative)])
                localized = plistlib.loads(contents)
                self.assertEqual(set(localized), {"NSDocumentsFolderUsageDescription", "NSDesktopFolderUsageDescription", "NSDownloadsFolderUsageDescription"})
                for key, value in localized.items():
                    self.assertTrue(value)
                    if language == "ko":
                        self.assertEqual(value, plist[key])
                    else:
                        self.assertTrue(value.isascii())
                        self.assertNotEqual(value, plist[key])


if __name__ == "__main__":
    unittest.main(verbosity=2)
