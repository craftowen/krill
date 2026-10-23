#!/usr/bin/env python3
"""Offline release-note selection and publication-branch tests; gh is always a stub."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import release_notes


ROOT = Path(__file__).resolve().parent.parent


def workflow_step(name):
    lines = (ROOT / ".github/workflows/release.yml").read_text().splitlines()
    start = lines.index(f"      - name: {name}")
    run = next(i for i in range(start, len(lines)) if lines[i] == "        run: |")
    body = []
    for line in lines[run + 1:]:
        if line and not line.startswith("          "):
            break
        body.append(line[10:])
    return "\n".join(body) + "\n"


class NotesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="krill-notes-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.notes = self.root / "docs/releases"
        self.notes.mkdir(parents=True)

    def render(self):
        return release_notes.render(self.root, "v9.8.7", "example/krill")

    def test_fallback_is_truthful_and_does_not_borrow_another_tag(self):
        (self.notes / "v9.8.6.md").write_text("A change from a different release.")
        text = self.render()
        self.assertIn("No release-specific highlights were supplied", text)
        self.assertNotIn("A change from a different release", text)
        for boundary in ("Apple Silicon", "Windows x64", "not notarized", "Authenticode",
                         "not yet recorded", "Rebooting", "Linux", "actual download availability"):
            self.assertIn(boundary, text)
        self.assertIn("https://github.com/example/krill/releases/tag/v9.8.7#whats-changed", text)
        self.assertIn("https://github.com/example/krill/blob/v9.8.7/README.md", text)

    def test_exact_tag_notes_keep_their_content_below_the_common_english_header(self):
        detail = "## Verified change\n\nA maintainer-supplied description.\n"
        (self.notes / "v9.8.7.md").write_text(detail)
        text = self.render()
        self.assertIn(detail.strip(), text)
        self.assertNotIn("No release-specific highlights", text)
        self.assertLess(text.index("Platform scope"), text.index("Verified change"))

    def test_empty_and_symlink_notes_fail_instead_of_silently_falling_back(self):
        path = self.notes / "v9.8.7.md"
        path.write_text(" \n")
        with self.assertRaisesRegex(ValueError, "empty"):
            self.render()
        path.unlink()
        secret = self.root / "private.txt"
        secret.write_text("private input")
        path.symlink_to(secret)
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.render()

    def test_invalid_identifiers_fail_before_reading_release_notes(self):
        for tag, repo in (("../../private", "o/r"), ("v1.2.3\ninjected", "o/r"),
                          ("v1.2.3", "o/r/extra"), ("v1.2.3", "../r")):
            with self.subTest(tag=tag, repo=repo), self.assertRaises(ValueError):
                release_notes.render(self.root, tag, repo)

    def test_render_is_read_only_and_never_launches_external_commands(self):
        (self.notes / "v9.8.7.md").write_text("A supplied summary.\n")
        before = {str(p): p.read_bytes() for p in self.root.rglob("*") if p.is_file()}
        with patch("subprocess.run", side_effect=AssertionError("no external command")), \
                patch("socket.create_connection", side_effect=AssertionError("no network")):
            self.render()
        after = {str(p): p.read_bytes() for p in self.root.rglob("*") if p.is_file()}
        self.assertEqual(before, after)

    def test_cli_prints_notes_without_creating_an_output_file(self):
        result = subprocess.run([sys.executable, str(ROOT / "packaging/release_notes.py"),
                                 "--tag", "v9.8.7", "--repo", "example/krill", "--root", str(self.root)],
                                check=True, capture_output=True, text=True)
        self.assertTrue(result.stdout.startswith("# Krill v9.8.7\n"))
        self.assertEqual(result.stderr, "")
        self.assertEqual(list(self.notes.iterdir()), [])


class PublicationBranchTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="krill-notes-publish-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "packaging").mkdir()
        shutil.copy2(ROOT / "packaging/release_notes.py", self.root / "packaging/release_notes.py")
        (self.root / "docs/releases").mkdir(parents=True)
        (self.root / "docs/releases/v9.8.7.md").write_text("A reviewed tag-specific summary.\n")
        (self.root / "bin").mkdir()
        (self.root / "dist").mkdir()
        (self.root / "tmp").mkdir()
        (self.root / "packaging/dist").mkdir()
        for arch in ("x86_64", "aarch64"):
            (self.root / f"dist/source-commit-{arch}.txt").write_text("a" * 40 + "\n")
        fake = self.root / "bin/gh"
        fake.write_text('''#!/usr/bin/python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["GH_CALLS"], "a") as f: f.write(json.dumps(args) + "\\n")
if args[:2] == ["release", "view"]:
    if os.environ.get("EXISTING") == "1":
        print("Krill-9.8.7-windows-x64.zip")
        for arch in ("x86_64", "aarch64"):
            print(f"krill-9.8.7-linux-{arch}.tar.gz")
            print(f"krill-9.8.7-linux-{arch}.tar.gz.sha256")
        sys.exit(0)
    print("release not found", file=sys.stderr)
    sys.exit(1)
if args[:2] == ["release", "create"]:
    assert "--generate-notes" in args
    notes = pathlib.Path(args[args.index("--notes-file") + 1]).read_text()
    pathlib.Path(os.environ["CREATED_NOTES"]).write_text(notes)
    sys.exit(0)
raise SystemExit("Unexpected gh mutation: " + repr(args))
''')
        fake.chmod(0o755)
        self.env = {
            "PATH": str(self.root / "bin") + ":/usr/bin:/bin:/usr/sbin:/sbin",
            "TAG": "v9.8.7", "VERSION": "9.8.7", "GITHUB_REPOSITORY": "example/krill",
            "GH_REPO": "example/krill", "GITHUB_SHA": "a" * 40,
            "RUNNER_TEMP": str(self.root / "tmp"), "GH_CALLS": str(self.root / "calls.jsonl"),
            "CREATED_NOTES": str(self.root / "created-notes.md"),
        }

    def execute(self, script, existing=False):
        (self.root / "calls.jsonl").unlink(missing_ok=True)
        (self.root / "created-notes.md").unlink(missing_ok=True)
        result = subprocess.run(["/bin/bash", "-c", script], cwd=self.root,
                                env={**self.env, "EXISTING": "1" if existing else "0"},
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]

    def test_all_workflow_create_branches_use_the_header_and_generated_changelog(self):
        for name in ("Publish GitHub Release", "Publish Windows assets"):
            with self.subTest(step=name):
                calls = self.execute(workflow_step(name))
                create = next(call for call in calls if call[:2] == ["release", "create"])
                self.assertEqual(create[create.index("--target") + 1], "a" * 40)
                text = (self.root / "created-notes.md").read_text()
                self.assertIn("A reviewed tag-specific summary.", text)
                self.assertIn("Platform scope and known limits", text)

    def test_existing_complete_releases_do_not_regenerate_notes_or_mutate(self):
        for name in ("Publish GitHub Release", "Publish Windows assets"):
            with self.subTest(step=name):
                calls = self.execute(workflow_step(name), existing=True)
                self.assertTrue(all(call[:2] == ["release", "view"] for call in calls))
                self.assertFalse((self.root / "created-notes.md").exists())

    def test_invalid_exact_tag_notes_stop_before_any_release_creation(self):
        (self.root / "docs/releases/v9.8.7.md").write_text("\n")
        for name in ("Publish GitHub Release", "Publish Windows assets"):
            with self.subTest(step=name):
                calls_file = self.root / "calls.jsonl"
                calls_file.unlink(missing_ok=True)
                result = subprocess.run(["/bin/bash", "-c", workflow_step(name)], cwd=self.root,
                                        env=self.env, capture_output=True, text=True, timeout=20)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("release notes are empty", result.stderr)
                calls = [json.loads(line) for line in calls_file.read_text().splitlines()]
                self.assertTrue(all(call[:2] == ["release", "view"] for call in calls))
                self.assertFalse((self.root / "created-notes.md").exists())

    def test_local_create_command_preserves_verify_tag_and_uses_rendered_file(self):
        script = (ROOT / "packaging/publish-github.sh").read_text()
        render = script[script.index('RELEASE_NOTES="$OUT_DIR/release-notes.md"'):]
        render = render.split("\n\nUPDATE_PUBKEY=", 1)[0]
        create = script[script.index('gh release create "$TAG"'):].split("\n\n#", 1)[0]
        commands = 'set -euo pipefail\nOUT_DIR=packaging/dist\nREPO=example/krill\nASSETS=(offline-artifact.zip)\n' + render + "\n" + create
        calls = self.execute(commands)
        self.assertIn("--verify-tag", calls[0])
        self.assertIn("A reviewed tag-specific summary.", (self.root / "created-notes.md").read_text())


if __name__ == "__main__":
    unittest.main()
