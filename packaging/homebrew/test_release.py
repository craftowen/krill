#!/usr/bin/env python3
"""Offline regression tests. Native release smoke uses a tiny signed fixture, not the UI."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("generate", HERE / "generate.py")
generate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generate)


def run(args, **kwargs):
    result = subprocess.run(list(map(str, args)), text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=90, **kwargs)
    if result.returncode:
        raise AssertionError(f"{args}: exit {result.returncode}\n{result.stdout}")
    return result.stdout


class MetadataTests(unittest.TestCase):
    def test_urls_reject_credentials_and_nonlocal_files(self):
        for url in ["https://token@example.com/a", "https://example.com/a?token=secret",
                    "http://example.com/a", "file://host/a", "https:///a", "https://a/b#secret"]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                generate.artifact_url(url)
        self.assertEqual(generate.artifact_url("file:///tmp/a%20b.dmg"), "file:///tmp/a%20b.dmg")

    def test_ruby_interpolation_is_literal(self):
        value = 'https://example.invalid/#{raise "injected"}/#@x/#$x/한글'
        literal = generate.ruby_string(value)
        result = run(["ruby", "-rjson", "-e", f"puts JSON.generate({literal})"])
        self.assertEqual(json.loads(result), value)

    def test_generation_hashes_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory(prefix="mk072-meta-") as tmp:
            root = Path(tmp)
            source, dmg = root / "source.tar.gz", root / "release.dmg"
            source.write_bytes(b"source"); dmg.write_bytes(b"dmg")
            args = [sys.executable, HERE / "generate.py", "--version", "0.1.0",
                    "--source", source, "--source-url", source.as_uri(), "--dmg", dmg,
                    "--dmg-url", dmg.as_uri(), "--homepage", "https://example.invalid/krill",
                    "--output", root / "tap"]
            run(args)
            self.assertIn(hashlib.sha256(b"source").hexdigest(), (root / "tap/SHA256SUMS").read_text())
            formula = (root / "tap/Formula/krill.rb").read_text()
            self.assertIn('"--locked"', formula)
            self.assertIn('"packaging/bundle.sh"', formula)
            for file in (root / "tap").rglob("*.rb"):
                run(["ruby", "-c", file])
            result = subprocess.run(list(map(str, args)), capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)


@unittest.skipUnless(sys.platform == "darwin", "native DMG smoke requires macOS")
class ReleaseTests(unittest.TestCase):
    def test_local_dmg_install_path_conflict_and_zap(self):
        with tempfile.TemporaryDirectory(prefix="mk072-smoke-", dir="/tmp") as tmp:
            root = Path(tmp).resolve()
            repo = root / "repo"
            shutil.copytree(HERE.parent, repo / "packaging", ignore=shutil.ignore_patterns("dist", "__pycache__"))
            (repo / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
            (repo / "packaging/bundle.sh").write_text('#!/bin/bash\n# MK-061 test stub: fixture bundle is prepared by this test.\ntest -d packaging/dist/Krill.app\n')
            # Test fixture respects MK-061's output contract; it never runs an agent or GUI.
            app = repo / "packaging/dist/Krill.app"
            macos, helpers = app / "Contents/MacOS", app / "Contents/Helpers"
            macos.mkdir(parents=True); helpers.mkdir()
            fixture = root / "fixture.c"
            fixture.write_text('#include <stdio.h>\nint main(void) { puts("usage: krill fixture"); return 2; }\n')
            run(["cc", fixture, "-o", root / "fixture"])
            for relative, identifier in [("MacOS/krill-app", "dev.krill.app"),
                                         ("MacOS/krill", "dev.krill.app.cli"),
                                         ("Helpers/krill-holder", "dev.krill.app.holder"),
                                         ("Helpers/krilld", "dev.krill.app.krilld")]:
                binary = app / "Contents" / relative
                shutil.copy2(root / "fixture", binary)
                run(["codesign", "--force", "--sign", "-", "--identifier", identifier, binary])
            with (app / "Contents/Info.plist").open("wb") as stream:
                plistlib.dump(dict(CFBundleIdentifier="dev.krill.app", CFBundleName="Krill",
                                   CFBundleExecutable="krill-app", CFBundlePackageType="APPL",
                                   CFBundleShortVersionString="0.1.0", CFBundleVersion="1"), stream)
            run(["codesign", "--force", "--sign", "-", app])
            run(["git", "init", "-q", repo])
            run(["git", "-C", repo, "add", "."])
            run(["git", "-C", repo, "-c", "user.name=Release test", "-c", "user.email=test@invalid",
                 "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"])
            release = ["bash", "packaging/release.sh", "0.1.0", "https://example.invalid/krill"]
            run(release, cwd=repo)
            dmg = repo / "packaging/dist/Krill-0.1.0.dmg"
            run(["hdiutil", "verify", dmg])
            self.assertIn("SHA256SUMS", str(next((repo / "packaging/dist/homebrew-krill").glob("SHA256SUMS"))))
            # The source snapshot is exactly the committed tree, including packaging.
            files = run(["tar", "-tzf", repo / "packaging/dist/Krill-0.1.0-source.tar.gz"])
            self.assertIn("Krill-0.1.0/packaging/release.sh", files)
            result = subprocess.run(release, cwd=repo, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("already exist", result.stderr)
            dirty = repo / "uncommitted.txt"; dirty.write_text("dirty")
            result = subprocess.run(release, cwd=repo, capture_output=True, text=True)
            self.assertIn("clean worktree", result.stderr)
            dirty.unlink()
            wrong_version = subprocess.run(["bash", "packaging/release.sh", "0.1.1", "https://example.invalid"],
                                           cwd=repo, capture_output=True, text=True)
            self.assertIn("bundle version does not match", wrong_version.stderr)
            brew = shutil.which("brew")
            self.assertIsNotNone(brew, "Homebrew is required for cask smoke")
            prefix = root / "brew"
            (prefix / "bin").mkdir(parents=True)
            # Copy the launcher: a symlink points Homebrew at its real repository
            # and allows automatic tap work outside the isolated prefix.
            shutil.copy2(brew, prefix / "bin/brew")
            (prefix / "Library").mkdir()
            (prefix / "Library/Homebrew").symlink_to(Path(brew).resolve().parents[1] / "Library/Homebrew")
            run(["git", "init", "-q", prefix])
            # Homebrew's uninstall resolver expects core even for an absolute cask.
            # An empty local core fixture and disabled git transports keep this offline.
            core = prefix / "Library/Taps/homebrew/homebrew-core"
            (core / "Formula").mkdir(parents=True)
            run(["git", "init", "-q", core])
            home = root / "home"; home.mkdir()
            (root / "temp").mkdir()
            env = dict(os.environ, HOME=str(home), HOMEBREW_TEMP=str(root / "temp"),
                       HOMEBREW_CACHE=str(root / "cache"), HOMEBREW_LOGS=str(root / "logs"),
                       HOMEBREW_NO_AUTO_UPDATE="1", HOMEBREW_NO_ANALYTICS="1",
                       HOMEBREW_NO_INSTALL_FROM_API="1", HOMEBREW_NO_INSTALL_CLEANUP="1",
                       HOMEBREW_DEVELOPER="1", XDG_CONFIG_HOME=str(home / ".config"),
                       GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="protocol.allow", GIT_CONFIG_VALUE_0="never",
                       PATH=f"{prefix / 'bin'}:{os.environ['PATH']}")
            brew = prefix / "bin/brew"
            self.assertEqual(Path(run([brew, "--prefix"], env=env).strip()), prefix)
            self.assertEqual(Path(run([brew, "--repository"], env=env).strip()), prefix)
            cask = repo / "packaging/dist/homebrew-krill/Casks/krill.rb"
            formula = repo / "packaging/dist/homebrew-krill/Formula/krill.rb"
            info = json.loads(run([brew, "info", "--formula", "--json=v2", formula], env=env))
            self.assertEqual(info["formulae"][0]["name"], "krill")
            self.assertIn("rust", info["formulae"][0]["build_dependencies"])
            install = [brew, "install", "--cask", cask, f"--appdir={root / 'apps'}"]
            # A foreign krill is never replaced, including a dangling symlink.
            cli = prefix / "bin/krill"
            cli.symlink_to(root / "foreign-krill")
            collision = subprocess.run(list(map(str, install)), env=env, capture_output=True, text=True, timeout=90)
            self.assertNotEqual(collision.returncode, 0)
            self.assertIn("krill already exists", collision.stdout + collision.stderr)
            self.assertEqual(cli.readlink(), root / "foreign-krill")
            cli.unlink()
            run(install, env=env)
            self.assertEqual(cli.resolve(), root / "apps/Krill.app/Contents/MacOS/krill")
            # Quarantined ad-hoc executables can be blocked by Gatekeeper. Verify
            # PATH discovery without executing downloaded code or bypassing policy.
            result = subprocess.run(["/bin/sh", "-c", "command -v krill"], env=env, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0)
            self.assertIn(str(cli), result.stdout)
            # Run the real zap resolver against nonempty isolated data. Only the
            # final Trash move is redirected, since Foundation ignores HOME for Trash.
            # Current bundle id paths plus pre-MK-170 legacy ones (compatibility links after migration).
            managed = [home / f"Library/{base}" for bundle, logs in [("dev.krill.app", "Krill"), ("dev.minikrill.app", "Minikrill")]
                       for base in [f"Application Support/{bundle}", f"Caches/{bundle}", f"Logs/{logs}",
                                    f"Preferences/{bundle}.plist", f"Saved Application State/{bundle}.savedState"]]
            for path in managed:
                path.parent.mkdir(parents=True, exist_ok=True)
                if path.suffix == ".plist":
                    path.write_text("owned fixture")
                else:
                    (path / "nested").mkdir(parents=True)
                    (path / "nested/data").write_text("owned fixture")
            sentinel = home / "Library/Application Support/unrelated"
            sentinel.write_text("keep")
            resolver = root / "zap.rb"
            resolver.write_text('''require "cask/cask_loader"
require "cask/utils/trash"
require "fileutils"
module IsolatedTrash
  def trash(*paths, command: nil)
    destination = Pathname(ENV.fetch("HOME"))/"test-trash"
    destination.mkpath
    paths.each_with_index do |path, index|
      raise "zap escaped HOME" unless path.to_s.start_with?("#{ENV.fetch('HOME')}/")
      FileUtils.mv(path, destination/index.to_s)
    end
    [paths.map(&:to_s), []]
  end
end
Cask::Utils::Trash.singleton_class.prepend(IsolatedTrash)
cask = Cask::CaskLoader.load(ARGV.fetch(0))
artifact = cask.artifacts.find { |item| item.is_a?(Cask::Artifact::Zap) }
raise "missing zap" unless artifact
artifact.zap_phase(command: SystemCommand)
''')
            run([brew, "ruby", resolver, cask], env=env)
            for path in managed:
                self.assertFalse(path.exists(), str(path))
            self.assertEqual(sentinel.read_text(), "keep")
            self.assertEqual(len(list((home / "test-trash").iterdir())), len(managed))
            # Empty zap paths exercise Homebrew dispatch without touching the user's Trash.
            run([brew, "uninstall", "--cask", "--zap", cask], env=env)
            self.assertFalse(cli.exists()); self.assertFalse(cli.is_symlink())
            self.assertFalse((root / "apps/Krill.app").exists())
            self.assertFalse((prefix / "Caskroom/krill").exists())
            # Refuse a broken signature before producing a new DMG.
            with (helpers / "krilld").open("ab") as stream:
                stream.write(b"tamper")
            broken = subprocess.run(release, cwd=repo, capture_output=True, text=True)
            self.assertNotEqual(broken.returncode, 0)
            self.assertNotIn("already exist", broken.stderr)
            print("DMG + brew install --cask + PATH + foreign krill preservation + uninstall --zap: PASS")


if __name__ == "__main__":
    unittest.main(verbosity=2)
