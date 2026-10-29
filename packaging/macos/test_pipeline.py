"""실제 Mach-O·codesign·ditto로 패키징 성공 및 변조 거부를 재현한다."""

import hashlib
import os
import pathlib
import plistlib
import re
import shutil
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = pathlib.Path(__file__).resolve().parent


@unittest.skipUnless(sys.platform == "darwin", "macOS codesign is required")
class PipelineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = tempfile.TemporaryDirectory(prefix="mk061-fixture-")
        cls.root = pathlib.Path(cls.fixture.name)
        cls.bins = cls.root / "bins"
        cls.bins.mkdir()
        cls.keychain = cls.root / "test-codesign.keychain-db"
        subprocess.run(
            ["/usr/bin/clang", "-x", "c", "-", "-o", str(cls.bins / "krill-app")],
            input=b"int main(void) { return 0; }\n",
            check=True,
        )
        for name in ("krill", "krill-holder", "krilld", "krill-link"):
            shutil.copy2(cls.bins / "krill-app", cls.bins / name)
        if os.environ.get("GITHUB_ACTIONS") == "true":
            # Hosted runners need the temporary private-key keychain in their search list,
            # even with codesign --keychain. This changes only the disposable CI user.
            original = shlex.split(subprocess.check_output(
                ["/usr/bin/security", "list-keychains", "-d", "user"], text=True))
            cls.addClassCleanup(subprocess.run,
                ["/usr/bin/security", "list-keychains", "-d", "user", "-s", *original],
                capture_output=True, check=True)
            cls.addClassCleanup(cls.fixture.cleanup)
            bootstrap = subprocess.run(
                [str(HERE.parent / "bundle.sh"), "--bin-dir", str(cls.bins), "--output", str(cls.root / "bootstrap")],
                env=dict(os.environ, KRILL_CODESIGN_KEYCHAIN=str(cls.keychain)),
                capture_output=True, text=True, timeout=60)
            if bootstrap.returncode and "The specified item could not be found in the keychain" not in bootstrap.stderr:
                raise RuntimeError(bootstrap.stdout + bootstrap.stderr)
            subprocess.run(["/usr/bin/security", "list-keychains", "-d", "user", "-s", str(cls.keychain), *original],
                           capture_output=True, check=True)

    @classmethod
    def tearDownClass(cls):
        if cls.keychain.exists():
            subprocess.run(
                ["/usr/bin/security", "delete-keychain", str(cls.keychain)],
                capture_output=True,
            )
        cls.fixture.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="mk061-output with spaces-")
        self.addCleanup(self.temp.cleanup)
        self.output = pathlib.Path(self.temp.name)
        self.app = self.output / "Krill.app"

    def package(self, bins=None, extra_args=()):
        env = dict(os.environ, KRILL_CODESIGN_KEYCHAIN=str(self.keychain))
        return subprocess.run(
            [
                str(HERE.parent / "bundle.sh"),
                "--bin-dir", str(bins or self.bins),
                "--output", str(self.output),
                *map(str, extra_args),
            ],
            env=env,
            capture_output=True, text=True,
        )

    def verified_package(self):
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def verify(self):
        return subprocess.run(
            [str(HERE / "verify.sh"), str(self.app)],
            capture_output=True, text=True,
        )

    def test_archive_roundtrip_and_source_binaries_unchanged(self):
        before = {p.name: p.read_bytes() for p in self.bins.iterdir()}
        self.verified_package()
        for p in self.bins.iterdir():
            self.assertEqual(before[p.name], p.read_bytes())
        with (self.app / "Contents/Info.plist").open("rb") as f:
            plist = plistlib.load(f)
        self.assertEqual(plist["CFBundleIdentifier"], "dev.krill.app")
        self.assertEqual(plist["CFBundleExecutable"], "krill-app")
        self.assertEqual(plist["CFBundleIconFile"], "AppIcon")
        self.assertRegex(plist["KrillUpdatePublicKey"], r"^[0-9a-f]{64}$")
        self.assertTrue(plist["KrillUpdateURL"].endswith("/releases/latest/download/latest.json"))
        self.assertRegex(
            plist["KrillUpdateDesignatedRequirement"],
            r'^identifier "dev\.krill\.app" and certificate root = H"[0-9a-f]{40}"$',
        )
        self.assertTrue((self.app / "Contents/Resources/AppIcon.icns").is_file())
        self.assertTrue((self.app / "Contents/Resources/krill-chime.caf").is_file())
        self.assertIn("NSDocumentsFolderUsageDescription", plist)
        archive, = self.output.glob("*.zip")
        self.assertIn("-adhoc.zip", archive.name)
        arch = subprocess.check_output(
            ["/usr/bin/lipo", "-archs", str(self.bins / "krill-app")], text=True,
        ).strip()
        self.assertTrue(archive.name.endswith(f"-{arch}-adhoc.zip"))
        checksum = pathlib.Path(str(archive) + ".sha256").read_text()
        self.assertEqual(checksum.split()[0], hashlib.sha256(archive.read_bytes()).hexdigest())
        unpacked = self.output / "unzipped"
        subprocess.run(["/usr/bin/ditto", "-x", "-k", str(archive), str(unpacked)], check=True)
        result = subprocess.run(
            [str(HERE / "verify.sh"), str(unpacked / "Krill.app")],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(list(self.output.glob(".krill.*")))

    def test_signed_bundle_preserves_tested_machine_code(self):
        sys.path.insert(0, str(HERE.parent))
        import release_check
        self.verified_package()
        with patch.object(release_check, "verify", return_value=self.bins):
            release_check.verify_bundle(self.output / "receipt.json", "a" * 40, self.output / "unused-key", self.app)
            changed = self.output / "different"
            subprocess.run(["/usr/bin/clang", "-x", "c", "-", "-o", str(changed)],
                           input=b"int main(void) { return 1; }\n", check=True)
            shutil.copyfile(changed, self.app / "Contents/Helpers/krilld")
            with self.assertRaisesRegex(ValueError, "differs from the tested binary"):
                release_check.verify_bundle(self.output / "receipt.json", "a" * 40, self.output / "unused-key", self.app)

    def test_ed25519_manifest_signing_and_forbidden_test_keys(self):
        script = str(HERE / "ed25519_manifest.py")
        key_file = self.output / "release.ed25519.key"
        keygen = subprocess.run(
            [sys.executable, script, "keygen", "--key-file", str(key_file)],
            capture_output=True, text=True, check=True,
        )
        pub_hex = keygen.stdout.strip()
        self.assertRegex(pub_hex, r"^[0-9a-f]{64}$")
        self.assertIn("created a NEW Ed25519 update signing key", keygen.stderr)
        self.assertEqual(key_file.stat().st_mode & 0o777, 0o600)
        pub_file = pathlib.Path(f"{key_file}.pub")
        self.assertTrue(pub_file.is_file())
        self.assertEqual(pub_file.read_text().strip(), pub_hex)

        # Calling keygen again preserves the existing key
        keygen2 = subprocess.run(
            [sys.executable, script, "keygen", "--key-file", str(key_file)],
            capture_output=True, text=True, check=True,
        )
        self.assertEqual(keygen2.stdout.strip(), pub_hex)
        self.assertEqual(keygen2.stderr, "")

        manifest_path = self.output / "latest.json"
        sha = "ab" * 32
        subprocess.run(
            [
                sys.executable, script, "sign",
                "--key-file", str(key_file),
                "--version", "0.2.0",
                "--url", "https://github.com/craftowen/krill/releases/download/v0.2.0/Krill-0.2.0-arm64-adhoc.zip",
                "--sha256", sha,
                "--min-daemon-protocol", "1",
                "--channel", "stable",
                "--rollout-pct", "100",
                "--output", str(manifest_path),
            ],
            capture_output=True, text=True, check=True,
        )
        import json
        manifest = json.loads(manifest_path.read_text())
        self.assertEqual(manifest["version"], "0.2.0")
        self.assertEqual(manifest["sha256"], sha)
        self.assertRegex(manifest["ed25519_sig"], r"^[0-9a-f]{128}$")

        # RFC 8032 §7.1 test seed must be rejected
        bad_env = dict(
            os.environ,
            KRILL_RELEASE_SIGNING_KEY="9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        )
        bad_res = subprocess.run(
            [sys.executable, script, "keygen", "--key-file", str(self.output / "unused.key")],
            env=bad_env, capture_output=True, text=True,
        )
        self.assertNotEqual(bad_res.returncode, 0)
        self.assertIn("RFC 8032", bad_res.stderr)

    def test_rebuilds_preserve_designated_requirement_and_inplace_install_inodes(self):
        self.verified_package()
        req1 = subprocess.run(
            ["/usr/bin/codesign", "-dr", "-", str(self.app)],
            capture_output=True, text=True, check=True,
        )
        combined1 = req1.stdout + req1.stderr
        self.assertIn('designated => identifier "dev.krill.app" and certificate root = H"', combined1)
        self.assertNotIn("cdhash", combined1)
        requirement_expr = combined1.split("designated => ", 1)[1].splitlines()[0].strip()

        installed = self.output / "Installed.app"
        shutil.copytree(self.app, installed, symlinks=True)
        app_ino_before = installed.stat().st_ino
        macos_ino_before = (installed / "Contents/MacOS").stat().st_ino
        bin_ino_before = (installed / "Contents/MacOS/krill-app").stat().st_ino
        # Execute before install to populate XNU UBC cs_blob cache
        run_before = subprocess.run([str(installed / "Contents/MacOS/krill-app")], capture_output=True)
        self.assertEqual(run_before.returncode, 0)

        bins2 = self.output / "bins2"
        bins2.mkdir()
        subprocess.run(
            ["/usr/bin/clang", "-x", "c", "-", "-o", str(bins2 / "krill-app")],
            input=b"int main(void) { return 42; }\n",
            check=True,
        )
        for name in ("krill", "krill-holder", "krilld", "krill-link"):
            shutil.copy2(bins2 / "krill-app", bins2 / name)

        res2 = self.package(bins=bins2, extra_args=["--install", installed])
        self.assertEqual(res2.returncode, 0, res2.stdout + res2.stderr)
        self.assertEqual(installed.stat().st_ino, app_ino_before)
        self.assertEqual((installed / "Contents/MacOS").stat().st_ino, macos_ino_before)
        self.assertNotEqual((installed / "Contents/MacOS/krill-app").stat().st_ino, bin_ino_before)
        run_after = subprocess.run([str(installed / "Contents/MacOS/krill-app")], capture_output=True)
        self.assertEqual(run_after.returncode, 42)
        check_req = subprocess.run(
            ["/usr/bin/codesign", "--verify", "--strict", f"-R={requirement_expr}", str(installed)],
            capture_output=True, text=True,
        )
        self.assertEqual(check_req.returncode, 0, check_req.stdout + check_req.stderr)

    def test_tampered_helper_is_rejected(self):
        self.verified_package()
        helper = self.app / "Contents/Helpers/krilld"
        data = bytearray(helper.read_bytes())
        sections = subprocess.check_output(["/usr/bin/otool", "-l", str(helper)], text=True)
        text = re.search(r"sectname __text\s+segname __TEXT\s+addr \S+\s+size (0x[0-9a-fA-F]+)\s+offset (\d+)", sections)
        self.assertIsNotNone(text, "fixture must contain a signed machine-code section")
        size, offset = int(text[1], 16), int(text[2])
        self.assertGreater(size, 0)
        self.assertLess(offset + size, len(data))
        # A file midpoint can land in signing metadata on Intel; change actual signed code.
        data[offset + size // 2] ^= 1
        helper.write_bytes(data)
        self.assertNotEqual(self.verify().returncode, 0)

    def test_valid_signature_with_wrong_helper_identifier_is_rejected(self):
        self.verified_package()
        subprocess.run(
            ["/usr/bin/codesign", "--force", "--sign", "-", "--identifier", "wrong.helper",
             str(self.app / "Contents/Helpers/krill-holder")],
            check=True, capture_output=True,
        )
        self.assertNotEqual(self.verify().returncode, 0)

    def test_resigned_bundle_with_wrong_plist_identifier_is_rejected(self):
        self.verified_package()
        subprocess.run(
            ["/usr/bin/plutil", "-replace", "CFBundleIdentifier", "-string", "wrong.app",
             str(self.app / "Contents/Info.plist")],
            check=True, capture_output=True,
        )
        subprocess.run(
            ["/usr/bin/codesign", "--force", "--sign", "-", "--identifier", "dev.krill.app",
             str(self.app)],
            check=True, capture_output=True,
        )
        self.assertNotEqual(self.verify().returncode, 0)

    def test_missing_input_does_not_publish_partial_bundle(self):
        result = self.package(self.output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing executable", result.stderr)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_existing_output_is_preserved(self):
        self.verified_package()
        original = (self.app / "Contents/Info.plist").read_bytes()
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("output already exists", result.stderr)
        self.assertEqual(original, (self.app / "Contents/Info.plist").read_bytes())
        self.assertEqual(self.verify().returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)

