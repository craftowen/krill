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


if __name__ == "__main__":
    unittest.main()
