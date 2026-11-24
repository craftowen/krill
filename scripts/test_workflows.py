#!/usr/bin/env python3
"""GitHub workflow supply-chain and trigger contracts that actionlint does not check."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = sorted((ROOT / ".github/workflows").glob("*.yml"))


class WorkflowTests(unittest.TestCase):
    def test_external_actions_are_pinned_to_a_commit_sha(self):
        # .github/dependabot.yml policy: tags can be moved, commit SHAs cannot.
        self.assertTrue(WORKFLOWS)
        for workflow in WORKFLOWS:
            for number, line in enumerate(workflow.read_text().splitlines(), 1):
                match = re.match(r"\s*(?:-\s*)?uses:\s*(\S+)", line)
                if not match or match[1].startswith("./"):
                    continue
                with self.subTest(workflow=workflow.name, line=number):
                    self.assertRegex(match[1], r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$")


    def test_local_ci_and_nightly_build_tests_with_the_same_profile(self):
        # Timing-budget tests passed locally at opt-level 0 but ran at 2 in CI and nightly (F-14).
        ci = (ROOT / ".github/workflows/ci-macos.yml").read_text()
        check = (ROOT / "scripts/check.sh").read_text()
        nightly = (ROOT / "packaging/nightly.py").read_text()
        for key in ("CARGO_PROFILE_TEST_OPT_LEVEL", "CARGO_PROFILE_TEST_DEBUG"):
            with self.subTest(key=key):
                value = re.search(r"\b" + key + r":\s*(\S+)", ci)[1]
                self.assertRegex(check, r"(?m)^export .*\b" + key + r"=\$\{" + key + r":-" + re.escape(value) + r"\}")
                self.assertIn(key + '="' + value + '"', nightly)

    def test_ci_cache_scope_reuse_and_release_audit_contracts(self):
        macos = (ROOT / ".github/workflows/ci-macos.yml").read_text()
        windows = (ROOT / ".github/workflows/ci-windows.yml").read_text()
        release = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn("branches: [dev, main, 'release/**']", macos)
        self.assertIn("smoke & cache warm (macos-15)", macos)
        self.assertIn("python3 packaging/ci_cache_prune.py", macos)

        self.assertIn("branches: [dev, main, 'release/**']", windows)
        self.assertIn("name: reuse-windows-ci", windows)
        self.assertIn("ci_evidence.py reuse --workflow ci-windows.yml", windows)
        self.assertEqual(len(re.findall(r"uses:\s*Swatinem/rust-cache@[0-9a-f]{40}", windows)), 3)
        self.assertIn("CARGO_PROFILE_RELEASE_LTO=off", windows)
        self.assertIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16", windows)
        self.assertIn("python3 packaging/ci_cache_prune.py", windows)

        build_step = release.split("- name: Build release binaries", 1)[1].split("- name: Verify Windows PE", 1)[0]
        self.assertNotIn("CARGO_PROFILE_RELEASE_LTO", build_step)
        self.assertNotIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", build_step)
        audit_step = release.split("- name: Verify native Windows audit regressions", 1)[1].split("- name: Fetch pinned ConPTY", 1)[0]
        self.assertIn("CARGO_PROFILE_RELEASE_LTO: 'off'", audit_step)
        self.assertIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS: '16'", audit_step)
        self.assertIn("'--target-dir', $auditTarget", audit_step)


if __name__ == "__main__":
    unittest.main()
